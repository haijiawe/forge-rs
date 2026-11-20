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
/// Code returned when a schema was required but the child finished without
/// calling `structured_output`. This is a failure, never a success.
pub const CODE_BAD_OUTPUT: &str = "bad_output";

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
            },
            "schema": {
                "type": "object",
                "description": "Optional JSON schema (subset: type/required/properties/items). When present the child must finish by calling structured_output with arguments that satisfy it; the result comes back structured."
            },
            "continuable": {
                "type": "boolean",
                "description": "Create the child continuable: it stays resumable by its session id after it settles, so you can send it more work later with a tool that resumes it. Defaults to false (one-shot)."
            },
            "label": {
                "type": "string",
                "description": "Optional short label recorded on a continuable child's descriptor."
            },
            "resume": {
                "type": "string",
                "description": "Resume this continuable child (its session id) with `prompt` instead of starting a new one. The child must have been created with continuable:true and not have ended; its schema is fixed from creation, so `schema` may not be set here."
            },
            "background": {
                "type": "boolean",
                "description": "Start the child and return immediately instead of waiting for its answer. The answer arrives later as a message in a later turn, so you can keep working in the meantime. Cannot be combined with `schema`."
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
    /// Optional per-call result schema; overrides `Options::structured_schema`.
    #[serde(default)]
    pub schema: Option<Value>,
    /// When true the child is created continuable: its log is left open (no
    /// `session/end`) so a later `Runner::resume` can append another turn.
    #[serde(default)]
    pub continuable: bool,
    /// Optional label stored on a continuable child's descriptor.
    #[serde(default)]
    pub label: String,
    /// When set, resume this existing continuable child (its session id) with
    /// `prompt` instead of creating a new child.
    #[serde(default)]
    pub resume: String,
    /// When true, start the child and return at once. Its answer arrives later
    /// as a `next-turn` message on this session's log, so the caller does not
    /// have to wait for it (and does not lose it if it stops paying attention).
    #[serde(default)]
    pub background: bool,
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

        // A resume continues an existing continuable child's log. It shares the
        // cancellation and pool paths with a fresh delegation, but it cannot
        // change the schema (that was fixed when the child was created) and it
        // writes no new catalog row (the child already has one).
        if !args.resume.trim().is_empty() {
            if args.schema.is_some() {
                return ToolResult::error(
                    ERR_BAD_ARGS,
                    "subagent: resume cannot set schema; the schema is fixed when the child is created",
                );
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
            let run =
                self.runner
                    .resume(args.resume.trim(), &args.prompt, &args.system, &args.model);
            drop(release);
            drop(unregister);
            return match run {
                Ok(out) => result(out, None, false),
                Err(e) => {
                    if forge_agent::is_cancelled(&e) {
                        cancelled_result("")
                    } else {
                        result(ChildRun::default(), Some(e), false)
                    }
                }
            };
        }

        // A per-call schema overrides whatever the runner was built with. The
        // effective schema drives both the child's terminal tool and how this
        // result is shaped (a missing submission becomes `bad_output`).
        let schema = args
            .schema
            .clone()
            .or_else(|| self.runner.options().structured_schema.clone());

        let canceller = &self.runner.options().canceller;
        if canceller.is_cancelled() {
            return cancelled_result("");
        }

        let ctx = crate::runner::CallContext {
            cancel: Cancel::default(),
            parent_session_id: String::new(),
            workspace: if call.workspace.is_empty() {
                self.runner.options().workspace.clone()
            } else {
                call.workspace.clone()
            },
        };

        // A background delegation returns before the child has said anything.
        // Its result arrives later as a `next-turn` message on the parent log,
        // which is the only place it can arrive: this call is long gone by then.
        if args.background {
            if schema.is_some() {
                return ToolResult::error(
                    ERR_BAD_ARGS,
                    "subagent: a background child cannot report a schema result; its answer arrives as a message",
                );
            }
            let label = if args.label.trim().is_empty() {
                None
            } else {
                Some(args.label.clone())
            };
            return match self.runner.spawn(ctx, &args.prompt, label) {
                Ok(()) => ToolResult::ok(
                    "subagent: started in the background; its answer will arrive as a message in a later turn.",
                )
                .with_meta("background", json!(true)),
                Err(e) => result(ChildRun::default(), Some(e), false),
            };
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

        let mode = if args.continuable {
            crate::descriptor::Mode::Continuable
        } else {
            crate::descriptor::Mode::OneShot
        };
        let label = if args.label.trim().is_empty() {
            None
        } else {
            Some(args.label.as_str())
        };
        let run = self.runner.run_child(
            &ctx,
            &args.prompt,
            &args.system,
            &args.model,
            schema.clone(),
            mode,
            label,
        );
        let out = match run {
            Ok(out) => out,
            Err(e) => {
                if forge_agent::is_cancelled(&e) {
                    return cancelled_result("");
                }
                return result(ChildRun::default(), Some(e), schema.is_some());
            }
        };

        // Both guards drop here: release the serial slot, then unregister.
        drop(release);
        drop(unregister);
        result(out, None, schema.is_some())
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
/// are both `Code="subagent_failed"`; cancellation has its own code; a required
/// structured result that never arrived is `bad_output` (a failure); empty text
/// without a schema is a success that says what happened, not a failure.
pub(crate) fn result(
    out: ChildRun,
    setup_error: Option<Error>,
    expects_structured: bool,
) -> ToolResult {
    let mut meta = serde_json::Map::new();
    meta.insert("steps".into(), json!(out.steps));
    meta.insert("turns".into(), json!(out.turns));
    if !out.id.is_empty() {
        meta.insert("sessionId".into(), json!(out.id));
    }
    if !out.model.is_empty() {
        meta.insert("model".into(), json!(out.model));
    }
    if let Some(v) = &out.structured {
        meta.insert("structured".into(), v.clone());
    }
    // The catalog row the parent loop must append. A tool never writes the
    // parent log; it hands the loop a payload through `Result.meta`, exactly as
    // `compress` hands it `contextBlock`.
    if let Some(catalog) = &out.catalog {
        meta.insert(crate::catalog::CATALOG_META.into(), catalog.clone());
    }
    if let Some(name) = &out.name {
        meta.insert("name".into(), json!(name));
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
    } else if expects_structured && out.structured.is_none() {
        // A schema was supplied, so free text is not an acceptable answer even
        // though the child ran fine. This is the whole point of the contract.
        ToolResult::error(
            CODE_BAD_OUTPUT,
            format!(
                "subagent: the child finished without calling structured_output, but a schema was required (session {})",
                out.id
            ),
        )
    } else if let Some(v) = &out.structured {
        ToolResult::ok(serde_json::to_string(v).unwrap_or_else(|_| v.to_string()))
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
