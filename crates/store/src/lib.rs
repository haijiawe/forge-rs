//! A rebuildable SQLite index over the append-only session logs.
//!
//! This crate is the Rust port of Go's `internal/store` (M4 U1). The log remains
//! the only source of truth: every row here can be thrown away and rebuilt with
//! [`Store::rebuild`]. Keep that property when changing anything in this crate.
//!
//! The public surface mirrors `docs/contracts/m4-surface.md` §2 with snake_case
//! names. Errors are `hearth_core::Error`; see `crates/core/README.md` for the
//! error vocabulary. Timestamps are i64 milliseconds since the Unix epoch.

mod cursor;
mod rebuild;
mod schema;
mod search;
mod store;

pub use store::{is_bad_cursor, is_closed, Hit, Options, Query, Session, Stats, Store};

/// The result type used by every fallible operation in this crate.
pub type Result<T> = std::result::Result<T, hearth_core::Error>;
