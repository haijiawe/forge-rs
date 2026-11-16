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
                cancel.cancel();
            }
        }
        Self::notify(&self.wake);
    }

    /// Stop one delegation, identified by the model's `callId`.
    pub fn cancel_call(&self, call_id: &str) {
        if let Ok(active) = self.active.lock() {
            if let Some(cancel) = active.get(call_id) {
                cancel.cancel();
            }
        }
        Self::notify(&self.wake);
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
    pub parent_session_id: String,
    pub clock: Clock,
    pub now: NowFn,
    pub logger: Arc<dyn Logger>,
    /// Inherited by every child executor. There is deliberately no approver:
    /// a child that needs `Decision::Ask` fails closed with `no_approver`.
    pub policy: Option<Arc<dyn Policy>>,
    pub limits: Limits,
    pub canceller: Canceller,
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
            parent_session_id: String::new(),
            clock: Arc::new(wall_clock_ms),
            now: Arc::new(wall_clock_ms),
            logger: Arc::new(NoopLogger),
            policy: None,
            limits: Limits::default(),
            canceller: Canceller::default(),
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
