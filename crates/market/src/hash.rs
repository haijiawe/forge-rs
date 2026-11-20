//! SHA-256 in the one form this crate needs: a lowercase hex string.
//!
//! The function itself lives in `hearth-core` now, because it is no longer only
//! a market concern: a `file/write` record states the digest of the bytes it
//! saved, and a reader anywhere is invited to check it against the file. Two
//! implementations of the same wire-format value is exactly the second truth
//! `hearth-core` exists to prevent, so this module keeps only the parts that are
//! specific to how a market compares digests.

pub use hearth_core::digest::sha256_hex;

/// A streaming digest, for a file too large to hold twice in memory.
///
/// Why not just read the file: the caller that digests a recorded file also
/// wants its length, and reading a 200 MB file into a `Vec` to hash it is the
/// kind of allocation a market should not need.
pub type Sha256 = hearth_core::Sha256;

/// Is this 64 lowercase hex characters? The index's `sha256` is compared as
/// text, so its shape is checked as text.
pub fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_digest_is_the_well_known_one() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_streaming_form_matches_the_one_shot_form() {
        let mut context = Sha256::new();
        context.update(b"abc");
        context.update(b"def");
        assert_eq!(context.finish(), sha256_hex(b"abcdef"));
    }

    #[test]
    fn hex_shape_is_checked_not_guessed() {
        assert!(is_sha256_hex(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_sha256_hex(
            "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_sha256_hex("deadbeef"));
    }
}
