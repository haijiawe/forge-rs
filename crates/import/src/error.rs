//! Error sentinels for the importer.
//!
//! `forge_core::Error` is the only error type in the workspace, so sentinels
//! are a `(kind, message tag)` pair: callers check `is_*` (or compare
//! `err.kind()` and `err.message()`). Tags are plain user-facing words — no
//! internal package names ever appear in an error from this crate.

use forge_core::{Error, ErrorKind};

/// The target session log already exists; `Write` never overwrites.
pub const ERR_EXISTS: &str = "target session log already exists";
/// The newest old generation is zstd-compressed; decoding is out of scope.
pub const ERR_COMPRESSED: &str = "old session log is zstd-compressed";
/// The directory passed to `Detect` is missing, unreadable, or not a directory.
pub const ERR_NO_DIR: &str = "no such directory";
/// The directory holds no canonical old session log.
pub const ERR_NO_SESSION: &str = "no old-format session log found";
/// The first row is not a usable old header.
pub const ERR_BAD_HEADER: &str = "malformed old session header";
/// An event row is corrupt.
pub const ERR_BAD_LOG: &str = "malformed old session record";
/// The old header claims a generation newer than this converter speaks.
pub const ERR_UNSUPPORTED_VERSION: &str = "unsupported old session format version";
/// The plan no longer describes the source; refuse rather than merge.
pub const ERR_PLAN_STALE: &str = "plan does not match the source";

// PascalCase aliases mirror the Go sentinels named in the contract. Rust's
// `non_upper_case_globals` is silenced locally; the idiomatic names above are
// preferred in new code.
#[allow(non_upper_case_globals)]
pub const ErrExists: &str = ERR_EXISTS;
#[allow(non_upper_case_globals)]
pub const ErrCompressed: &str = ERR_COMPRESSED;
#[allow(non_upper_case_globals)]
pub const ErrNoDir: &str = ERR_NO_DIR;

/// True when `err` means the target log already exists.
pub fn is_exists(err: &Error) -> bool {
    err.is(ErrorKind::Conflict) && err.message().starts_with(ERR_EXISTS)
}

/// True when `err` means the old log is zstd-compressed.
pub fn is_compressed(err: &Error) -> bool {
    err.is(ErrorKind::Invalid) && err.message().starts_with(ERR_COMPRESSED)
}

/// True when `err` means the source directory is missing or unusable.
pub fn is_no_dir(err: &Error) -> bool {
    err.is(ErrorKind::NotFound) && err.message().starts_with(ERR_NO_DIR)
}

/// True when `err` means no old session log was found.
pub fn is_no_session(err: &Error) -> bool {
    err.is(ErrorKind::NotFound) && err.message().starts_with(ERR_NO_SESSION)
}

/// True when `err` means the old header is malformed.
pub fn is_bad_header(err: &Error) -> bool {
    err.is(ErrorKind::Invalid) && err.message().starts_with(ERR_BAD_HEADER)
}

/// True when `err` means an old event row is malformed.
pub fn is_bad_log(err: &Error) -> bool {
    err.is(ErrorKind::Invalid) && err.message().starts_with(ERR_BAD_LOG)
}

/// True when `err` means the old format is too new for this converter.
pub fn is_unsupported_version(err: &Error) -> bool {
    err.is(ErrorKind::UnsupportedVersion)
}

/// True when `err` means a plan became stale between `MakePlan` and `Write`.
pub fn is_plan_stale(err: &Error) -> bool {
    err.is(ErrorKind::Conflict) && err.message().starts_with(ERR_PLAN_STALE)
}

pub(crate) fn err(kind: ErrorKind, tag: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(kind, format!("{tag}: {detail}"))
}

/// Append a physical location without moving the sentinel tag out of first
/// position, so the `is_*` predicates keep working on wrapped errors.
pub(crate) fn with_location(err: Error, path: &std::path::Path, line: usize) -> Error {
    Error::new(
        err.kind(),
        format!("{} at {}:{}", err.message(), path.display(), line),
    )
}
