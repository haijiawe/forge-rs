//! The `subagent` tool and its depth-limited sibling.
//!
//! Both tools are always advertised: at the depth limit the model still sees
//! `subagent`, but its call always returns `depth_exceeded` with the reason in
//! `Meta`. Unregistering it would leave the model guessing why it vanished.

use std::sync::Arc;

use forge_agent::Cancel;
use forge_core::Error;
use forge_llm::ToolSpec;
use forge_tool::{Call, Result as ToolResult, Tool, CODE_CANCELLED, ERR_BAD_ARGS};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::child::ChildRun;
use crate::options::Canceller;
use crate::runner::Runner;

/// Wire name of the delegation tool.
pub const TOOL_NAME: &str = "subagent";
/// Code returned when the child fails, whether in setup or in its loop.
pub const CODE_FAILED: &str = "subagent_failed";
/// Code returned by the depth-limited tool on every call.
pub const CODE_DEPTH_EXCEEDED: &str = "depth_exceeded";

const DESCRIPTION: &str = "Delegate a self-contained, small task to a fresh child agent that does not see this conversation. Use only for independent research you can specify from scratch; do NOT delegate tasks that need this conversation's context, tasks you can do directly, or tasks requiring approval (children fail closed and cannot ask).";

/// The tool's JSON Schema. A fresh tree on every call so a caller cannot
/// mutate a cached schema and poison later requests.
pub fn tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "Fully specified task; the child has no other context."
            },
            "system": {
                "type": "string",
                "description": "Optional system prompt for this child only."
            },
            "model": {
                "type": "string",
                "description": "Optional model override for this child only."
            }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

/// The advertised tool spec.
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_NAME.to_string(),
        description: DESCRIPTION.to_string(),
        schema: tool_schema(),
    }
}

/// Strict argument shape. `deny_unknown_fields` is the Rust equivalent of Go's
/// `DisallowUnknownFields`: a typo like `modle` is rejected before any log is
/// written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegateArgs {
    pub prompt: String,
    #[serde(default)]
    pub system: String,
    #[serde(default)]
    pub model: String,
}

impl DelegateArgs {
    /// Decode from a tool call, mapping any parse failure to `bad_args`.
    pub fn decode(call: &Call) -> std::result::Result<Self, ToolResult> {
        let value = if call.args.is_null() {
            json!({})
        } else {
            call.args.clone()
        };
        serde_json::from_value(value.clone()).map_err(|e| {
            ToolResult::error(ERR_BAD_ARGS, format!("subagent: invalid arguments: {e}"))
        })
    }
}

/// A runner's normal delegation tool.
pub(crate) struct DelegateTool {
    pub(crate) runner: Arc<Runner>,
}

impl Tool for DelegateTool {
    fn spec(&self) -> ToolSpec {
        spec()
    }

    fn call(&self, call: Call) -> ToolResult {
        let args = match DelegateArgs::decode(&call) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if args.prompt.trim().is_empty() {
            return ToolResult::error(ERR_BAD_ARGS, "subagent: prompt must not be empty");
        }

        let canceller = &self.runner.options().canceller;
        if canceller.is_cancelled() {
            return cancelled_result("");
        }

        let cancel = Cancel::default();
        canceller.register(call.id.clone(), cancel.clone());
        let unregister = UnregisterGuard {
            canceller,
            call_id: call.id.clone(),
        };

        if !self.runner.acquire(&cancel) {
            return cancelled_result("");
        }
        let release = ReleaseGuard {
            sem: self.runner.semaphore(),
        };

        if cancel.is_cancelled() || canceller.is_cancelled() {
            return cancelled_result("");
        }

        let ctx = crate::runner::CallContext {
            cancel: cancel.clone(),
            parent_session_id: String::new(),
            workspace: if call.workspace.is_empty() {
                self.runner.options().workspace.clone()
            } else {
                call.workspace.clone()
            },
        };

        let run = self
            .runner
            .run(&ctx, &args.prompt, &args.system, &args.model);
        let out = match run {
            Ok(out) => out,
            Err(e) => {
                if forge_agent::is_cancelled(&e) {
                    return cancelled_result("");
                }
                return result(ChildRun::default(), Some(e));
            }
        };

        // Both guards drop here: release the serial slot, then unregister.
        drop(release);
        drop(unregister);
        result(out, None)
    }
}

/// A tool advertised at the depth limit. It never delegates; it explains why.
pub(crate) struct DepthExceeded {
    pub(crate) depth: usize,
    pub(crate) max_depth: usize,
}

impl Tool for DepthExceeded {
    fn spec(&self) -> ToolSpec {
        spec()
    }

    fn call(&self, _call: Call) -> ToolResult {
        ToolResult::error(
            CODE_DEPTH_EXCEEDED,
            format!(
                "subagent: delegation depth limit reached (this session is at depth {}, the limit is {}). Do the task here instead of delegating it.",
                self.depth, self.max_depth
            ),
        )
        .with_meta("depth", json!(self.depth))
        .with_meta("maxDepth", json!(self.max_depth))
    }
}

/// Shape the child's outcome into a tool result. Setup errors and loop errors
/// are both `Code="subagent_failed"`; cancellation has its own code; empty
/// text is a success that says what happened, not a failure.
pub(crate) fn result(out: ChildRun, setup_error: Option<Error>) -> ToolResult {
    let mut meta = serde_json::Map::new();
    meta.insert("steps".into(), json!(out.steps));
    meta.insert("turns".into(), json!(out.turns));
    if !out.id.is_empty() {
        meta.insert("sessionId".into(), json!(out.id));
    }
    if !out.model.is_empty() {
        meta.insert("model".into(), json!(out.model));
    }

    let mut r = if let Some(e) = setup_error {
        ToolResult::error(
            CODE_FAILED,
            format!("subagent setup failed: {}", e.message()),
        )
    } else if out.cancelled {
        ToolResult::error(
            CODE_CANCELLED,
            format!(
                "subagent: cancelled (child {} wrote turn/end cancelled and session/end)",
                out.id
            ),
        )
    } else if let Some(e) = &out.error {
        ToolResult::error(
            CODE_FAILED,
            format!(
                "subagent failed: {} (session {}, {} steps)",
                e.message(),
                out.id,
                out.steps
            ),
        )
    } else if out.text.trim().is_empty() {
        ToolResult::ok(format!(
            "the child finished without any text; its session is {}",
            out.id
        ))
    } else {
        ToolResult::ok(out.text.clone())
    };
    r.meta = meta;
    r
}

/// A cancellation result with no session attached (cancelled before start).
pub(crate) fn cancelled_result(_reason: &str) -> ToolResult {
    ToolResult::error(
        CODE_CANCELLED,
        "subagent: cancelled before the child started",
    )
}

/// Remove a per-call cancel token on every exit path.
struct UnregisterGuard<'a> {
    canceller: &'a Canceller,
    call_id: String,
}

impl Drop for UnregisterGuard<'_> {
    fn drop(&mut self) {
        self.canceller.unregister(&self.call_id);
    }
}

/// Release the serial slot on every exit path, including early returns.
struct ReleaseGuard<'a> {
    sem: &'a crate::runner::Semaphore,
}

impl Drop for ReleaseGuard<'_> {
    fn drop(&mut self) {
        self.sem.release();
    }
}
