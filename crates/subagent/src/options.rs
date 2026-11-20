//! Constructor knobs for a delegation `Runner`.
//!
//! Go's `Options` carries an `agent.ToolSet` (registry, policy, approver,
//! limits) and receives a `context.Context` on every call. The frozen Rust
//! `forge_agent::ToolRunner` exposes only `specs`/`run`, and
//! `forge_tool::Tool::call` has no context parameter, so this port keeps the
//! policy and limits here, drops the approver entirely, and carries
//! cancellation in [`Canceller`]. See the crate README for the full deviation
//! list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use forge_agent::{Cancel, ToolRunner};
use forge_core::{Error, ErrorKind, Result};
use forge_llm::Provider;
use forge_tool::{Limits, Policy};

/// Epoch milliseconds, the same type `forge_agent` uses for event timestamps.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Epoch milliseconds used only to mint a fresh child session id. Kept
/// separate from [`Clock`] so goldens can freeze the id without freezing every
/// event timestamp.
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Go's `DefaultMaxDepth`.
pub const DEFAULT_MAX_DEPTH: usize = 1;
/// Go's `agent` default when `max_steps` is zero.
pub const DEFAULT_MAX_STEPS: usize = 24;
/// Default concurrency: 1 — the serial behaviour the Go oracle pins. DSH's
/// `SubagentRuntime` defaults to 8; set [`Options::max_active`] to raise it.
pub const DEFAULT_MAX_ACTIVE: usize = 1;

/// A deliberately tiny logging seam. No `tracing`/`log` crate is in the
/// workspace dependency budget, so the default is a no-op and embedders can
/// supply a closure-backed implementation.
pub trait Logger: Send + Sync {
    fn debug(&self, _message: &str) {}
    fn info(&self, _message: &str) {}
    fn warn(&self, _message: &str) {}
    fn error(&self, _message: &str) {}
}

/// The default logger: drops every message.
#[derive(Debug, Default)]
pub struct NoopLogger;

impl Logger for NoopLogger {}

/// Cancellation for delegations.
///
/// `cancel_all` is the parent-session stop button; it sets the root flag and
/// also trips every registered per-call token so an in-flight child sees the
/// stop at its next stream boundary. `cancel_call` targets one `callId`, which
/// is what makes "cancelled while queued" independent of sibling calls.
#[derive(Clone, Default)]
pub struct Canceller {
    root: Arc<AtomicBool>,
    active: Arc<Mutex<HashMap<String, Cancel>>>,
    /// Shared wake-up pair: cancellation and semaphore release both notify
    /// it, so a queued call observes cancellation without polling or sleeping.
    wake: Arc<(Mutex<()>, Condvar)>,
}

impl Canceller {
    /// The pair a serial semaphore must wait on to be cancellable.
    pub fn wake_pair(&self) -> Arc<(Mutex<()>, Condvar)> {
        self.wake.clone()
    }

    /// True when the whole runner has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.root.load(Ordering::SeqCst)
    }

    /// Stop every in-flight and queued delegation.
    pub fn cancel_all(&self) {
        self.root.store(true, Ordering::SeqCst);
        if let Ok(active) = self.active.lock() {
            for cancel in active.values() {
                // The parent is going away; its children go with it.
                cancel.cancel_with(forge_agent::Cause::Parent);
            }
        }
        Self::notify(&self.wake);
    }

    /// Stop one delegation, identified by the model's `callId`. Returns whether a
    /// live token was found and tripped, so a control surface can tell "stopped
    /// it" from "nothing was running under that id".
    pub fn cancel_call(&self, call_id: &str) -> bool {
        let mut found = false;
        if let Ok(active) = self.active.lock() {
            if let Some(cancel) = active.get(call_id) {
                cancel.cancel();
                found = true;
            }
        }
        Self::notify(&self.wake);
        found
    }

    /// Register a fresh per-call token. A duplicate `callId` replaces the old
    /// token, which is harmless: the old call is finishing and unregisters.
    pub fn register(&self, call_id: String, cancel: Cancel) {
        if let Ok(mut active) = self.active.lock() {
            active.insert(call_id, cancel);
        }
    }

    /// Drop the token when a call finishes so a later id can be reused.
    pub fn unregister(&self, call_id: &str) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(call_id);
        }
    }

    /// Notify every waiter on the shared wake pair. The guard is taken before
    /// the notify so a waiter holding it cannot miss the signal between its
    /// predicate check and `Condvar::wait`.
    fn notify(wake: &Arc<(Mutex<()>, Condvar)>) {
        if let Ok(guard) = wake.0.lock() {
            wake.1.notify_all();
            drop(guard);
        }
    }
}

