//! One-way import of retired Hearth session logs (session-format v0..v4) into
//! this build's v1 event log.
//!
//! Three steps, only the last of which has a side effect: `detect` reads the
//! old directory, `make_plan` maps without writing, and `write` commits a
//! new log under `<data_dir>/sessions/<id>.jsonl`.
//!
//! The old format is never read at runtime: this crate is the only place that
//! knows its field names, and its product is a v1 log the session reader folds
//! without any knowledge of where it came from.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod detect;
pub mod error;
pub(crate) mod jsonx;
pub mod mapping;
pub mod write;

pub use detect::{detect, Detect, Source};
pub use error::{
    is_bad_header, is_bad_log, is_compressed, is_exists, is_no_dir, is_no_session, is_plan_stale,
    is_unsupported_version, ErrCompressed, ErrExists, ErrNoDir, ERR_BAD_HEADER, ERR_BAD_LOG,
    ERR_COMPRESSED, ERR_EXISTS, ERR_NO_DIR, ERR_NO_SESSION, ERR_PLAN_STALE,
    ERR_UNSUPPORTED_VERSION,
};
pub use mapping::{make_plan, MakePlan, Plan};
pub use write::{write, Write};
