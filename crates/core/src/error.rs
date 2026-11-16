//! One error type, because the caller of a harness operation is either a
//! program that wants to branch on what went wrong or a human reading one line.
//!
//! There is no `thiserror` here on purpose: the enum is small, the impls are
//! mechanical, and a proc-macro crate is a compile-time tax paid by every
//! build to save twenty lines.

use std::fmt;

/// What kind of failure this is, in the vocabulary the CLI and the HTTP error
/// envelope already speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The operating system said no.
    Io,
    /// Bytes were not what they claimed to be.
    Json,
    /// The shape was not the frozen shape.
    Invalid,
    /// A log written by a version this build does not know.
    UnsupportedVersion,
    /// Records arrived out of order, or one is missing.
    NotDense,
    /// Two records share an id.
    DuplicateId,
    /// The thing named does not exist.
    NotFound,
    /// The thing named exists and the request contradicts it.
    Conflict,
    /// A caller passed something the callee refuses to guess at.
    Refused,
}

impl ErrorKind {
    /// The stable string form. Used in payloads, so it must not drift.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Io => "io",
            ErrorKind::Json => "json",
            ErrorKind::Invalid => "invalid",
            ErrorKind::UnsupportedVersion => "unsupported_version",
            ErrorKind::NotDense => "not_dense",
            ErrorKind::DuplicateId => "duplicate_id",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Refused => "refused",
        }
    }
}

/// An error with a kind, a message, and optionally the thing that caused it.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Attach the underlying cause. The message stays the account a human
    /// reads; the source is what a debugger walks.
    pub fn with_source(
        mut self,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn is(&self, kind: ErrorKind) -> bool {
        self.kind == kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|b| b.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::new(ErrorKind::Io, format!("io: {e}")).with_source(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::new(ErrorKind::Json, format!("invalid JSON: {e}")).with_source(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
