//! The delegation runner: one serial slot, one tool, fresh child logs.
//!
//! `Runner::run` is public for embedders that want to drive a child directly;
//! the `subagent` tool is the normal entry point. A direct call still goes
//! through the depth check only via `tool()` — `run` itself is the primitive.

use std::sync::{Arc, Condvar, Mutex};

use forge_agent::{Cancel, EventLog, ToolResult, ToolRunner};
use forge_core::Result;
use forge_llm::{ToolCall, ToolSpec};
use forge_tool::{
    Call as ToolCallInput, Executor, ExecutorOptions, Limits, Policy, Registry, Tool,
    CODE_CANCELLED, CODE_IO_ERROR,
};

use crate::child::{open_child_log, read_child_facts, write_end, ChildRun};
use crate::options::{Options, DEFAULT_MAX_DEPTH};
use crate::tool::{DelegateTool, DepthExceeded, TOOL_NAME};

/// What a direct `Runner::run` call needs from its caller. The tool builds one
/// of these internally; there is no context parameter on `forge_tool::Tool`.
#[derive(Debug, Default)]
pub struct CallContext {
    /// Polled by the child agent at every stream boundary.
    pub cancel: Cancel,
    /// Overrides `Options::parent_session_id` when non-empty.
    pub parent_session_id: String,
    /// Overrides `Options::workspace` when non-empty.
    pub workspace: String,
}

/// A serial semaphore with a cancellable wait. Both release and cancellation
/// notify the shared pair, so a queued call wakes up without polling or
/// sleeping — it re-checks its `Cancel` after each wake.
pub(crate) struct Semaphore {
    free: Mutex<bool>,
    wake: Arc<(Mutex<()>, Condvar)>,
}

impl Semaphore {
    fn new(wake: Arc<(Mutex<()>, Condvar)>) -> Self {
        Self {
            free: Mutex::new(true),
            wake,
        }
    }

    pub(crate) fn release(&self) {
        if let Ok(guard) = self.wake.0.lock() {
            if let Ok(mut free) = self.free.lock() {
                *free = true;
            }
            self.wake.1.notify_all();
            drop(guard);
        }
    }
}

/// Delegation runner. Wrap in an `Arc` to obtain its tool.
pub struct Runner {
    options: Options,
    max_depth: usize,
    sem: Semaphore,
}

impl Runner {
    /// Validate the options and freeze the effective max depth.
    pub fn new(options: Options) -> Result<Self> {
        options.validate()?;
        let wake = options.canceller.wake_pair();
        let max_depth = if options.max_depth == 0 {
            DEFAULT_MAX_DEPTH
        } else {
            options.max_depth
        };
        Ok(Self {
            options,
            max_depth,
            sem: Semaphore::new(wake),
        })
    }

    /// The tool advertised to the model. At the depth limit this is a tool
    /// that always returns `depth_exceeded` — it is never unregistered.
    pub fn tool(self: &Arc<Self>) -> Arc<dyn Tool> {
        if self.options.depth >= self.max_depth {
            Arc::new(DepthExceeded {
                depth: self.options.depth,
                max_depth: self.max_depth,
            })
        } else {
            Arc::new(DelegateTool {
                runner: Arc::clone(self),
            })
        }
    }

    /// Effective depth limit (zero in `Options` became `DEFAULT_MAX_DEPTH`).
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// The options this runner was built with.
    pub fn options(&self) -> &Options {
        &self.options
    }

    pub(crate) fn semaphore(&self) -> &Semaphore {
        &self.sem
    }

    /// Acquire the single serial slot. Returns `false` if cancelled while
    /// queued — the caller must turn that into a `cancelled` tool result.
    pub(crate) fn acquire(&self, cancel: &Cancel) -> bool {
        let wake = &self.sem.wake;
        let mut guard = match wake.0.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        loop {
            if cancel.is_cancelled() || self.options.canceller.is_cancelled() {
                return false;
            }
            {
                let mut free = match self.sem.free.lock() {
                    Ok(g) => g,
                    Err(_) => return false,
                };
                if *free {
                    *free = false;
                    return true;
                }
            }
            guard = match wake.1.wait(guard) {
                Ok(g) => g,
                Err(_) => return false,
            };
        }
    }

