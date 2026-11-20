//! A DEFLATE (RFC 1951) decompressor, written here because the dependency
//! budget has no room for one.
//!
//! Why hand-written rather than a dependency: the budget in the workspace
//! `Cargo.toml` is a decision record, and `flate2` would arrive with a C
//! build step (`miniz_oxide` is pure Rust but `flate2` also drags a second
//! checksum and a second API surface for one function). Decompression is
//! integer-only, has a published test corpus, and is about 400 lines — the
//! kind of code that a project can own, and the kind that is nearly impossible
//! to get silently wrong once it is tested against streams a real encoder
//! produced. The encoder side is *not* written: nothing in the market needs to
//! make a tarball, and a test-only encoder would be untested code shipping in
//! the binary.
//!
//! **What is covered:** all three block types (stored, fixed Huffman, dynamic
//! Huffman), repeat codes 16/17/18 in the code-length alphabet, all 29 length
//! codes and 30 distance codes, and a bounded output buffer. What is *not*
//! accepted: dictionary-augmented streams (RFC 1950 `FDICT` would need a
//! preset dictionary; gzip has no such field and a tar has no use for one) and
//! anything after the final block.
//!
//! Bit order is the one place this is easy to get wrong, so it is stated once
//! and tested: DEFLATE fills bytes least-significant bit first, and a Huffman
//! code is stored most-significant bit first. [`BitReader::read`] walks a code
//! one bit at a time so no reversal is needed anywhere; that is slower than a
//! table-driven decoder and much harder to get wrong, and a plugin tarball is
//! a few hundred kilobytes.

use hearth_core::{Error, Result};

use crate::error::{
    invalid_with, ERR_DEFLATE_CODE_LENGTHS, ERR_DEFLATE_DISTANCE, ERR_DEFLATE_ENDOFBLOCK,
    ERR_DEFLATE_HUFFMAN, ERR_DEFLATE_RESERVED, ERR_DEFLATE_STORED, ERR_DEFLATE_TRUNCATED,
    ERR_OUTPUT_LIMIT,
};

/// The length bases of RFC 1951 §3.2.5, indexed by `code - 257`.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits per length code.
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// The distance bases, indexed by the distance code.
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits per distance code.
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The order code lengths are transmitted in (RFC 1951 §3.2.7).
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// The most a single tarball may decompress to.
///
/// Why a limit at all: a 10 KB gzip stream can expand to gigabytes (a
/// decompression bomb), and an install that fills the disk is indistinguishable
/// from an install that hangs. 64 MiB is two orders of magnitude above a
/// plausible plugin.
pub const DEFAULT_MAX_OUTPUT: usize = 64 << 20;

/// A little-endian bit reader over a byte slice.
struct BitReader<'a> {
    bytes: &'a [u8],
    /// Bit position from the start of `bytes`; bit 0 is the low bit of `bytes[0]`.
    position: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        BitReader { bytes, position: 0 }
    }

    /// Read `count` bits, least significant bit first, as an integer.
    fn read(&mut self, count: u8) -> Result<u32> {
        let mut value = 0u32;
        for index in 0..count {
            let byte = self
                .bytes
                .get(self.position >> 3)
                .ok_or_else(|| invalid_with(ERR_DEFLATE_TRUNCATED, "no more input bits"))?;
            let bit = (byte >> (self.position & 7)) & 1;
            value |= (bit as u32) << index;
            self.position += 1;
        }
        Ok(value)
    }

    /// Discard bits up to the next byte boundary.
    fn align_to_byte(&mut self) {
        self.position = (self.position + 7) & !7;
    }

    /// The bytes from the current (already aligned) position.
    fn aligned_bytes(&self) -> Result<&'a [u8]> {
        self.bytes
            .get(self.position >> 3..)
            .ok_or_else(|| invalid_with(ERR_DEFLATE_TRUNCATED, "stored block header is truncated"))
    }

    fn skip_bytes(&mut self, count: usize) {
        self.position += count * 8;
    }

    /// Number of whole input bytes consumed, rounded up.
    fn bytes_consumed(&self) -> usize {
        self.position.div_ceil(8)
    }
}

