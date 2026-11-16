//! Delegated child agents (port of `archive/forge-go/internal/subagent` M5 U3).
//!
//! Layer L2. A `Runner` exposes one `subagent` tool; calling it opens a fresh
//! child session log under `<data_dir>/sessions/<fresh-id>.jsonl`, runs one
//! `forge_agent::Agent` turn there, and returns the child's last non-empty
//! assistant message. The event log is the only truth: final text and turn
//! counts are read back from the log, never taken from the aggregate result.
//!
//! No async runtime. Cancellation is an `Arc<AtomicBool>` carried by
//! [`Canceller`]; the agent polls it at stream boundaries, and the serial
//! semaphore polls it while a call is queued.

#![forbid(unsafe_code)]

pub mod child;
pub mod options;
pub mod runner;
pub mod tool;

pub use child::{child_log_path, format_stamp, ChildRun};
pub use options::{
    Canceller, Clock, Logger, NoopLogger, NowFn, Options, DEFAULT_MAX_DEPTH, DEFAULT_MAX_STEPS,
};
pub use runner::{CallContext, Runner};
pub use tool::{spec, tool_schema, DelegateArgs, CODE_DEPTH_EXCEEDED, CODE_FAILED, TOOL_NAME};
