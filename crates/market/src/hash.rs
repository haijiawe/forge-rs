//! SHA-256 in the one form this crate needs: a lowercase hex string.
//!
//! `ring` is already in the dependency budget because `rustls` needs a digest,
//! so `sha2` would put a second implementation of the same function in the same
//! binary. The wrapper exists because every comparison in this crate is between
//! hex strings read from a JSON document and hex strings produced here, and a
//! byte-array comparison would need its own formatting rule anyway.

use ring::digest::{Context, SHA256};

/// The digest of `bytes`, lowercase hex, 64 characters.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&SHA256, bytes);
    hex_lower(digest.as_ref())
}

/// A streaming digest, for a file too large to hold twice in memory.
///
/// Why not just read the file: the caller that digests a recorded file also
/// wants its length, and reading a 200 MB file into a `Vec` to hash it is the
/// kind of allocation a market should not need.
pub struct Sha256(Context);

impl Sha256 {
    pub fn new() -> Self {
        Sha256(Context::new(&SHA256))
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub fn finish(self) -> String {
        hex_lower(self.0.finish().as_ref())
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Sha256::new()
    }
}

/// Lowercase hex, two characters per byte.
pub fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: [u8; 16] = *b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

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
