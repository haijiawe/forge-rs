//! One error type, with a stable code per failure mode.
//!
//! `forge_core::ErrorKind` is deliberately closed — it is the vocabulary the
//! event log and the HTTP envelope already speak, and a plugin failure has no
//! place in it (no event type carries "a plugin timed out"). So this crate has
//! its own small enum whose `code()` is what a human greps for and what the CLI
//! prints. Every variant names a different fix: `bad_manifest` sends the author
//! to `plugin.json`, `timeout` sends them to their own code, `child_exited`
//! sends them to stderr.
//!
//! The vocabulary is exactly the one `PROTOCOL.md` documents. It is a closed
//! set for the same reason the runtime list is: a caller matching on `code()`
//! must be able to enumerate the cases, so a new code is a documented addition
//! with a test, never an ad-hoc string at a call site.

use std::fmt;

/// What went wrong, in the vocabulary of [`Error::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No plugin directory of that name under the host's root.
    UnknownPlugin,
    /// `plugin.json` is missing, unparseable, or violates a stated rule.
    BadManifest,
    /// `runtime` names something this host cannot start.
    UnsupportedRuntime,
    /// The child process could not be started at all — including a manifest
    /// that names no `entry`, where there is no program to start.
    SpawnFailed,
    /// A call arrived before `load` completed.
    NotInitialized,
    /// The bytes were not the protocol: a mismatched id, a missing id, a reply
    /// with neither `result` nor `error`, a wrong `jsonrpc` version.
    ProtocolError,
    /// The child did not answer within the deadline; it has been killed.
    Timeout,
    /// The child is gone. Detected on the next call, never by a watcher thread.
    ChildExited,
    /// A reply exceeded the byte cap. The over-long line is drained, not
    /// buffered, so a runaway plugin cannot exhaust the host's memory.
    TooLarge,
    /// The child answered, and the answer was not what the method returns.
    BadResponse,
    /// The operating system said no.
    Io,
}

impl Kind {
    /// The stable string form. Printed by the CLI and compared by tests, so it
    /// must not drift.
    pub fn code(self) -> &'static str {
        match self {
            Kind::UnknownPlugin => "unknown_plugin",
            Kind::BadManifest => "bad_manifest",
            Kind::UnsupportedRuntime => "unsupported_runtime",
            Kind::SpawnFailed => "spawn_failed",
            Kind::NotInitialized => "not_initialized",
            Kind::ProtocolError => "protocol_error",
            Kind::Timeout => "timeout",
            Kind::ChildExited => "child_exited",
            Kind::TooLarge => "too_large",
            Kind::BadResponse => "bad_response",
            Kind::Io => "io",
        }
    }

    /// Every kind, so a test can prove the vocabulary has no duplicate code.
    pub fn all() -> &'static [Kind] {
        &[
            Kind::UnknownPlugin,
            Kind::BadManifest,
            Kind::UnsupportedRuntime,
            Kind::SpawnFailed,
            Kind::NotInitialized,
            Kind::ProtocolError,
            Kind::Timeout,
            Kind::ChildExited,
            Kind::TooLarge,
            Kind::BadResponse,
            Kind::Io,
        ]
    }
}

/// A failure with a kind and one line of text.
///
/// `message` always says what to do next: "unknown plugin" without the root
/// that was searched sends the operator nowhere. The kind says which party has
/// to act (author, operator, plugin, host); the message says on what.
#[derive(Debug, Clone)]
pub struct Error {
    kind: Kind,
    message: String,
}

impl Error {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "plugin: {}: {}", self.message, self.kind.code())
    }
}

impl std::error::Error for Error {}

// There is deliberately no `From<io::Error>`: an I/O failure is
// `spawn_failed` when starting the child, `child_exited` when the pipe broke,
// and `bad_manifest` when a file could not be read — three different fixes. A
// blanket conversion would force one code onto all three, which is exactly the
// "which of these is my problem" ambiguity the codes exist to remove.
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::new(Kind::BadResponse, error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_distinct_and_stable() {
        let mut codes: Vec<&str> = Kind::all().iter().map(|kind| kind.code()).collect();
        codes.sort_unstable();
        let count = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), count, "two kinds share a code: {codes:?}");
        // The codes are part of the CLI's output and of PROTOCOL.md, so a
        // rename here is a breaking change and has to fail a test.
        assert_eq!(Kind::TooLarge.code(), "too_large");
        assert_eq!(Kind::ProtocolError.code(), "protocol_error");
        assert_eq!(Kind::ChildExited.code(), "child_exited");
        assert_eq!(Kind::BadResponse.code(), "bad_response");
    }

    #[test]
    fn the_display_line_carries_the_code_so_one_line_says_what_to_fix() {
        let error = Error::new(Kind::Timeout, "example: tools/call exceeded 200 ms");
        assert_eq!(
            error.to_string(),
            "plugin: example: tools/call exceeded 200 ms: timeout"
        );
    }
}
