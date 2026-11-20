//! Two digests, and the difference between them matters.
//!
//! This module holds both because the alternative is a second definition in
//! whichever crate needed it next, and two digests of the same bytes that do not
//! agree are worse than none.

/// FNV-1a over the bytes, as sixteen lowercase hex characters.
///
/// A change detector, not a security boundary. The agent needs to answer one
/// question without storing a second copy of the thing it is asking about: "is
/// the input to this request the same as the input to the last one?" A
/// fingerprint answers it in sixteen characters instead of duplicating an
/// eight-kilobyte system prompt on every step.
///
/// FNV-1a 64-bit, deliberately. Not SHA-256, because this is not defending
/// anything: an adversary who can choose the bytes already writes the log. The
/// property that matters here is that a one-character edit changes the digest,
/// and 2^-64 is far below the odds of the disk corrupting a line first.
///
/// Stable across versions and platforms: this value is written to logs and
/// compared years later, so it must never change with a compiler upgrade.
pub fn fingerprint(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// SHA-256 over the bytes, as sixty-four lowercase hex characters.
///
/// This one **is** a wire-format value, which is a different job from
/// [`fingerprint`]: a `file/write` record states the digest of the bytes it
/// saved, and a reader on any platform, written in any language, may check it
/// against the file. A non-cryptographic digest would make that check the
/// reader's word against this crate's, and the point of writing it down is that
/// neither has to be trusted. `ring` is already in the binary for rustls, so
/// this costs no new code; `sha2` would have put a second implementation of the
/// same function in the same binary.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    hex_lower(digest.as_ref())
}

/// A streaming SHA-256, for a file too large to hold twice in memory.
///
/// The caller that digests a recorded file also wants its length, and reading a
/// 200 MB file into a `Vec` just to hash it is the kind of allocation a tool
/// should not need.
pub struct Sha256(ring::digest::Context);

impl Sha256 {
    pub fn new() -> Self {
        Sha256(ring::digest::Context::new(&ring::digest::SHA256))
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Finish and return the digest. Consuming `self` keeps a finished context
    /// from being updated again, which ring allows and would silently produce a
    /// digest of the wrong bytes.
    pub fn finish(self) -> String {
        hex_lower(self.0.finish().as_ref())
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{fingerprint, sha256_hex, Sha256};

    #[test]
    fn an_empty_input_has_the_offset_basis() {
        assert_eq!(fingerprint(b""), "cbf29ce484222325");
    }

    #[test]
    fn the_published_fnv_vectors_hold() {
        // Published FNV-1a 64 test vectors. If these ever fail, the function
        // changed and every fingerprint in every existing log became a lie.
        assert_eq!(fingerprint(b"a"), "af63dc4c8601ec8c");
        assert_eq!(fingerprint(b"foobar"), "85944171f73967e8");
    }

    #[test]
    fn one_changed_byte_changes_the_digest() {
        assert_ne!(fingerprint(b"hello"), fingerprint(b"hellp"));
        assert_ne!(fingerprint(b"hello"), fingerprint(b"hello "));
    }

    #[test]
    fn the_output_is_always_sixteen_hex_characters() {
        for input in [b"".as_slice(), b"a", b"a longer prompt with\nnewlines\n"] {
            let out = fingerprint(input);
            assert_eq!(out.len(), 16);
            assert!(out
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        }
    }

    #[test]
    fn the_published_sha256_vectors_hold() {
        // Published NIST vectors. A `file/write` record states a digest that any
        // other implementation is invited to check, so this function is not
        // allowed to be merely self-consistent.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            // Capital T on purpose: this is the published vector for this exact
            // string, and the lowercase sentence has a different digest.
            sha256_hex(b"The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
    }

    #[test]
    fn streaming_matches_the_one_shot_digest() {
        // The streaming form exists for files too large to hold twice; a chunk
        // boundary must not change the answer, or two readers of the same file
        // would disagree about whether it was tampered with.
        let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let mut streaming = Sha256::new();
        for chunk in bytes.chunks(7) {
            streaming.update(chunk);
        }
        assert_eq!(streaming.finish(), sha256_hex(&bytes));
    }

    #[test]
    fn the_two_digests_are_not_interchangeable() {
        // Same bytes, different function, different value: a caller that reaches
        // for the wrong one gets a mismatch rather than a plausible-looking match.
        assert_ne!(fingerprint(b"abc"), sha256_hex(b"abc"));
        assert_eq!(sha256_hex(b"abc").len(), 64);
    }
}
