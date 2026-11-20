//! The delegation runner: a bounded pool of concurrent children, one tool,
//! fresh child logs.
//!
//! `Runner::run` is public for embedders that want to drive a child directly;
//! the `subagent` tool is the normal entry point. A direct call still goes
//! through the depth check only via `tool()` — `run` itself is the primitive.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};

use forge_agent::{Cancel, EventLog, ToolResult, ToolRunner};
use forge_core::{Error, ErrorKind, EventType, Result};
use forge_llm::{ToolCall, ToolSpec};
use forge_tool::{
    Call as ToolCallInput, Executor, ExecutorOptions, Limits, Policy, Registry, Tool,
    CODE_CANCELLED, CODE_IO_ERROR,
};
use serde_json::Value;

use crate::backend::Backend;
use crate::child::{open_child_log, read_child_facts, reopen_child_log, write_end, ChildRun};
use crate::descriptor::Mode;
use crate::lock::ChildLock;
use crate::options::{Options, DEFAULT_MAX_ACTIVE, DEFAULT_MAX_DEPTH};
use crate::structured::{GuardedTool, StructuredOutputTool, STRUCTURED_OUTPUT_TOOL};
use crate::tool::{DelegateTool, DepthExceeded, TOOL_NAME};

/// How a prompt reaches a child. In an asynchronous runtime these differ (queue
/// waits for the next turn, steer interrupts the current step). forge-rs runs a
/// child synchronously, so there is no in-flight step to interrupt: both append
/// a turn. The distinction is kept because the tools and callers already speak
/// it, and a future async backend can honour it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Queue,
    Steer,
}

/// What a direct `Runner::run` call needs from its caller. The tool builds one
/// of these internally; there is no context parameter on `forge_tool::Tool`.
#[derive(Debug, Clone, Default)]
pub struct CallContext {
    /// Polled by the child agent at every stream boundary.
    pub cancel: Cancel,
    /// Overrides `Options::parent_session_id` when non-empty.
    pub parent_session_id: String,
    /// Overrides `Options::workspace` when non-empty.
    pub workspace: String,
}

/// A counting semaphore with a cancellable wait: up to `capacity` children run
/// at once, the rest queue. Both release and cancellation notify the shared
/// pair, so a queued call wakes up without polling or sleeping — it re-checks its
/// `Cancel` after each wake.
pub(crate) struct Semaphore {
    capacity: usize,
    free: Mutex<usize>,
    wake: Arc<(Mutex<()>, Condvar)>,
}