    /// Run one child session and return its log-derived outcome.
    ///
    /// Setup failures (bad options, log creation, head write, agent
    /// construction) are `Err`. A failure after the child started is reported
    /// in `ChildRun::error` so the terminal `session/end` is still written.
    pub fn run(
        &self,
        ctx: &CallContext,
        prompt: &str,
        system_override: &str,
        model_override: &str,
    ) -> Result<ChildRun> {
        if ctx.cancel.is_cancelled() || self.options.canceller.is_cancelled() {
            return Err(forge_agent::cancelled_error());
        }

        let model = if model_override.trim().is_empty() {
            self.options.model.clone()
        } else {
            model_override.trim().to_string()
        };
        let system = if system_override.trim().is_empty() {
            self.options.system.clone()
        } else {
            system_override.trim().to_string()
        };
        let workspace = if ctx.workspace.is_empty() {
            self.options.workspace.clone()
        } else {
            ctx.workspace.clone()
        };
        let parent = if ctx.parent_session_id.is_empty() {
            self.options.parent_session_id.clone()
        } else {
            ctx.parent_session_id.clone()
        };
        let depth = self.options.depth + 1;

        let (mut out, store) =
            open_child_log(&self.options, &workspace, &model, &system, &parent, depth)?;
        let tools = self.child_tools(&out.id, &workspace)?;

        let store = Arc::new(store);
        let log: Arc<dyn EventLog> = store.clone();
        let mut agent_opt = forge_agent::Options::new(
            Arc::clone(&self.options.provider),
            log,
            model,
            out.id.clone(),
            workspace,
        );
        agent_opt.system = system;
        agent_opt.tools = tools;
        agent_opt.max_steps = self.options.effective_max_steps();
        agent_opt.clock = Arc::clone(&self.options.clock);
        agent_opt.cancel = ctx.cancel.clone();

        let agent = forge_agent::Agent::new(agent_opt)?;
        let (run_result, loop_error) = agent.run(prompt);

        out.steps = run_result.steps;
        out.error = loop_error;
        out.cancelled = ctx.cancel.is_cancelled()
            || run_result.reason == forge_agent::TURN_CANCELLED
            || out
                .error
                .as_ref()
                .map(forge_agent::is_cancelled)
                .unwrap_or(false);

        let (turns, text) = read_child_facts(&store);
        out.turns = turns;
        out.text = text;

        let reason = if out.error.is_some() {
            "aborted"
        } else {
            "completed"
        };
        write_end(&store, &out.id, reason, out.turns)?;
        Ok(out)
    }

    /// Build the child's tool runner: parent tools by name (through the parent
    /// `ToolRunner`) plus a nested `subagent` tool at `Depth+1`. Policy and
    /// limits are inherited; the approver is never carried over.
    fn child_tools(&self, self_id: &str, workspace: &str) -> Result<Option<Arc<dyn ToolRunner>>> {
        let parent = match &self.options.tools {
            Some(p) => Arc::clone(p),
            None => return Ok(None),
        };
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        for spec in parent.specs() {
            if spec.name == TOOL_NAME {
                continue;
            }
            tools.push(Arc::new(AdaptedTool {
                spec,
                runner: Arc::clone(&parent),
            }));
        }

        let nested_opt = Options {
            provider: Arc::clone(&self.options.provider),
            data_dir: self.options.data_dir.clone(),
            workspace: self.options.workspace.clone(),
            model: self.options.model.clone(),
            system: self.options.system.clone(),
            tools: Some(Arc::clone(&parent)),
            max_steps: self.options.max_steps,
            depth: self.options.depth + 1,
            max_depth: self.max_depth,
            parent_session_id: self_id.to_string(),
            clock: Arc::clone(&self.options.clock),
            now: Arc::clone(&self.options.now),
            logger: Arc::clone(&self.options.logger),
            policy: self.options.policy.clone(),
            limits: self.options.limits,
            canceller: self.options.canceller.clone(),
        };
        let nested = Arc::new(Runner::new(nested_opt)?);
        tools.push(nested.tool());

        let registry = Arc::new(Registry::new(tools));
        Ok(Some(Arc::new(ChildToolRunner {
            registry,
            policy: self.options.policy.clone(),
            limits: self.options.limits,
            workspace: workspace.to_string(),
        })))
    }
}

/// Adapt a parent `ToolRunner` tool into a `forge_tool::Tool` so it can sit in
/// the child registry. The parent's own policy/approver stay with the parent;
/// the child executor applies its own (approver-free) policy around this call.
struct AdaptedTool {
    spec: ToolSpec,
    runner: Arc<dyn ToolRunner>,
}

impl Tool for AdaptedTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call(&self, call: ToolCallInput) -> forge_tool::Result {
        let tc = ToolCall {
            id: call.id,
            name: call.name,
            args: call.args,
        };
        match self.runner.run(tc) {
            Ok(r) => forge_tool::Result {
                output: r.output,
                truncated: r.truncated,
                is_error: r.is_error,
                code: r.code,
                meta: r
                    .meta
                    .as_ref()
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .unwrap_or_default(),
            },
            Err(e) => {
                let code = if forge_agent::is_cancelled(&e) {
                    CODE_CANCELLED
                } else {
                    CODE_IO_ERROR
                };
                forge_tool::Result::error(code, e.message())
            }
        }
    }
}

/// A `forge_tool::Executor` wrapped as the agent's `ToolRunner`. Executor owns
/// policy, limits and the (always absent) approver; it also records the tool
/// decision and duration in the event payload via `tool/result` meta.
struct ChildToolRunner {
    registry: Arc<Registry>,
    policy: Option<Arc<dyn Policy>>,
    limits: Limits,
    workspace: String,
}

impl ToolRunner for ChildToolRunner {
    fn specs(&self) -> Vec<ToolSpec> {
        self.registry.specs()
    }

    fn run(&self, call: ToolCall) -> Result<ToolResult> {
        let executor = Executor::new(
            Arc::clone(&self.registry),
            ExecutorOptions {
                policy: self.policy.clone(),
                approver: None,
                limits: self.limits,
                workspace: self.workspace.clone(),
            },
        );
        let fc = ToolCallInput::from_tool_call(&call, &self.workspace);
        let r = executor.run(fc);
        Ok(ToolResult {
            output: r.output,
            is_error: r.is_error,
            code: r.code,
            truncated: r.truncated,
            meta: if r.meta.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(r.meta))
            },
        })
    }
}
