//! Names for things.
//!
//! An id is opaque in the log and meaningful to the operator: sixteen random
//! bytes in lower-case hex, so a log line is greppable and an id cannot be
//! mistaken for a sequence number. The alternative — a timestamp plus a
//! counter — would leak the order of events into the id and tempt a reader into
//! trusting it instead of `seq`.

use crate::error::{Error, ErrorKind, Result};

/// 16 random bytes, lower-case hex. 32 characters, always.
///
/// Entropy comes from the OS CSPRNG. A non-cryptographic generator here would
/// be a session id collision that two machines both believe is unique.
pub fn new_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::new(ErrorKind::Io, format!("ids: no OS entropy available: {e}")))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// A session id, checked once at the boundary.
///
/// The rule is the one `hearth chat --session` enforces: it becomes a file name
/// (`<dataDir>/sessions/<id>.jsonl`), so it may not contain a separator, may
/// not be empty, and may not start with a character a shell would eat.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub const MAX_LEN: usize = 128;

    pub fn parse(raw: impl Into<String>) -> Result<SessionId> {
        let raw = raw.into();
        let bytes = raw.as_bytes();
        if bytes.is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "session: empty id"));
        }
        if bytes.len() > SessionId::MAX_LEN {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "session: id is {} bytes, the limit is {}",
                    bytes.len(),
                    SessionId::MAX_LEN
                ),
            ));
        }
        let first = bytes[0];
        if !first.is_ascii_alphanumeric() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("session: id must start with a letter or digit: {raw:?}"),
            ));
        }
        for &b in &bytes[1..] {
            if !(b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-') {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "session: id may only contain letters, digits, dot, underscore and dash: {raw:?}"
                    ),
                ));
            }
        }
        Ok(SessionId(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_thirty_two_hex_characters_and_unique() {
        // Uniqueness over a handful is not a proof, but a generator that
        // repeats within a thousand draws is broken beyond argument.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let id = new_id().unwrap();
            assert_eq!(id.len(), 32, "id {id} is not 32 characters");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "id {id} is not lower-case hex"
            );
            assert!(
                seen.insert(id),
                "the generator repeated an id within 1000 draws"
            );
        }
    }

    #[test]
    fn session_ids_accept_what_the_cli_documents() {
        for ok in ["s1", "20261116-073441-adceff", "a.b_c-d", "A1"] {
            assert!(SessionId::parse(ok).is_ok(), "{ok} should be accepted");
        }
        for bad in [
            "",
            "-leading",
            ".dot",
            "has space",
            "has/slash",
            "has\\slash",
            "ünicode",
        ] {
            assert!(SessionId::parse(bad).is_err(), "{bad} should be refused");
        }
        assert!(SessionId::parse("x".repeat(SessionId::MAX_LEN + 1)).is_err());
    }
}
