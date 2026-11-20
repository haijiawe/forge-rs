//! A change detector, not a security boundary.
//!
//! The agent needs to answer one question without storing a second copy of the
//! thing it is asking about: "is the input to this request the same as the
//! input to the last one?" A fingerprint answers it in sixteen characters
//! instead of duplicating an eight-kilobyte system prompt on every step.
//!
//! FNV-1a 64-bit, deliberately. Not SHA-256, because this is not defending
//! anything: an adversary who can choose the bytes already writes the log. The
//! property that matters here is that a one-character edit changes the digest,
//! and 2^-64 is far below the odds of the disk corrupting a line first.
//!
//! It lives in `forge-core` because the alternative is a second definition in
//! whichever crate needed it next, and two fingerprints of the same bytes that
//! do not agree are worse than none.

/// FNV-1a over the bytes, as sixteen lowercase hex characters.
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

#[cfg(test)]
mod tests {
    use super::fingerprint;

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
}