impl Semaphore {
    fn new(wake: Arc<(Mutex<()>, Condvar)>, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            free: Mutex::new(capacity),
            wake,
        }
    }

    /// Take one slot if a slot is free. Non-blocking; the caller waits on the
    /// condition variable and tries again.
    fn try_acquire(&self) -> bool {
        let Ok(mut free) = self.free.lock() else {
            return false;
        };
        if *free > 0 {
            *free -= 1;
            true
        } else {
            false
        }
    }

    pub(crate) fn release(&self) {
        if let Ok(guard) = self.wake.0.lock() {
            if let Ok(mut free) = self.free.lock() {
                *free = (*free + 1).min(self.capacity);
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
    /// Serialises operations on one child's log (resume vs resume). Two callers
    /// appending to one child at once would interleave turns.
    lock: ChildLock,
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
        let capacity = if options.max_active == 0 {
            DEFAULT_MAX_ACTIVE
        } else {
            options.max_active
        };
        Ok(Self {
            options,
            max_depth,
            sem: Semaphore::new(wake, capacity),
            lock: ChildLock::new(),
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

    /// The named-provider registry this runner consults. Register a backend here
    /// (through a clone of the handle) and set `Options::backend_name` to route
    /// children to it.
    pub fn providers(&self) -> &crate::providers::Providers {
        &self.options.providers
    }

    /// Which backend a child runs on, if any: the directly-set `backend` wins,
    /// else the name is resolved through the provider registry. A name that is
    /// not registered is an error — a typo must not silently run in-process.
    fn resolve_backend(&self) -> Result<Option<Arc<dyn Backend>>> {
        if let Some(backend) = &self.options.backend {
            return Ok(Some(Arc::clone(backend)));
        }
        match &self.options.backend_name {
            None => Ok(None),
            Some(name) => match self.options.providers.get(name) {
                Some(backend) => Ok(Some(backend)),
                None => Err(Error::new(
                    ErrorKind::NotFound,
                    format!("subagent: no provider named {name:?} is registered"),
                )),
            },
        }
    }

    /// Stop a running or queued child by its id. Returns whether a live child was
    /// found and cancelled. Works in-process: the child registers its cancel
    /// token under its own id for the duration of its run.
    pub fn interrupt(&self, child_id: &str) -> bool {
        self.options.canceller.cancel_call(child_id)
    }

    /// Honour a cross-process interrupt marker (`<data_dir>/sessions/<id>.cancel`)
    /// while this child runs: when the file appears, trip the same cancel token
    /// `interrupt` would. Returns a stop flag and the watcher handle; the caller
    /// sets the flag and joins after the run so the watcher never outlives it.
    fn watch_cancel(
        &self,
        child_id: &str,
    ) -> (
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    ) {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = Arc::clone(&done);
        let data_dir = self.options.data_dir.clone();
        let id = child_id.to_string();
        let canceller = self.options.canceller.clone();
        let handle = std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            while !stop.load(Ordering::SeqCst) {
                if crate::cancel_file::is_requested(&data_dir, &id) {
                    canceller.cancel_call(&id);
                    let _ = crate::cancel_file::clear(&data_dir, &id);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
        (done, handle)
    }

    /// Send more work to a continuable child, tagged with a [`Delivery`]. Both
    /// deliveries append a turn here (see [`Delivery`]); this is the named
    /// control entry point a UI or a tool uses so the intent is on the record.
    pub fn deliver(&self, child_id: &str, prompt: &str, _delivery: Delivery) -> Result<ChildRun> {
        self.resume(child_id, prompt, "", "")
    }

    pub(crate) fn semaphore(&self) -> &Semaphore {
        &self.sem
    }

    /// Start a child on a background thread and leave its settlement in the
    /// parent's durable queue instead of returning it to the caller.
    ///
    /// This is the asynchronous shape DSH calls a "settled notice": the caller
    /// delegates and moves on, and the result arrives later as a `next-turn`
    /// message. Two things make that honest here. The child runs in its own
    /// session log, so nothing about it depends on this process staying alive.
    /// And the notice is an `agent/inbox` row on the parent log, so a parent
    /// that was restarted between delegation and settlement still finds it.
    ///
    /// A runner with no `parent_log` cannot deliver anything, so this refuses up
    /// front rather than spawning a thread whose only outcome is a lost result.
    pub fn spawn(
        self: &Arc<Self>,
        ctx: CallContext,
        prompt: impl Into<String>,
        label: Option<String>,
    ) -> Result<()> {
        if self.options.parent_log.is_none() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "subagent: spawn needs a parent log to deliver its notice to",
            ));
        }
        let runner = Arc::clone(self);
        let prompt = prompt.into();
        std::thread::spawn(move || {
            // The slot is taken inside the thread, not by the caller: a caller
            // that acquired it and returned immediately would let every
            // background child run at once, which is exactly what the pool is
            // for. Queueing here means a background delegation waits its turn
            // like a foreground one.
            if !runner.acquire(&ctx.cancel) {
                let _ = runner.leave_settlement(&Err(forge_core::Error::new(
                    ErrorKind::Refused,
                    "subagent: the child was cancelled before it started",
                )));
                return;
            }
            let out = runner.run_child(
                &ctx,
                &prompt,
                "",
                "",
                None,
                Mode::Continuable,
                label.as_deref(),
            );
            runner.semaphore().release();
            let _ = runner.leave_settlement(&out);
        });
        Ok(())
    }

    /// Record how a child ended as a `next-turn` message for the parent. The
    /// notice is the same text the synchronous path would have returned, so a
    /// reader cannot tell which delivery shape a child used from its words
    /// alone -- only from the row.
    fn leave_settlement(&self, out: &Result<ChildRun>) -> Result<()> {
        let Some(path) = self.options.parent_log.as_ref() else {
            return Ok(());
        };
        let notice = match out {
            Ok(run) => {
                let name = run.name.clone().unwrap_or_else(|| run.id.clone());
                crate::continuation::settlement_notice(
                    &name,
                    "completed",
                    run.mode,
                    &run.text,
                    run.structured.as_ref(),
                )
            }
            // A child that never started has no closing text to quote, but the
            // parent still has to learn that its delegation went nowhere.
            Err(e) => format!(
                "a delegated child failed before it settled: {}",
                e.message()
            ),
        };
        let store = forge_session::Store::open(path, forge_session::Options::default())?;
        let mut events = Vec::new();
        store.read_from(0, |ev| {
            events.push(ev.clone());
            Ok(())
        })?;
        let pending = forge_agent::inbox::fold(&events);
        let msg = forge_agent::inbox::Msg::new(notice)?;
        let event = forge_agent::inbox::enqueue(
            &self.options.parent_session_id,
            forge_agent::inbox::Target::NextTurn,
            &pending,
            &msg,
        );
        store.append(&event)?;
        Ok(())
    }

    /// Acquire one of the concurrent slots. Returns `false` if cancelled while
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
            if self.sem.try_acquire() {
                return true;
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
        self.run_with_schema(ctx, prompt, system_override, model_override, None)
    }

    /// Like [`Runner::run`] but lets the caller supply a result schema for this
    /// one child, overriding `Options::structured_schema`. When a schema is in
    /// effect the child must finish by calling `structured_output`; a run that
    /// never does is reported as `bad_output` by the tool, not as a success.
    pub fn run_with_schema(
        &self,
        ctx: &CallContext,
        prompt: &str,
        system_override: &str,
        model_override: &str,
        structured_schema: Option<Value>,
    ) -> Result<ChildRun> {
        self.run_child(
            ctx,
            prompt,
            system_override,
            model_override,
            structured_schema,
            Mode::OneShot,
            None,
        )
    }

    /// Run one child session with an explicit creation mode. A `OneShot` child
    /// writes a terminal `session/end` when it finishes; a `Continuable` child
    /// stays open (its last `turn/end` is its resting state) so
    /// [`Runner::resume`] may append another turn later.
    // Eight parameters is the honest shape here: the four the run has always
    // taken, plus the schema, the mode and the label. Bundling them into a
    // struct would only move the count, not the coupling.
    #[allow(clippy::too_many_arguments)]
    pub fn run_child(
        &self,
        ctx: &CallContext,
        prompt: &str,
        system_override: &str,
        model_override: &str,
        structured_schema: Option<Value>,
        mode: Mode,
        label: Option<&str>,
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
        // Door one: a session that was archived starts no new children. The
        // check is on the parent, not on the child, because the child has no
        // log yet — this is the only moment the decision can be made.
        if !self.options.parent_session_id.is_empty() {
            crate::archive::admit(&self.options.data_dir, &self.options.parent_session_id)?;
        }
        let depth = self.options.depth + 1;
        let schema = structured_schema.or_else(|| self.options.structured_schema.clone());

        // A configured backend runs the child somewhere else. We do not open a
        // local child log for it: the worker owns that transcript. We still
        // reserve the sibling name and hand back the catalog row, because those
        // belong to the parent log, which is ours to read and the loop's to write.
        if let Some(backend) = self.resolve_backend()? {
            let mut request = crate::backend::BackendRequest::run(prompt, mode, workspace.clone())
                .with_system(system.clone())
                .with_model(model.clone());
            if let Some(s) = &schema {
                request = request.with_schema(s.clone());
            }
            let mut out = backend.call(&request)?.into_result()?;
            out.mode = mode;
            if let Some(parent_log) = &self.options.parent_log {
                let committed: std::collections::BTreeSet<String> =
                    crate::list::children_in_log(parent_log)
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|entry| entry.name)
                        .collect();
                if let Ok(name) = crate::names::reserve(&committed, None) {
                    out.name = Some(name.clone());
                    let entry =
                        crate::catalog::CatalogEntry::new(out.id.clone(), mode).with_name(name);
                    out.catalog = Some(entry.to_payload());
                }
            }
            return Ok(out);
        }

        let (mut out, store) = open_child_log(
            &self.options,
            &workspace,
            &model,
            &system,
            &parent,
            depth,
            mode,
            label,
        )?;

        // Reserve a sibling name against the parent catalog and prepare the row
        // the parent loop will append. This is a read of the parent log plus a
        // `Result.meta` hand-off — never a write: the loop is the parent log's
        // only writer (AGENTS.md rule 1). A parent that has no log (a team
        // worker) gets no name and no catalog row, which is correct: its
        // transcript belongs to the board, not to a session.
        if let Some(parent_log) = &self.options.parent_log {
            let committed: std::collections::BTreeSet<String> =
                crate::list::children_in_log(parent_log)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|entry| entry.name)
                    .collect();
            if let Ok(name) = crate::names::reserve(&committed, None) {
                out.name = Some(name.clone());
                let entry = crate::catalog::CatalogEntry::new(out.id.clone(), mode).with_name(name);
                out.catalog = Some(entry.to_payload());
            }
        }

        let tools = self.child_tools(&out.id, &workspace, schema.as_ref(), depth)?;

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
        self.options
            .canceller
            .register(out.id.clone(), ctx.cancel.clone());
        let (watch_done, watcher) = self.watch_cancel(&out.id);
        let (run_result, loop_error) = agent.run(prompt);
        self.options.canceller.unregister(&out.id);
        watch_done.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = watcher.join();

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
        out.structured = read_structured(&store);

        out.mode = mode;
        let reason = if out.error.is_some() {
            "aborted"
        } else {
            "completed"
        };
        // A continuable child is not terminal: leaving it without `session/end`
        // is what makes `resume` possible later (and mirrors how an HTTP session
        // rests between turns). A one-shot child is done the moment it ends.
        if mode == Mode::OneShot {
            write_end(&store, &out.id, reason, out.turns)?;
        }
        Ok(out)
    }

    /// Append another turn to a continuable child's existing log — a cold resume
    /// that works from a different process because it is driven entirely by the
    /// durable log, not by any in-memory activation. A one-shot child, a child
    /// that already ended, or a missing log all fail before a single event is
    /// written.
    pub fn resume(
        &self,
        child_id: &str,
        prompt: &str,
        system_override: &str,
        model_override: &str,
    ) -> Result<ChildRun> {
        // One child, one turn-appender at a time: a concurrent resume of the same
        // child would interleave `turn/start`/`turn/end` in its log.
        self.lock.run(child_id, || {
            self.resume_locked(child_id, prompt, system_override, model_override)
        })
    }

    fn resume_locked(
        &self,
        child_id: &str,
        prompt: &str,
        system_override: &str,
        model_override: &str,
    ) -> Result<ChildRun> {
        if self.options.canceller.is_cancelled() {
            return Err(forge_agent::cancelled_error());
        }
        // A backend resumes in the place the child actually ran. The local log
        // is only the truth for local children; a worker child's transcript is
        // the worker's.
        if let Some(backend) = self.resolve_backend()? {
            return backend
                .call(&crate::backend::BackendRequest::resume(child_id, prompt))?
                .into_result();
        }
        // Door two: resume. The log is reopened first only because that is where
        // the id's shape is validated: an id that is not a child id must be
        // reported as such, not as an archive problem. Reopening writes nothing,
        // so no work has been admitted when the archive check runs.
        let (mut out, store) = reopen_child_log(&self.options, child_id)?;
        crate::archive::admit(&self.options.data_dir, child_id)?;

        let model = if model_override.trim().is_empty() {
            if out.model.trim().is_empty() {
                self.options.model.clone()
            } else {
                out.model.clone()
            }
        } else {
            model_override.trim().to_string()
        };
        let system = if system_override.trim().is_empty() {
            self.options.system.clone()
        } else {
            system_override.trim().to_string()
        };
        let workspace = self.options.workspace.clone();

        let tools = self.child_tools(&out.id, &workspace, None, out.depth)?;
        let store = Arc::new(store);
        let log: Arc<dyn EventLog> = store.clone();
        let mut agent_opt = forge_agent::Options::new(
            Arc::clone(&self.options.provider),
            log,
            model.clone(),
            out.id.clone(),
            workspace,
        );
        agent_opt.system = system;
        agent_opt.tools = tools;
        agent_opt.max_steps = self.options.effective_max_steps();
        agent_opt.clock = Arc::clone(&self.options.clock);
        let cancel = Cancel::default();
        agent_opt.cancel = cancel.clone();

        let agent = forge_agent::Agent::new(agent_opt)?;
        self.options
            .canceller
            .register(out.id.clone(), cancel.clone());
        let (watch_done, watcher) = self.watch_cancel(&out.id);
        let (run_result, loop_error) = agent.run(prompt);
        self.options.canceller.unregister(&out.id);
        watch_done.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = watcher.join();

        out.model = model;
        out.steps = run_result.steps;
        out.error = loop_error;
        out.cancelled = run_result.reason == forge_agent::TURN_CANCELLED;
        let (turns, text) = read_child_facts(&store);
        out.turns = turns;
        out.text = text;
        out.structured = read_structured(&store);
        Ok(out)
    }

    /// Build the child's tool runner: parent tools by name (through the parent
    /// `ToolRunner`) plus a nested `subagent` tool for the child, plus — when a
    /// result schema is in effect — the `structured_output` terminal tool.
    /// Policy and limits are inherited; the approver is never carried over.
    ///
    /// `child_depth` is the depth of the child these tools serve. The nested
    /// `subagent` tool is built at exactly that depth, so a grandchild it spawns
    /// lands at `child_depth + 1` and is refused once that reaches `max_depth`.
    /// A cold resume passes the depth read from the child's own header, so depth
    /// does not reset to zero in a fresh process.
    ///
    /// When `structured_schema` is `Some`, every other tool is wrapped in a
    /// [`GuardedTool`] sharing one “terminated” flag, so any call after
    /// `structured_output` fails closed. A registry is built even when there are
    /// no parent tools, because the terminal tool must still be advertised.
    fn child_tools(
        &self,
        self_id: &str,
        workspace: &str,
        structured_schema: Option<&Value>,
        child_depth: usize,
    ) -> Result<Option<Arc<dyn ToolRunner>>> {
        let parent = self.options.tools.as_ref().map(Arc::clone);
        if parent.is_none() && structured_schema.is_none() {
            return Ok(None);
        }

        let terminated = structured_schema.map(|_| Arc::new(AtomicBool::new(false)));
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        if let Some(parent) = &parent {
            for spec in parent.specs() {
                if spec.name == TOOL_NAME {
                    continue;
                }
                let inner: Arc<dyn Tool> = Arc::new(AdaptedTool {
                    spec,
                    runner: Arc::clone(parent),
                });
                tools.push(guard(inner, terminated.as_ref()));
            }

            let nested_opt = Options {
                provider: Arc::clone(&self.options.provider),
                data_dir: self.options.data_dir.clone(),
                workspace: self.options.workspace.clone(),
                model: self.options.model.clone(),
                system: self.options.system.clone(),
                tools: Some(Arc::clone(parent)),
                max_steps: self.options.max_steps,
                depth: child_depth,
                max_depth: self.max_depth,
                max_active: self.options.max_active,
                parent_session_id: self_id.to_string(),
                parent_log: Some(crate::child::child_log_path(
                    &self.options.data_dir,
                    self_id,
                )),
                clock: Arc::clone(&self.options.clock),
                now: Arc::clone(&self.options.now),
                logger: Arc::clone(&self.options.logger),
                policy: self.options.policy.clone(),
                limits: self.options.limits,
                canceller: self.options.canceller.clone(),
                structured_schema: None,
                // A nested child runs inside whichever process this runner is in,
                // so it never re-delegates to an external backend.
                backend: None,
                providers: crate::providers::Providers::new(),
                backend_name: None,
            };
            let nested = Arc::new(Runner::new(nested_opt)?);
            tools.push(guard(nested.tool(), terminated.as_ref()));
        }

        if let (Some(schema), Some(flag)) = (structured_schema, terminated.as_ref()) {
            tools.push(Arc::new(StructuredOutputTool {
                schema: schema.clone(),
                terminated: Arc::clone(flag),
            }));
        }

        let registry = Arc::new(Registry::new(tools));
        Ok(Some(Arc::new(ChildToolRunner {
            registry,
            policy: self.options.policy.clone(),
            limits: self.options.limits,
            workspace: workspace.to_string(),
        })))
    }
}

/// Wrap a tool in the terminated guard when a schema is in effect.
fn guard(inner: Arc<dyn Tool>, terminated: Option<&Arc<AtomicBool>>) -> Arc<dyn Tool> {
    match terminated {
        Some(flag) => Arc::new(GuardedTool {
            inner,
            terminated: Arc::clone(flag),
        }),
        None => inner,
    }
}

/// Recover the child's structured result from its own log. A `tool/result`
/// records `tool`/`callId`/`ok` but not the arguments, so we anchor on the
/// successful `structured_output` result, then read `args` from the `tool/call`
/// with that same `callId`. Only the last successful submission can exist
/// because the guard rejects every later call.
fn read_structured(store: &forge_session::Store) -> Option<Value> {
    let mut events: Vec<forge_core::Event> = Vec::new();
    if store
        .read_from(0, |event: &forge_core::Event| {
            events.push(event.clone());
            Ok(())
        })
        .is_err()
    {
        return None;
    }

    let mut accepted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for event in &events {
        if event.typ.as_str() != EventType::TOOL_RESULT.as_str() {
            continue;
        }
        if let Some(payload) = &event.payload {
            let is_submission =
                payload.get("tool").and_then(Value::as_str) == Some(STRUCTURED_OUTPUT_TOOL);
            let ok = payload.get("ok").and_then(Value::as_bool) == Some(true);
            if is_submission && ok {
                if let Some(call_id) = payload.get("callId").and_then(Value::as_str) {
                    accepted.insert(call_id.to_string());
                }
            }
        }
    }
    if accepted.is_empty() {
        return None;
    }

    let mut structured = None;
    for event in &events {
        if event.typ.as_str() != EventType::TOOL_CALL.as_str() {
            continue;
        }
        if let Some(payload) = &event.payload {
            if payload.get("tool").and_then(Value::as_str) != Some(STRUCTURED_OUTPUT_TOOL) {
                continue;
            }
            if let Some(call_id) = payload.get("callId").and_then(Value::as_str) {
                if accepted.contains(call_id) {
                    structured = payload.get("args").cloned();
                }
            }
        }
    }
    structured
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
                parts: r.parts,
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
            parts: r.parts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wake() -> Arc<(Mutex<()>, Condvar)> {
        Arc::new((Mutex::new(()), Condvar::new()))
    }

    #[test]
    fn the_pool_admits_exactly_its_capacity_then_makes_the_rest_wait() {
        let sem = Semaphore::new(wake(), 2);
        assert!(sem.try_acquire(), "first slot");
        assert!(sem.try_acquire(), "second slot");
        assert!(!sem.try_acquire(), "third waits while two run");
        sem.release();
        assert!(sem.try_acquire(), "a released slot is reusable");
        assert!(!sem.try_acquire(), "and only one slot came back");
    }

    #[test]
    fn a_zero_capacity_pool_still_admits_one() {
        // A misconfiguration must not deadlock every delegation.
        let sem = Semaphore::new(wake(), 0);
        assert!(sem.try_acquire());
        assert!(!sem.try_acquire());
    }

    #[test]
    fn release_never_exceeds_capacity() {
        let sem = Semaphore::new(wake(), 1);
        sem.release();
        sem.release();
        assert!(sem.try_acquire());
        assert!(
            !sem.try_acquire(),
            "released more times than taken is clamped"
        );
    }
}
