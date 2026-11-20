//! `hearth-session`: the append-only log and the views built from it.
//!
//! The log is the only truth. Everything else in this crate is a pure function
//! of a prefix of that log: [`projection`] folds it into a snapshot, [`fork`]
//! copies a completed prefix into a new session, [`decision`] follows one log to
//! answer one approval, and [`compact`] decides how much history one summary
//! record may stand in for. None of them keeps state that a restart would lose,
//! and none of them writes anywhere except the log it owns.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod blobs;
pub mod compact;
pub mod decision;
pub mod fork;
pub mod projection;
pub mod store;

pub use blobs::{is_valid_name, Blobs};
pub use compact::{
    compact, compact_log, is_already_compacted, is_empty_summary, is_invalid_log, is_invalid_plan,
    last_compacted_span, open_compaction, plan_compaction, CompactPlan, Summarizer,
    TruncatingSummarizer, DEFAULT_KEEP_TURNS, DEFAULT_SUMMARY_BYTES, ERR_ALREADY_COMPACTED,
    ERR_EMPTY_SUMMARY, ERR_INVALID_LOG, ERR_INVALID_PLAN,
};
pub use decision::{Decision, DecisionWatcher};
pub use fork::{fork, ForkOptions, ForkResult};
pub use projection::{
    Approval, AssistantCall, Counters, Message, Projection, Snapshot, ToolCall, Turn,
};
pub use store::{Options, Store};

// The envelope is shared vocabulary, not this crate's type: re-export it so a
// caller can use one import for the store and the records it reads.
pub use hearth_core::{Error, ErrorKind, Event, EventType, Result, SessionId};
