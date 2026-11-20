//! The crate's one error type, plus the stable codes a CLI branches on.
//!
//! Why an enum of refusal kinds instead of a free-form message: `doctor` and
//! `hearth expert list` have to answer "is the roster usable, and if not, what do
//! I fix" without a human reading prose. Each kind maps to one fixed string, so
//! a script can match it and a doc can list it. The message beside the code is
//! the sentence a human reads, and every one of them ends by naming the fix.
//!
//! There is no `anyhow` here and no `Box<dyn Error>`: the only failures this
//! crate has are "this file is not a definition I can trust", and a caller that
//! wants to ignore them must ignore them by name, not by unwrapping.

use std::fmt;

/// What kind of refusal this is. The variants are a closed set: a roster file
/// either parses and validates, or it is refused for one of these reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorKind {
    /// The `---` fence is missing, so the file is not a definition at all.
    MissingFrontmatter,
    /// The frontmatter exists but is not the shape this reader accepts.
    BadFrontmatter,
    /// A required key is absent or empty.
    MissingField,
    /// `slug` does not match the roster's slug grammar.
    BadSlug,
    /// Two files define the same slug.
    DuplicateSlug,
    /// A team names an expert that the roster does not define.
    UnknownMember,
    /// A team names an expert that exists but has `enabled: false`.
    DisabledMember,
    /// A team has fewer than two members.
    TooFewMembers,
    /// The bytes could not be taken: I/O, size cap, or not UTF-8.
    Unreadable,
}

impl ErrorKind {
    /// The stable code, for machines. Never reworded; a CLI flag, a test and a
    /// fire-alarm query all match on these exact strings.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::MissingFrontmatter => "missing_frontmatter",
            ErrorKind::BadFrontmatter => "bad_frontmatter",
            ErrorKind::MissingField => "missing_field",
            ErrorKind::BadSlug => "bad_slug",
            ErrorKind::DuplicateSlug => "duplicate_slug",
            ErrorKind::UnknownMember => "unknown_member",
            ErrorKind::DisabledMember => "disabled_member",
            ErrorKind::TooFewMembers => "too_few_members",
            ErrorKind::Unreadable => "unreadable",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One refusal, with the file it came from already spelled into the message.
///
/// The message always names the path (when there is one) and the fix, because
/// this string is what `hearth doctor` prints and a roster is edited by hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    /// Build a refusal from a kind and a ready-to-print sentence.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
        }
    }

    /// The refusal kind, for callers that want to match on the enum.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The stable code of [`Error::kind`], for callers that want the string the
    /// CLI prints. Same value as `error.kind().as_str()`.
    pub fn code(&self) -> &'static str {
        self.kind.as_str()
    }

    /// The human sentence, path and fix included.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    /// `code: message`, so a bare `eprintln!("{err}")` is already greppable by
    /// code and still readable as a sentence.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for Error {}

/// Convenience alias for the crate's fallible operations.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_the_documented_strings() {
        let cases = [
            (ErrorKind::MissingFrontmatter, "missing_frontmatter"),
            (ErrorKind::BadFrontmatter, "bad_frontmatter"),
            (ErrorKind::MissingField, "missing_field"),
            (ErrorKind::BadSlug, "bad_slug"),
            (ErrorKind::DuplicateSlug, "duplicate_slug"),
            (ErrorKind::UnknownMember, "unknown_member"),
            (ErrorKind::DisabledMember, "disabled_member"),
            (ErrorKind::TooFewMembers, "too_few_members"),
            (ErrorKind::Unreadable, "unreadable"),
        ];
        for (kind, code) in cases {
            assert_eq!(kind.as_str(), code);
            let error = Error::new(kind, "detail");
            assert_eq!(error.code(), code);
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), format!("{code}: detail"));
        }
    }
}