/// A canonical Huffman decoder: code lengths in, symbols out.
///
/// The decode walks one bit at a time and compares `(length, code)` against the
/// canonical assignment. A table-based decoder would be several times faster
/// and would need a second representation of the same truth; for tarballs the
/// simple form is the right trade.
#[derive(Debug)]
struct Huffman {
    /// `(code length, code) -> symbol`, sorted by length then code.
    entries: Vec<(u8, u16, u16)>,
    /// Longest code length present, to bound the decode loop.
    max_length: u8,
    /// How many symbols carry a non-zero length.
    symbols: usize,
}

impl Huffman {
    /// Build from a list of code lengths (RFC 1951 §3.2.2).
    ///
    /// A tree with no symbols at all is legal for the distance alphabet of a
    /// block that has no matches; it decodes nothing, and asking it to decode
    /// is a stream error, not a construction error.
    fn new(lengths: &[u8]) -> Result<Self> {
        let mut symbols = 0usize;
        let mut max_length = 0u8;
        for &length in lengths {
            if length > 15 {
                return Err(invalid_with(
                    ERR_DEFLATE_CODE_LENGTHS,
                    format!("code length {length} is greater than 15"),
                ));
            }
            if length > 0 {
                symbols += 1;
                max_length = max_length.max(length);
            }
        }

        // Count symbols per length, then check the Kraft inequality: the number
        // of codes must not exceed the space the lengths have. An
        // over-subscribed tree would make `code` wrap and silently decode the
        // wrong symbol.
        let mut counts = [0u32; 16];
        for &length in lengths {
            counts[length as usize] += 1;
        }
        counts[0] = 0;
        let mut left = 1i64;
        for (length, &count) in counts.iter().enumerate().skip(1) {
            left = left * 2 - count as i64;
            if left < 0 {
                return Err(invalid_with(
                    ERR_DEFLATE_CODE_LENGTHS,
                    format!("the code is over-subscribed at length {length}"),
                ));
            }
        }

        // Canonical assignment, done the way RFC 1951 §3.2.2 spells it out:
        // compute the first code of every length from the counts, then hand
        // them out in symbol order within each length. Iterating symbols and
        // shifting on a length *increase* is the classic wrong version — it
        // silently assigns codes in the wrong order whenever a shorter code
        // appears after a longer one, which the RFC's own example does.
        let mut next_code = [0u32; 16];
        let mut code = 0u32;
        for bits in 1..=15usize {
            code = (code + counts[bits - 1]) << 1;
            next_code[bits] = code;
        }
        let mut entries = Vec::with_capacity(symbols);
        for (symbol, &length) in lengths.iter().enumerate() {
            if length == 0 {
                continue;
            }
            let slot = length as usize;
            let assigned = next_code[slot];
            next_code[slot] += 1;
            let assigned = u16::try_from(assigned).map_err(|_| {
                invalid_with(ERR_DEFLATE_CODE_LENGTHS, "a canonical code exceeds 15 bits")
            })?;
            entries.push((length, assigned, symbol as u16));
        }
        entries.sort_by_key(|&(length, code, _)| (length, code));

        Ok(Huffman {
            entries,
            max_length,
            symbols,
        })
    }

    fn is_empty(&self) -> bool {
        self.symbols == 0
    }

    /// Decode one symbol, bit by bit.
    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16> {
        if self.entries.is_empty() {
            return Err(invalid_with(
                ERR_DEFLATE_HUFFMAN,
                "a block tried to use an alphabet with no codes",
            ));
        }
        let mut code = 0u16;
        for length in 1..=self.max_length {
            code = (code << 1) | reader.read(1)? as u16;
            for &(entry_length, entry_code, symbol) in &self.entries {
                if entry_length == length && entry_code == code {
                    return Ok(symbol);
                }
            }
        }
        Err(invalid_with(
            ERR_DEFLATE_HUFFMAN,
            format!("no symbol for {}-bit code", self.max_length),
        ))
    }
}

