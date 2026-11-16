//! `forge-session`: the append-only log and the views built from it.
//!
//! The log is the only truth. Everything else in this crate is a pure function
//! of a prefix of that log: [`projection`] folds it into a snapshot, [`fork`]
//! copies a completed prefix into a new session, and [`decision`] follows one
//! log to answer one approval. None of them keeps state that a restart would
//! lose, and none of them writes anywhere except the log it owns.
//!
//! Compaction is deliberately not implemented here yet. The projection already
//! understands `compaction/summary` records (they hide earlier messages without
//! rewriting counters), but the writer that produces them belongs to a later
//! wave; inventing one now would create a second source of truth.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod decision;
pub mod fork;
pub mod projection;
pub mod store;

pub use decision::{Decision, DecisionWatcher};
pub use fork::{fork, ForkOptions, ForkResult};
pub use projection::{
    Approval, AssistantCall, Counters, Message, Projection, Snapshot, ToolCall, Turn,
};
pub use store::{Options, Store};

// The envelope is shared vocabulary, not this crate's type: re-export it so a
// caller can use one import for the store and the records it reads.
pub use forge_core::{Error, ErrorKind, Event, EventType, Result, SessionId};
