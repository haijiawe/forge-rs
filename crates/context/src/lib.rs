//! Addressable compression: blocks the model can name, restore and search.
//!
//! Compaction already exists in [`hearth_session`], and it is one-way: the oldest
//! complete history is replaced by a summary and never comes back. That is the
//! right shape for "the window is full", and the wrong shape for "I need to stop
//! paying for these forty messages, but I may need one of them again".
//!
//! This crate holds the second shape. A *block* names an inclusive `seq` range
//! of the log, carries a summary the model reads instead, and has an id the
//! model can hand back to restore it. The log stays append-only: creating a
//! block appends one `context/compressed` event, and restoring one is a read.
//!
//! Three rules, all of them consequences of the log being the only truth:
//!
//! 1. **Address ranges, not slices.** A block stores the seq numbers it hides.
//!    Seq numbers survive replay, re-forking and a different reader; a message
//!    index does not survive any of the three.
//! 2. **One address per range.** Two blocks may not overlap. Overlap would mean
//!    the same message is hidden twice, and "restore" would have two answers.
//! 3. **A block never hides a live edge.** The range must be closed by the log
//!    that contains it, which the writer checks, not this crate.
//!
//! Nothing here reads files, starts threads or talks to a model. Search is over
//! the summaries in the ledger; restoring the originals is the caller's job,
//! because the caller is the one holding the log.

#![forbid(unsafe_code)]

pub mod block;
pub mod ledger;
pub mod prune;
pub mod wire;

pub use block::{
    is_bad_block, is_block_too_large, is_empty_summary, is_overlap, Block, ERR_BAD_BLOCK,
    ERR_BLOCK_TOO_LARGE, ERR_EMPTY_SUMMARY, ERR_OVERLAP, MAX_SUMMARY_BYTES,
};
pub use ledger::{Hit, Ledger};
pub use prune::{prune_tool_outputs, ToolOutput, Trim};
pub use wire::{
    context_compressed, context_compressed_with_surface, decode_surface_op, parse_block,
    parse_record, parse_surface_op, CompressedRecord, SurfaceOp, EVENT_TYPE, SURFACE_REPLACE,
};
