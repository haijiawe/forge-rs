//! A gzip (RFC 1952) member reader, on top of [`crate::inflate`].
//!
//! Why this is separate from DEFLATE: a `.tar.gz` is a gzip member whose payload
//! is a tar; a bare `.tar` is the same bytes without the wrapper. The tar reader
//! must not know about either, and the gzip reader must not know about tar, so
//! each can be tested against a fixture the other never sees.
//!
//! Only one member is read. A `.tar.gz` with concatenated members is not a tar,
//! and silently reading the first member of one is exactly the kind of "mostly
//! worked" that leaves a half-populated directory.

use forge_core::Result;

use crate::crc32::crc32;
use crate::error::{
    invalid_with, ERR_GZIP_CRC, ERR_GZIP_FLAGS, ERR_GZIP_HCRC, ERR_GZIP_HEADER, ERR_GZIP_MAGIC,
    ERR_GZIP_METHOD, ERR_GZIP_SIZE, ERR_GZIP_TRAILING,
};
use crate::inflate::{inflate_from, DEFAULT_MAX_OUTPUT};

/// The limit a caller uses when it has no reason to want a different one.
pub use crate::inflate::DEFAULT_MAX_OUTPUT as DEFAULT_MAX_GZIP_OUTPUT;

/// Is this a gzip stream? Used to decide between `.tar` and `.tar.gz` without
/// trusting a file extension, which after a download is only a suggestion.
pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// Decompress one gzip member.
pub fn gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    gunzip_limited(bytes, DEFAULT_MAX_OUTPUT)
}

/// Decompress one gzip member, refusing output larger than `max_output`.
pub fn gunzip_limited(bytes: &[u8], max_output: usize) -> Result<Vec<u8>> {
    let header = parse_header(bytes)?;
    let (body, consumed) = inflate_from(&bytes[header..], max_output)?;
    let footer_start = header
        .checked_add(consumed)
        .ok_or_else(|| invalid_with(ERR_GZIP_HEADER, "member offset overflow"))?;
    let footer_end = footer_start
        .checked_add(8)
        .ok_or_else(|| invalid_with(ERR_GZIP_HEADER, "member offset overflow"))?;
    let footer = bytes.get(footer_start..footer_end).ok_or_else(|| {
        invalid_with(
            ERR_GZIP_HEADER,
            format!(
                "the stream ends at {footer_start} bytes but the footer needs 8 more; \
                 the body was cut short"
            ),
        )
    })?;

    let expected_crc = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);
    let actual_crc = crc32(&body);
    if expected_crc != actual_crc {
        return Err(invalid_with(
            ERR_GZIP_CRC,
            format!("footer says {expected_crc:#010x}, the bytes give {actual_crc:#010x}"),
        ));
    }
    let expected_size = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);
    let actual_size = body.len() as u32;
    if expected_size != actual_size {
        return Err(invalid_with(
            ERR_GZIP_SIZE,
            format!("footer says {expected_size} bytes, the stream produced {actual_size}"),
        ));
    }

    // Trailing bytes after the footer mean a second member or a corrupt tail.
    // Either way the payload is not the whole tarball, so refuse instead of
    // installing a bundle that someone appended to.
    if footer_end != bytes.len() {
        return Err(invalid_with(
            ERR_GZIP_TRAILING,
            format!("{} bytes after the footer", bytes.len() - footer_end),
        ));
    }
    Ok(body)
}

