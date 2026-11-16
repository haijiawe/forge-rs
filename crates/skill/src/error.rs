//! The crate's small error surface.
//!
//! `Scan` never fails for filesystem problems: those become entries in
//! [`Set::skipped`] and [`Set::warnings`]. Errors are used only for the
//! impossible-to-recover cases and for [`Skill::load`] failures.

use std::fmt;

/// Error categories shared by the public API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The skill has no path to load from.
    NoPath,
    /// A file could not be opened, read, listed or inspected.
    Io,
    /// Frontmatter could not be parsed.
    InvalidData,
}

/// A stable, printable error for this crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    /// Build an error from a category and a ready-to-print message.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The category of this error.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The full message, including any path context.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Convenience alias returned by every fallible operation in this crate.
pub type Result<T> = std::result::Result<T, Error>;