/// Everything a [`crate::Runner`] needs. No hidden global state.
pub struct Options {
    pub provider: Arc<dyn Provider>,
    pub data_dir: PathBuf,
    pub workspace: String,
    pub model: String,
    pub system: String,
    pub tools: Option<Arc<dyn ToolRunner>>,
    /// Zero selects [`DEFAULT_MAX_STEPS`].
    pub max_steps: usize,
    /// Zero is the top level; each delegation adds one.
    pub depth: usize,
    /// Zero selects [`DEFAULT_MAX_DEPTH`].
    pub max_depth: usize,
    /// How many children may run at the same time; the rest wait their turn.
    /// Zero selects [`DEFAULT_MAX_ACTIVE`] (1 — the serial behaviour the Go
    /// oracle pins). Raise it for concurrency (DSH defaults to 8). Unlike DSH, a
    /// call over the limit queues rather than failing `ACTIVATION_LIMIT_REACHED`:
    /// the tool is synchronous, so queueing is the honest behaviour for a model
    /// that asked for the work.
    pub max_active: usize,
    pub parent_session_id: String,
    /// The parent session's log file, when there is one. The runner reads it to
    /// reserve a sibling name and to build the catalog row the loop will append;
    /// it never writes it (the loop is the parent log's only writer).
    pub parent_log: Option<PathBuf>,
    pub clock: Clock,
    pub now: NowFn,
    pub logger: Arc<dyn Logger>,
    /// Inherited by every child executor. There is deliberately no approver:
    /// a child that needs `Decision::Ask` fails closed with `no_approver`.
    pub policy: Option<Arc<dyn Policy>>,
    pub limits: Limits,
    pub canceller: Canceller,
    /// Optional JSON-schema subset the child must satisfy by calling
    /// `structured_output` before it finishes. `None` (the default) keeps the
    /// free-text behaviour. A per-call `schema` on the `subagent` tool overrides
    /// this value for that one delegation.
    pub structured_schema: Option<serde_json::Value>,
    /// Where a child runs. `None` (the default) runs it in this process, as it
    /// always has. `Some` hands the child to another place — the one shipped
    /// implementation spawns our own binary in worker mode over stdio. See
    /// [`crate::backend`]. The in-process pool and depth limits still apply on
    /// this side; the backend only decides *where* a permitted child runs.
    pub backend: Option<Arc<dyn crate::backend::Backend>>,
    /// Named external providers, looked up by [`Options::backend_name`]. A caller
    /// registers a [`crate::backend::Backend`] here (or on a clone of this
    /// handle, since [`crate::providers::Providers`] is shared) and refers to it
    /// by name. Empty means "in-process" unless `backend` is set directly.
    pub providers: crate::providers::Providers,
    /// Resolve the child's backend from [`Options::providers`] by this name.
    /// Ignored when `backend` is set. A name that is not registered is an error,
    /// not a silent fallback: a typo must not turn "run in a worker" into "run
    /// here".
    pub backend_name: Option<String>,
}

impl Options {
    /// Build options with the Go defaults (24 steps, depth 0, max depth 1,
    /// wall clocks, no tools, no policy, no approver).
    pub fn new(
        provider: Arc<dyn Provider>,
        data_dir: impl AsRef<Path>,
        workspace: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            data_dir: data_dir.as_ref().to_path_buf(),
            workspace: workspace.into(),
            model: model.into(),
            system: String::new(),
            tools: None,
            max_steps: 0,
            depth: 0,
            max_depth: 0,
            max_active: 0,
            parent_session_id: String::new(),
            parent_log: None,
            clock: Arc::new(wall_clock_ms),
            now: Arc::new(wall_clock_ms),
            logger: Arc::new(NoopLogger),
            policy: None,
            limits: Limits::default(),
            canceller: Canceller::default(),
            structured_schema: None,
            backend: None,
            providers: crate::providers::Providers::new(),
            backend_name: None,
        }
    }

    /// Validate the knobs that Go validates in `New`. Negative integers cannot
    /// occur with `usize`; zero depth/max-depth are defaults, not errors.
    pub fn validate(&self) -> Result<()> {
        if self.data_dir.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "subagent: DataDir is required",
            ));
        }
        if self.model.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "subagent: Model is required",
            ));
        }
        Ok(())
    }

    /// Effective step cap handed to the child agent.
    pub fn effective_max_steps(&self) -> usize {
        if self.max_steps == 0 {
            DEFAULT_MAX_STEPS
        } else {
            self.max_steps
        }
    }
}

fn wall_clock_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}
