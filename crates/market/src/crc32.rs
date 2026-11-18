//! CRC-32 (the ISO-HDLC polynomial gzip uses in its footer).
//!
//! Why hand-written: `crc32fast` is not in the dependency budget, and the
//! algorithm is fifteen lines. It is also the check that catches a truncated or
//! corrupted gzip member, so it earns its place next to the decompressor rather
//! than being skipped as "the digest already covers it" — the digest covers the
//! *compressed* bytes the index names, this covers what the tarball claims about
//! itself, and a test that only had one of the two could not tell which broke.

/// The CRC-32 table, built once per process at first use.
fn table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xedb8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *slot = value;
        }
        table
    })
}

/// The CRC-32 of `bytes`, as gzip writes it in its footer (little-endian there).
pub fn crc32(bytes: &[u8]) -> u32 {
    let table = table();
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc = table[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

/// CRC-16/ARC, which gzip's `FHCRC` header field uses (`crc32(...) & 0xffff`).
pub fn crc16_from_crc32(bytes: &[u8]) -> u16 {
    (crc32(bytes) & 0xffff) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check value every CRC-32 implementation is pinned against.
    #[test]
    fn the_check_value_of_123456789_is_pinned() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn the_empty_input_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    /// gzip's FHCRC is the low 16 bits of the running CRC of the header bytes,
    /// which for this input is a value a wrong polynomial would miss.
    #[test]
    fn the_header_crc_is_the_low_half() {
        assert_eq!(crc16_from_crc32(b"123456789"), 0x3926);
    }
}