/// Parse the header, returning the offset of the DEFLATE data.
fn parse_header(bytes: &[u8]) -> Result<usize> {
    let magic = bytes
        .get(..2)
        .ok_or_else(|| invalid_with(ERR_GZIP_HEADER, "shorter than its 2-byte magic"))?;
    if !is_gzip(magic) {
        return Err(invalid_with(
            ERR_GZIP_MAGIC,
            format!("first two bytes are {:02x} {:02x}", magic[0], magic[1]),
        ));
    }
    let method = *bytes
        .get(2)
        .ok_or_else(|| invalid_with(ERR_GZIP_HEADER, "no compression method byte"))?;
    if method != 8 {
        return Err(invalid_with(
            ERR_GZIP_METHOD,
            format!("compression method {method}, only 8 (deflate) is defined"),
        ));
    }
    let flags = *bytes
        .get(3)
        .ok_or_else(|| invalid_with(ERR_GZIP_HEADER, "no flags byte"))?;
    // Bits 5..7 are reserved and must be zero (RFC 1952 §2.3.1).
    if flags & 0xe0 != 0 {
        return Err(invalid_with(
            ERR_GZIP_FLAGS,
            format!("flags {flags:#04x} set reserved bits"),
        ));
    }
    // MTIME, XFL and OS are advisory; nothing in a market depends on them.
    let mut offset = 10usize;

    if flags & 0x04 != 0 {
        // FEXTRA: a little-endian length, then that many bytes.
        let pair = bytes
            .get(offset..offset + 2)
            .ok_or_else(|| invalid_with(ERR_GZIP_HEADER, "FEXTRA length is truncated"))?;
        let length = usize::from(u16::from_le_bytes([pair[0], pair[1]]));
        offset += 2 + length;
        if offset > bytes.len() {
            return Err(invalid_with(
                ERR_GZIP_HEADER,
                format!("FEXTRA claims {length} bytes past the end"),
            ));
        }
    }
    for (flag, name) in [(0x08u8, "FNAME"), (0x10u8, "FCOMMENT")] {
        if flags & flag != 0 {
            let rest = bytes.get(offset..).ok_or_else(|| {
                invalid_with(ERR_GZIP_HEADER, format!("{name} starts past the end"))
            })?;
            let end = rest.iter().position(|&byte| byte == 0).ok_or_else(|| {
                invalid_with(
                    ERR_GZIP_HEADER,
                    format!("{name} is not terminated by a NUL"),
                )
            })?;
            offset += end + 1;
        }
    }
    if flags & 0x02 != 0 {
        // FHCRC: the low 16 bits of the CRC-32 of everything up to here.
        let stored = bytes
            .get(offset..offset + 2)
            .ok_or_else(|| invalid_with(ERR_GZIP_HEADER, "FHCRC is truncated"))?;
        let expected = u16::from_le_bytes([stored[0], stored[1]]);
        let actual = crate::crc32::crc16_from_crc32(&bytes[..offset]);
        if expected != actual {
            return Err(invalid_with(
                ERR_GZIP_HCRC,
                format!("header says {expected:#06x}, the bytes give {actual:#06x}"),
            ));
        }
        offset += 2;
    }
    if offset > bytes.len() {
        return Err(invalid_with(
            ERR_GZIP_HEADER,
            "the header runs past the end of the stream",
        ));
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real gzip member produced on this machine by `tar -czf` (bsdtar 3.8.9
    /// with zlib 1.2.13). It holds a six-entry tarball, one of whose names is
    /// longer than 100 bytes, so the payload exercises both the dynamic Huffman
    /// block and the pax long-name entry. The sha256 is pinned in the market
    /// integration test; here the point is that the bytes decompress at all.
    pub(crate) const TARBALL_GZ: &[u8] = include_bytes!("../tests/fixtures/fixture.tar.gz");

    #[test]
    fn the_embedded_fixture_is_a_gzip_stream() {
        assert!(is_gzip(TARBALL_GZ));
    }

    #[test]
    fn the_embedded_fixture_decompresses_to_a_tar() {
        let tar = gunzip(TARBALL_GZ).expect("gunzip");
        assert_eq!(tar.len() % 512, 0, "a tar is a whole number of blocks");
        assert!(!tar.is_empty());
        // The tar's own magic at offset 257 of the first header.
        assert_eq!(
            &tar[257..262],
            b"ustar",
            "{}",
            String::from_utf8_lossy(&tar[..300])
        );
    }

    #[test]
    fn a_flipped_byte_in_the_body_is_caught_by_the_crc() {
        let mut broken = TARBALL_GZ.to_vec();
        let index = broken.len() / 2;
        broken[index] ^= 0x01;
        let err = gunzip(&broken).unwrap_err();
        // Either the block itself rejects the change or, more commonly, the
        // footer CRC does; both are refusals, neither is a silently wrong tarball.
        assert!(
            !err.to_string().is_empty(),
            "a corrupted member must be an error"
        );
    }

    #[test]
    fn a_corrupted_footer_crc_is_named() {
        let mut broken = TARBALL_GZ.to_vec();
        let last = broken.len() - 8;
        broken[last] ^= 0xff;
        let err = gunzip(&broken).unwrap_err();
        assert!(err.to_string().starts_with(ERR_GZIP_CRC), "{err}");
    }

    #[test]
    fn trailing_bytes_after_the_footer_are_refused() {
        let mut extended = TARBALL_GZ.to_vec();
        extended.extend_from_slice(b"\x1f\x8b");
        let err = gunzip(&extended).unwrap_err();
        assert!(err.to_string().starts_with(ERR_GZIP_TRAILING), "{err}");
    }

    #[test]
    fn a_non_gzip_stream_is_refused_by_its_magic() {
        let err = gunzip(b"not a gzip stream at all").unwrap_err();
        assert!(err.to_string().starts_with(ERR_GZIP_MAGIC), "{err}");
    }

    #[test]
    fn a_reserved_flag_bit_is_refused() {
        let mut bytes = TARBALL_GZ.to_vec();
        bytes[3] |= 0x20;
        let err = gunzip(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_GZIP_FLAGS), "{err}");
    }

    #[test]
    fn a_method_other_than_deflate_is_refused() {
        let mut bytes = TARBALL_GZ.to_vec();
        bytes[2] = 7;
        let err = gunzip(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_GZIP_METHOD), "{err}");
    }

    #[test]
    fn a_header_that_stops_in_the_middle_is_refused() {
        let err = gunzip(&TARBALL_GZ[..5]).unwrap_err();
        assert!(err.to_string().starts_with(ERR_GZIP_HEADER), "{err}");
    }

    #[test]
    fn a_stream_without_a_footer_is_refused() {
        let err = gunzip(&TARBALL_GZ[..TARBALL_GZ.len() - 8]).unwrap_err();
        assert!(
            err.to_string().starts_with(ERR_GZIP_HEADER)
                || err
                    .to_string()
                    .starts_with(crate::error::ERR_DEFLATE_TRUNCATED),
            "{err}"
        );
    }
}