/// The fixed literal/length tree of RFC 1951 §3.2.6.
fn fixed_literal_tree() -> Result<Huffman> {
    let mut lengths = [0u8; 288];
    for (index, slot) in lengths.iter_mut().enumerate() {
        *slot = match index {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Huffman::new(&lengths)
}

/// The fixed distance tree: 30 codes of 5 bits, with the last two unused.
fn fixed_distance_tree() -> Result<Huffman> {
    Huffman::new(&[5u8; 30])
}

/// Decompress a raw DEFLATE stream.
///
/// Returns exactly the bytes the stream encodes; trailing bytes after the final
/// block are *not* consumed here — the gzip reader checks them, because a gzip
/// member that ends before its footer is a different error from a tar that ends
/// before its entries.
pub fn inflate(input: &[u8], max_output: usize) -> Result<Vec<u8>> {
    inflate_from(input, max_output).map(|(bytes, _)| bytes)
}

/// Decompress a raw DEFLATE stream, reporting how many input bytes it consumed.
pub fn inflate_from(input: &[u8], max_output: usize) -> Result<(Vec<u8>, usize)> {
    let mut reader = BitReader::new(input);
    let mut output: Vec<u8> = Vec::new();

    let mut last = false;
    while !last {
        last = reader.read(1)? == 1;
        let block_type = reader.read(2)?;
        match block_type {
            0 => stored_block(&mut reader, &mut output, max_output)?,
            1 => {
                let literals = fixed_literal_tree()?;
                let distances = fixed_distance_tree()?;
                huffman_block(&mut reader, &mut output, max_output, &literals, &distances)?;
            }
            2 => {
                let (literals, distances) = dynamic_trees(&mut reader)?;
                huffman_block(&mut reader, &mut output, max_output, &literals, &distances)?;
            }
            other => {
                return Err(invalid_with(
                    ERR_DEFLATE_RESERVED,
                    format!("block type {other}"),
                ))
            }
        }
    }

    Ok((output, reader.bytes_consumed()))
}

/// A stored block: length and one's complement, then the bytes verbatim.
fn stored_block(reader: &mut BitReader<'_>, output: &mut Vec<u8>, max_output: usize) -> Result<()> {
    reader.align_to_byte();
    let length = reader.read(16)? as usize;
    let complement = reader.read(16)? as usize;
    if length != (!complement & 0xffff) {
        return Err(invalid_with(
            ERR_DEFLATE_STORED,
            format!("LEN {length} against NLEN {complement:#06x}"),
        ));
    }
    let available = reader.aligned_bytes()?;
    if available.len() < length {
        return Err(invalid_with(
            ERR_DEFLATE_TRUNCATED,
            format!(
                "stored block wants {length} bytes, {} remain",
                available.len()
            ),
        ));
    }
    if output.len() + length > max_output {
        return Err(output_limit_error(max_output));
    }
    output.extend_from_slice(&available[..length]);
    reader.skip_bytes(length);
    Ok(())
}

/// A Huffman-coded block: literals, matches, end-of-block.
fn huffman_block(
    reader: &mut BitReader<'_>,
    output: &mut Vec<u8>,
    max_output: usize,
    literals: &Huffman,
    distances: &Huffman,
) -> Result<()> {
    loop {
        let symbol = literals.decode(reader)?;
        match symbol {
            0..=255 => {
                if output.len() + 1 > max_output {
                    return Err(output_limit_error(max_output));
                }
                output.push(symbol as u8);
            }
            256 => return Ok(()),
            _ => {
                let index = usize::from(symbol - 257);
                let (Some(&base), Some(&extra)) = (LENGTH_BASE.get(index), LENGTH_EXTRA.get(index))
                else {
                    return Err(invalid_with(
                        ERR_DEFLATE_HUFFMAN,
                        format!("literal code {symbol} is not a length code"),
                    ));
                };
                let length = usize::from(base) + reader.read(extra)? as usize;

                let distance_symbol = distances.decode(reader)?;
                let distance_index = usize::from(distance_symbol);
                let (Some(&distance_base), Some(&distance_extra)) = (
                    DISTANCE_BASE.get(distance_index),
                    DISTANCE_EXTRA.get(distance_index),
                ) else {
                    return Err(invalid_with(
                        ERR_DEFLATE_HUFFMAN,
                        format!("distance code {distance_symbol} is not a distance"),
                    ));
                };
                let distance = usize::from(distance_base) + reader.read(distance_extra)? as usize;
                if distance > output.len() {
                    return Err(invalid_with(
                        ERR_DEFLATE_DISTANCE,
                        format!("{distance} bytes back, only {} produced", output.len()),
                    ));
                }
                if output.len() + length > max_output {
                    return Err(output_limit_error(max_output));
                }
                // Overlapping copies are legal and are how runs are encoded:
                // copy one byte at a time from the growing output.
                let start = output.len() - distance;
                for offset in 0..length {
                    let byte = output[start + offset];
                    output.push(byte);
                }
            }
        }
    }
}

/// Read the code lengths that define a dynamic block's two trees.
fn dynamic_trees(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman)> {
    let literal_count = reader.read(5)? as usize + 257;
    let distance_count = reader.read(5)? as usize + 1;
    let code_length_count = reader.read(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err(invalid_with(
            ERR_DEFLATE_CODE_LENGTHS,
            format!("HLIT {literal_count} or HDIST {distance_count} is out of range"),
        ));
    }

    let mut code_length_lengths = [0u8; 19];
    for &slot in CODE_LENGTH_ORDER.iter().take(code_length_count) {
        code_length_lengths[slot] = reader.read(3)? as u8;
    }
    let code_length_tree = Huffman::new(&code_length_lengths)?;

    let wanted = literal_count + distance_count;
    let mut lengths = Vec::with_capacity(wanted);
    while lengths.len() < wanted {
        let symbol = code_length_tree.decode(reader)?;
        match symbol {
            0..=15 => lengths.push(symbol as u8),
            16 => {
                let previous = *lengths.last().ok_or_else(|| {
                    invalid_with(
                        ERR_DEFLATE_CODE_LENGTHS,
                        "repeat code 16 with no previous length",
                    )
                })?;
                let repeat = 3 + reader.read(2)? as usize;
                for _ in 0..repeat {
                    lengths.push(previous);
                }
            }
            17 => {
                let repeat = 3 + reader.read(3)? as usize;
                lengths.extend(std::iter::repeat_n(0u8, repeat));
            }
            18 => {
                let repeat = 11 + reader.read(7)? as usize;
                lengths.extend(std::iter::repeat_n(0u8, repeat));
            }
            other => {
                return Err(invalid_with(
                    ERR_DEFLATE_HUFFMAN,
                    format!("code-length symbol {other} does not exist"),
                ))
            }
        }
        if lengths.len() > wanted {
            return Err(invalid_with(
                ERR_DEFLATE_CODE_LENGTHS,
                format!("code lengths run past HDIST: {} > {wanted}", lengths.len()),
            ));
        }
    }

    let literal_lengths = &lengths[..literal_count];
    if literal_lengths.len() <= 256 || literal_lengths[256] == 0 {
        return Err(invalid_with(
            ERR_DEFLATE_ENDOFBLOCK,
            "length 256 has length 0",
        ));
    }
    let literals = Huffman::new(literal_lengths)?;
    let distances = Huffman::new(&lengths[literal_count..])?;
    if distances.is_empty() && literals.max_length == 0 {
        return Err(invalid_with(ERR_DEFLATE_HUFFMAN, "both trees are empty"));
    }
    Ok((literals, distances))
}

/// The `Error` an over-long stream produces, exposed so gzip can reuse the
/// wording rather than invent a second one.
pub(crate) fn output_limit_error(limit: usize) -> Error {
    invalid_with(
        ERR_OUTPUT_LIMIT,
        format!(">{limit} bytes after decompression"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DEFLATE stream zlib produced for `b"one stored block: the quick brown
    /// fox\n"`. Generated once with Python's zlib (`compressobj(0, DEFLATED,
    /// -15)`) and pinned here: a stored block is the one encoding that is
    /// byte-exact, so it is the oracle for the bit reader.
    const STORED: &[u8] = &[
        0x01, 0x26, 0x00, 0xd9, 0xff, 0x6f, 0x6e, 0x65, 0x20, 0x73, 0x74, 0x6f, 0x72, 0x65, 0x64,
        0x20, 0x62, 0x6c, 0x6f, 0x63, 0x6b, 0x3a, 0x20, 0x74, 0x68, 0x65, 0x20, 0x71, 0x75, 0x69,
        0x63, 0x6b, 0x20, 0x62, 0x72, 0x6f, 0x77, 0x6e, 0x20, 0x66, 0x6f, 0x78, 0x0a,
    ];

    /// zlib level 6 of `b"fixed block, small input, harder alphabet\n"`.
    /// Chosen because zlib picks a fixed-Huffman block for it, so the fixed tree
    /// is exercised by a stream no one wrote by hand.
    const FIXED: &[u8] = &[
        0x4b, 0xcb, 0xac, 0x48, 0x4d, 0x51, 0x48, 0xca, 0xc9, 0x4f, 0xce, 0xd6, 0x51, 0x28, 0xce,
        0x4d, 0xcc, 0xc9, 0x51, 0xc8, 0xcc, 0x2b, 0x28, 0x2d, 0xd1, 0x51, 0xc8, 0x48, 0x2c, 0x4a,
        0x49, 0x2d, 0x52, 0x48, 0xcc, 0x29, 0xc8, 0x48, 0x4c, 0x4a, 0x2d, 0xe1, 0x02, 0x00,
    ];

    /// zlib level 9 of a 2,160-byte sentence repeated 24 times: a dynamic
    /// Huffman block (HLIT 286, HDIST 24), i.e. the case that "tends to be
    /// silently wrong".
    const DYNAMIC: &[u8] = &[
        0xed, 0xcc, 0xed, 0x0d, 0x80, 0x20, 0x0c, 0x84, 0xe1, 0x55, 0x6e, 0x00, 0xe3, 0x1e, 0x8c,
        0x41, 0x4c, 0x15, 0x42, 0xf9, 0x08, 0x2d, 0x18, 0xb7, 0xd7, 0xe0, 0x1a, 0xfd, 0x7b, 0xef,
        0xe5, 0x71, 0x05, 0x1a, 0x08, 0xd9, 0xf7, 0x44, 0x8a, 0x7a, 0x42, 0xb2, 0x67, 0x46, 0xe3,
        0x71, 0xc5, 0x22, 0x1b, 0x3c, 0x8e, 0x40, 0x47, 0x92, 0x91, 0x11, 0x65, 0x5d, 0x6b, 0xe1,
        0x07, 0xad, 0xd7, 0x1c, 0x85, 0xbe, 0xc1, 0x2b, 0x64, 0xf4, 0x19, 0x27, 0xfd, 0xb9, 0x90,
        0xde, 0xb5, 0xa7, 0x1d, 0xce, 0x64, 0x93, 0x4d, 0x36, 0xd9, 0x64, 0x93, 0x4d, 0x36, 0x79,
        0xc9, 0x2f,
    ];

    /// zlib level 9 of `b"abcabcabcabc" * 30`: back-references that overlap the
    /// bytes being written, the case a naive copy gets wrong.
    const RUNS: &[u8] = &[0x4b, 0x4c, 0x4a, 0x4e, 0x1c, 0x45, 0xb4, 0x47, 0x00];

    fn lorem() -> Vec<u8> {
        let sentence = b"In the market of small plugins, a checksum is the only promise that survives the network. ";
        sentence.repeat(24)
    }

    #[test]
    fn a_stored_block_round_trips_byte_for_byte() {
        let text = inflate(STORED, DEFAULT_MAX_OUTPUT).expect("stored block");
        assert_eq!(text, b"one stored block: the quick brown fox\n");
    }

    #[test]
    fn a_fixed_huffman_block_decodes() {
        let text = inflate(FIXED, DEFAULT_MAX_OUTPUT).expect("fixed block");
        assert_eq!(text, b"fixed block, small input, harder alphabet\n");
    }

    #[test]
    fn a_dynamic_huffman_block_decodes() {
        let text = inflate(DYNAMIC, DEFAULT_MAX_OUTPUT).expect("dynamic block");
        assert_eq!(text.len(), 2160);
        assert_eq!(text, lorem());
        // A dynamic block's distinguishing feature is its code-length header;
        // if the decode ignored it the output would not equal the source.
        assert!(String::from_utf8_lossy(&text).starts_with("In the market of small plugins"));
    }

    #[test]
    fn overlapping_back_references_are_copied_one_byte_at_a_time() {
        let text = inflate(RUNS, DEFAULT_MAX_OUTPUT).expect("runs");
        assert_eq!(text, b"abcabcabcabc".repeat(30));
    }

    #[test]
    fn the_consumed_byte_count_stops_at_the_end_of_the_stream() {
        let mut stream = STORED.to_vec();
        stream.extend_from_slice(b"trailing bytes that are not deflate");
        let (text, consumed) = inflate_from(&stream, DEFAULT_MAX_OUTPUT).expect("stored");
        assert_eq!(text, b"one stored block: the quick brown fox\n");
        assert_eq!(consumed, STORED.len());
    }

    #[test]
    fn a_reserved_block_type_is_refused() {
        // BFINAL=1, BTYPE=3.
        let err = inflate(&[0b0000_0111], DEFAULT_MAX_OUTPUT).unwrap_err();
        assert!(err.to_string().starts_with(ERR_DEFLATE_RESERVED), "{err}");
    }

    #[test]
    fn a_broken_stored_length_is_refused() {
        let mut stream = STORED.to_vec();
        // NLEN is the complement of LEN; flipping a bit must be caught before
        // any byte is trusted.
        stream[3] ^= 0x01;
        let err = inflate(&stream, DEFAULT_MAX_OUTPUT).unwrap_err();
        assert!(err.to_string().starts_with(ERR_DEFLATE_STORED), "{err}");
    }

    #[test]
    fn a_stored_block_that_stops_early_is_refused() {
        let stream = &STORED[..STORED.len() - 1];
        let err = inflate(stream, DEFAULT_MAX_OUTPUT).unwrap_err();
        assert!(err.to_string().starts_with(ERR_DEFLATE_TRUNCATED), "{err}");
    }

    #[test]
    fn a_truncated_huffman_stream_is_refused_rather_than_guessed() {
        let err = inflate(&DYNAMIC[..20], DEFAULT_MAX_OUTPUT).unwrap_err();
        assert!(
            err.to_string().starts_with(ERR_DEFLATE_TRUNCATED)
                || err.to_string().starts_with(ERR_DEFLATE_HUFFMAN)
                || err.to_string().starts_with(ERR_DEFLATE_CODE_LENGTHS),
            "{err}"
        );
    }

    #[test]
    fn the_output_cap_stops_a_bomb() {
        let err = inflate(DYNAMIC, 100).unwrap_err();
        assert!(err.to_string().starts_with(ERR_OUTPUT_LIMIT), "{err}");
    }

    #[test]
    fn a_canonical_code_assignment_matches_the_rfc_example() {
        // RFC 1951 §3.2.2's example: lengths 2,1,3,3 give codes 10, 0, 110, 111
        // for symbols 0,1,2,3 in canonical order.
        let tree = Huffman::new(&[2, 1, 3, 3]).expect("valid lengths");
        assert_eq!(tree.entries.len(), 4);
        assert_eq!(tree.max_length, 3);
        let codes: Vec<(u8, u16, u16)> = tree
            .entries
            .iter()
            .map(|&(length, code, symbol)| (length, code, symbol))
            .collect();
        assert_eq!(
            codes,
            vec![(1, 0b0, 1), (2, 0b10, 0), (3, 0b110, 2), (3, 0b111, 3)]
        );
    }

    #[test]
    fn an_over_subscribed_code_is_refused() {
        let err = Huffman::new(&[1, 1, 1]).unwrap_err();
        assert!(
            err.to_string().starts_with(ERR_DEFLATE_CODE_LENGTHS),
            "{err}"
        );
    }

    #[test]
    fn an_incomplete_code_is_accepted_because_the_rfc_allows_it() {
        // A tree with a single one-bit code leaves the other half of the space
        // unused; zlib emits such trees for the distance alphabet of a block
        // with no matches, so refusing them would refuse real streams.
        let tree = Huffman::new(&[1]).expect("incomplete is legal");
        assert_eq!(tree.entries, vec![(1, 0, 0)]);
    }
}
