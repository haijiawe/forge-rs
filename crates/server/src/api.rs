//! The frozen surface between `forge-server` and whoever runs a turn.
//!
//! Layer L3. The HTTP layer owns sessions, the event log's seq counter and the
//! WebSocket fan-out; the runner owns a model call and its tool loop. Keeping
//! this trait in the consumer (server) prevents the runner from needing to know
//! HTTP, and lets the CLI and the e2e harness supply their own implementations.
//! `archive/forge-go/internal/server/runner.go` is the Go oracle. This file is owned by
//! the lead: an implementor reports a needed change instead of editing it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forge_core::{Event, Result};

/// Cooperative cancellation. There is no async runtime in this workspace:
/// cancellation is a flag the loop checks between stream deltas, never a thread
/// kill, and closing the stream is the runner's job.
#[derive(Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl Clone for CancelToken {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Advisory streaming text. The `assistant/message` event stays authoritative;
/// a lost delta is repaired by replaying the log.
pub trait DeltaSink: Send + Sync {
    fn delta(&self, turn: u32, step: u32, text: &str);
}

/// Text typed while a turn is running: in-memory only, never a second
/// `user/message` event written by the loop.
pub trait SteeringSource: Send + Sync {
    fn take_steering(&self) -> Vec<String>;
}

/// Everything a runner may learn from the server for one turn.
#[derive(Clone, Default)]
pub struct TurnContext {
    pub cancel: CancelToken,
    pub delta: Option<Arc<dyn DeltaSink>>,
    pub steering: Option<Arc<dyn SteeringSource>>,
}

impl TurnContext {
    pub fn delta(&self, turn: u32, step: u32, text: &str) {
        if let Some(sink) = &self.delta {
            sink.delta(turn, step, text);
        }
    }
    pub fn take_steering(&self) -> Vec<String> {
        self.steering
            .as_ref()
            .map(|source| source.take_steering())
            .unwrap_or_default()
    }
}

/// One turn of work. `run_turn` must append its own `turn/end` even when it is
/// cancelled; `cancel` only flips the token and appends nothing.
pub trait TurnRunner: Send + Sync {
    fn run_turn(&self, ctx: &TurnContext, session_id: &str, text: &str) -> Result<()>;
    fn cancel(&self) -> Result<()>;
}

/// How the browser UI is found. `None` means the build embedded it; a checkout
/// that never ran `make web` embeds a placeholder page with status 200.
/// Tests inject a temporary directory so no build step is needed.
pub enum StaticSource {
    Embedded,
    Dir(PathBuf),
}

/// Server configuration. Every zero-ish value has a safe default except
/// `data_dir`, which is required: all state lives under it.
pub struct Options {
    pub addr: String,
    pub data_dir: PathBuf,
    pub version: String,
    /// Empty means "load or create <dataDir>/token".
    pub token: String,
    /// `None` means no runner: `POST /messages` answers 503, which is how the
    /// HTTP conformance corpus exercises the surface without a model.
    pub new_runner: Option<Arc<dyn Fn(&str) -> Arc<dyn TurnRunner> + Send + Sync>>,
    pub static_source: StaticSource,
    pub poll_interval: Duration,
    pub ping_interval: Duration,
    pub delta_interval: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:0".into(),
            data_dir: PathBuf::new(),
            version: "0.0.0-dev".into(),
            token: String::new(),
            new_runner: None,
            static_source: StaticSource::Embedded,
            poll_interval: Duration::from_millis(25),
            ping_interval: Duration::from_secs(30),
            delta_interval: Duration::from_millis(50),
        }
    }
}

/// The one writer of every session log served by this process. Two writers
/// would assign the same `seq` twice; the runner uses this handle instead of
/// opening the log itself.
pub trait EventWriter: Send + Sync {
    fn append(&self, session_id: &str, event: &Event) -> Result<u64>;
    fn last_seq(&self, session_id: &str) -> u64;
    fn path(&self, session_id: &str) -> PathBuf;
}
