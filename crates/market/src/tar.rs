//! A tar reader: 512-byte headers, octal (and GNU base-256) sizes, GNU long
//! names, pax extended headers, and a checksum per header.
//!
//! Why hand-written: `tar` is not in the dependency budget either. The format is
//! old and forgiving in ways that matter here — a name can be split across
//! `prefix` and `name`, a long name arrives as a *previous* pseudo-entry, and
//! the checksum has two historical definitions. A reader that gets any of that
//! subtly wrong extracts a tarball with the wrong names, which after
//! [`crate::install`] records them is a bundle nobody can uninstall.
//!
//! **Safety is decided here, in [`Archive::parse`], not at write time.** Every
//! entry path is checked for absoluteness, `..` components, Windows drive
//! prefixes and NUL bytes before any bytes are written; a tarball with even one
//! such entry is refused as a whole rather than partially extracted. The
//! canonicalisation check in [`crate::unpack`] is the second line of defence,
//! because two checks that are not the same check catch different mistakes.

use forge_core::Result;

use crate::error::{
    invalid_with, refuse_with, ERR_DUPLICATE_ENTRY, ERR_ENTRY_LIMIT, ERR_TAR_CHECKSUM,
    ERR_TAR_HEADER, ERR_TAR_OCTAL, ERR_TAR_PAX, ERR_TAR_TRAILING, ERR_TAR_TRUNCATED,
    ERR_UNSAFE_PATH, ERR_UNSUPPORTED_ENTRY,
};

/// One header is 512 bytes; entry data is padded up to the next multiple.
const BLOCK: usize = 512;

/// How many entries a tarball may hold.
///
/// A plugin is tens of files. The cap is here because "the archive had two
/// million entries" is a denial of service delivered as a valid tarball, and the
/// install would spend minutes creating files before failing.
pub const MAX_ENTRIES: usize = 10_000;

/// One member of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path as the archive wrote it, normalised: `/` separators, no `.`
    /// components, no trailing slash. Never absolute, never containing `..`.
    pub path: String,
    /// Permission bits from the header. Applied on unix; ignored on Windows.
    pub mode: u32,
    /// The file's bytes, or empty for a directory.
    pub data: Vec<u8>,
    pub is_dir: bool,
}

/// A parsed archive, in the order the tarball listed it.
#[derive(Debug, Clone, Default)]
pub struct Archive {
    pub entries: Vec<Entry>,
}

impl Archive {
    /// The number of files (not directories).
    pub fn file_count(&self) -> usize {
        self.entries.iter().filter(|entry| !entry.is_dir).count()
    }

    /// Would writing these entries create this exact file?
    pub fn has_file(&self, path: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| !entry.is_dir && entry.path == path)
    }
}

/// What the previous pseudo-entry said about the next one.
#[derive(Default)]
struct Pending {
    /// A GNU `L` entry's payload: the real name of the next entry.
    long_name: Option<String>,
    /// A pax `x` entry's `path=` record.
    pax_path: Option<String>,
}

/// Parse a tar archive.
pub fn parse_tar(bytes: &[u8]) -> Result<Archive> {
    let mut archive = Archive::default();
    let mut offset = 0usize;
    let mut pending = Pending::default();
    let mut saw_end = false;

    while offset < bytes.len() {
        let header = bytes
            .get(offset..offset + BLOCK)
            .ok_or_else(|| invalid_with(ERR_TAR_HEADER, format!("at offset {offset}")))?;
        offset += BLOCK;

        if header.iter().all(|&byte| byte == 0) {
            saw_end = true;
            break;
        }
        verify_checksum(header)?;

        let typeflag = header[156];
        let size = parse_numeric(&header[124..136])?;
        let data_start = offset;
        let data_end = data_start
            .checked_add(size as usize)
            .ok_or_else(|| invalid_with(ERR_TAR_TRUNCATED, "size overflows the address space"))?;
        let data = bytes.get(data_start..data_end).ok_or_else(|| {
            invalid_with(
                ERR_TAR_TRUNCATED,
                format!(
                    "an entry at offset {data_start} wants {size} bytes, {} remain",
                    bytes.len().saturating_sub(data_start)
                ),
            )
        })?;
        // Data is padded to the next block; the pad bytes are not part of it and
        // are not checked, because some writers fill them with rubbish. A zero
        // length payload still occupies no block at all.
        let blocks = size.div_ceil(BLOCK as u64) as usize;
        offset = data_start + blocks * BLOCK;

        match typeflag {
            // A regular file (the two spellings of "old" and "new").
            b'0' | 0 => {
                let name = take_name(header, &mut pending)?;
                push_entry(&mut archive, name, mode(header), data.to_vec(), false)?;
            }
            // A directory.
            b'5' => {
                let name = take_name(header, &mut pending)?;
                push_entry(&mut archive, name, mode(header), Vec::new(), true)?;
            }
            // A GNU long name: the payload names the *next* entry.
            b'L' => {
                let name = parse_gnu_long_name(data)?;
                pending.long_name = Some(name);
            }
            // A pax extended header: key=value records for the next entry.
            b'x' => {
                pending.pax_path = parse_pax_path(data)?;
            }
            // GNU long link, symbolic link, hard link, character/block device,
            // FIFO. Every one of them can name a target outside the destination
            // or create a node the market has no business creating, and refusing
            // the whole archive is the only answer that cannot be bypassed.
            other => {
                return Err(refuse_with(
                    ERR_UNSUPPORTED_ENTRY,
                    format!(
                        "type flag {:?} at offset {}",
                        char::from(other),
                        data_start - BLOCK
                    ),
                ))
            }
        }
    }

    if !saw_end {
        return Err(invalid_with(
            ERR_TAR_HEADER,
            format!(
                "no end-of-archive marker; the last header began at {}",
                offset.saturating_sub(BLOCK)
            ),
        ));
    }
    // Everything after the first zero block must be zero: that is the archive's
    // own terminator and the padding a writer may add after it.
    let tail = bytes.get(offset..).unwrap_or_default();
    if let Some(index) = tail.iter().position(|&byte| byte != 0) {
        return Err(invalid_with(
            ERR_TAR_TRAILING,
            format!("a non-zero byte {} bytes after the end marker", index),
        ));
    }

    if archive.entries.iter().all(|entry| entry.is_dir) {
        return Err(refuse_with(
            crate::error::ERR_EMPTY_BUNDLE,
            "the archive lists only directories",
        ));
    }
    Ok(archive)
}

/// Is the header's checksum what the header says it is?
///
/// Two definitions exist: the header summed as unsigned bytes (what GNU tar and
/// bsdtar write) and as signed bytes (the original 1979 format, which mattered
/// on machines whose `char` was signed). Both are accepted; a header that
/// matches neither is corrupt.
fn verify_checksum(header: &[u8]) -> Result<()> {
    let stored = parse_numeric(&header[148..156])?;
    let mut unsigned: u64 = 0;
    let mut signed: i64 = 0;
    for (index, &byte) in header.iter().enumerate() {
        // The checksum field itself counts as spaces.
        let value = if (148..156).contains(&index) {
            b' '
        } else {
            byte
        };
        unsigned += u64::from(value);
        signed += i64::from(value as i8);
    }
    if stored == unsigned || (signed >= 0 && stored == signed as u64) {
        return Ok(());
    }
    Err(invalid_with(
        ERR_TAR_CHECKSUM,
        format!(
            "the header says {stored}, the bytes give {unsigned} (field {:?})",
            String::from_utf8_lossy(&header[148..156])
        ),
    ))
}

/// The path of this entry, preferring a pax override, then a GNU long name,
/// then the header's own `prefix` + `name`.
fn take_name(header: &[u8], pending: &mut Pending) -> Result<String> {
    let name = if let Some(name) = pending.pax_path.take() {
        name
    } else if let Some(name) = pending.long_name.take() {
        name
    } else {
        let name = field_string(&header[0..100]);
        let prefix = field_string(&header[345..500]);
        // The ustar prefix field only exists in the POSIX variant; the GNU
        // variant reuses those bytes for atime/ctime, where a non-empty read
        // would corrupt the name. Both variants write "ustar" at 257, so the
        // version field at 263 is what tells them apart: GNU writes " \0",
        // POSIX writes "00".
        let is_posix = header.get(257..263) == Some(b"ustar\0");
        if is_posix && !prefix.is_empty() {
            format!("{prefix}/{name}")
        } else {
            name
        }
    };
    // Clear whatever the pseudo-entries left, so the next ordinary header cannot
    // inherit a name meant for this one.
    pending.pax_path = None;
    pending.long_name = None;
    normalise_path(&name)
}

/// A GNU `L` payload is the name, NUL-terminated, possibly with trailing NULs.
fn parse_gnu_long_name(data: &[u8]) -> Result<String> {
    let end = data
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(data.len());
    let name = std::str::from_utf8(&data[..end])
        .map_err(|_| invalid_with(ERR_TAR_OCTAL, "a GNU long name is not valid UTF-8"))?;
    if name.is_empty() {
        return Err(invalid_with(ERR_TAR_PAX, "a GNU long name is empty"));
    }
    Ok(name.to_string())
}

/// A pax payload is a sequence of `"%d %s=%s\n"` records; only `path` matters
/// here, because it is the one that can change where a file lands.
fn parse_pax_path(data: &[u8]) -> Result<Option<String>> {
    let mut path = None;
    let mut rest = data;
    while !rest.is_empty() {
        // Leading spaces are allowed by some writers; the length is decimal.
        let digits_end = rest
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .ok_or_else(|| invalid_with(ERR_TAR_PAX, "a record has no length separator"))?;
        if digits_end == 0 {
            // A record whose length starts with a non-digit is malformed.
            return Err(invalid_with(
                ERR_TAR_PAX,
                format!(
                    "a record starts with {:?}",
                    String::from_utf8_lossy(&rest[..rest.len().min(16)])
                ),
            ));
        }
        let length: usize = std::str::from_utf8(&rest[..digits_end])
            .ok()
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| invalid_with(ERR_TAR_PAX, "a record length is not a decimal number"))?;
        if length <= digits_end || length > rest.len() {
            return Err(invalid_with(
                ERR_TAR_PAX,
                format!("record length {length} does not fit the payload"),
            ));
        }
        // The record is `"<length> <key>=<value>\n"`, where the length counts
        // itself, the single separating space and the newline. The space belongs
        // to the framing, not to the key.
        let record = &rest[digits_end..length];
        let record = record.strip_prefix(b" ").unwrap_or(record);
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        let equals = record
            .iter()
            .position(|&byte| byte == b'=')
            .ok_or_else(|| {
                invalid_with(
                    ERR_TAR_PAX,
                    format!(
                        "a record is not key=value: {:?}",
                        String::from_utf8_lossy(record)
                    ),
                )
            })?;
        let (key, value) = (&record[..equals], &record[equals + 1..]);
        let key = std::str::from_utf8(key)
            .map_err(|_| invalid_with(ERR_TAR_PAX, "a record key is not UTF-8"))?;
        if key == "path" {
            let value = std::str::from_utf8(value)
                .map_err(|_| invalid_with(ERR_TAR_PAX, "a pax path is not UTF-8"))?;
            path = Some(value.to_string());
        }
        rest = &rest[length..];
    }
    Ok(path)
}

/// Parse an octal (or GNU base-256) numeric field.
fn parse_numeric(field: &[u8]) -> Result<u64> {
    // GNU's extension for values that do not fit eight octal digits: the top bit
    // of the first byte is set and the rest is a big-endian two's complement
    // number. It appears for sizes over 8 GiB and for timestamps past 2242.
    if field.first().is_some_and(|byte| byte & 0x80 != 0) {
        let mut value: i128 = if field[0] & 0x40 != 0 { -1 } else { 0 };
        for (index, &byte) in field.iter().enumerate() {
            let byte = if index == 0 { byte & 0x7f } else { byte };
            value = (value << 8) | i128::from(byte);
        }
        if value < 0 {
            return Err(invalid_with(
                ERR_TAR_OCTAL,
                format!("negative value {value}"),
            ));
        }
        return u64::try_from(value)
            .map_err(|_| invalid_with(ERR_TAR_OCTAL, format!("value {value} overflows")));
    }

    let text = field_string(field);
    let text = text.trim();
    if text.is_empty() {
        // An empty field means zero in every tar implementation.
        return Ok(0);
    }
    let digits = text.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    if !digits.bytes().all(|byte| (b'0'..=b'7').contains(&byte)) {
        return Err(invalid_with(
            ERR_TAR_OCTAL,
            format!("{text:?} is not octal"),
        ));
    }
    u64::from_str_radix(digits, 8)
        .map_err(|_| invalid_with(ERR_TAR_OCTAL, format!("{text:?} overflows a u64")))
}

/// A NUL-terminated header string.
fn field_string(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// The permission bits, or 0o644 when the field is absent.
fn mode(header: &[u8]) -> u32 {
    parse_numeric(&header[100..108])
        .map(|value| (value & 0o7777) as u32)
        .unwrap_or(0o644)
}

/// Validate and normalise a path.
///
/// The rule is one sentence: **the path must be relative, and every component
/// must be a plain name.** Everything below is an instance of it. `.` is dropped
/// because `a/./b` and `a/b` are the same file and a record that lists both
/// twice would make uninstall ambiguous.
fn normalise_path(raw: &str) -> Result<String> {
    if raw.is_empty() {
        return Err(refuse_with(ERR_UNSAFE_PATH, "an entry has an empty path"));
    }
    if raw.contains('\0') {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!("{:?} contains a NUL byte", raw.escape_debug()),
        ));
    }
    // The archive separator is `/`; a backslash inside a component is a legal
    // filename on unix but a separator on Windows, so it cannot be allowed to
    // appear at all in a path this crate is going to join onto a directory.
    if raw.contains('\\') {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!("{:?} contains a backslash", raw.escape_debug()),
        ));
    }
    if raw.starts_with('/') {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!("{:?} is absolute", raw.escape_debug()),
        ));
    }
    // "C:/x" and "C:x" are absolute on Windows; a colon anywhere in the first
    // component is refused, because that is where a drive or an ADS would sit.
    if raw
        .split('/')
        .next()
        .is_some_and(|first| first.contains(':'))
    {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!("{:?} names a drive or a stream", raw.escape_debug()),
        ));
    }

    let mut parts: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                return Err(refuse_with(
                    ERR_UNSAFE_PATH,
                    format!("{:?} climbs with ..", raw.escape_debug()),
                ))
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!("{:?} names no file", raw.escape_debug()),
        ));
    }
    Ok(parts.join("/"))
}

/// Append an entry, refusing a duplicate path.
///
/// A tar may legally list the same name twice (the later one wins). Refusing is
/// the fail-closed choice: with duplicates, "which bytes did this file have" has
/// two answers and the install record can only hold one.
fn push_entry(
    archive: &mut Archive,
    path: String,
    mode: u32,
    data: Vec<u8>,
    is_dir: bool,
) -> Result<()> {
    if archive.entries.len() >= MAX_ENTRIES {
        return Err(refuse_with(
            ERR_ENTRY_LIMIT,
            format!("more than {MAX_ENTRIES} entries"),
        ));
    }
    if archive.entries.iter().any(|entry| entry.path == path) {
        return Err(refuse_with(
            ERR_DUPLICATE_ENTRY,
            format!("{path:?} appears twice"),
        ));
    }
    archive.entries.push(Entry {
        path,
        mode,
        data,
        is_dir,
    });
    Ok(())
}

/// A minimal tar *writer*, compiled only for tests.
///
/// It lives in the library rather than in `tests/` because the unit tests of
/// [`crate::unpack`] and [`crate::install`] need to build archives too, and an
/// integration test cannot reach a crate-private helper. Writing a tarball is
/// test scaffolding, never a thing this crate does for a user.
#[cfg(test)]
pub(crate) mod fixture {
    use super::BLOCK;

    /// Build one header block.
    pub(crate) fn header(name: &str, size: u64, typeflag: u8, mode: u32) -> [u8; BLOCK] {
        let mut block = [0u8; BLOCK];
        let name = name.as_bytes();
        assert!(name.len() <= 100, "the test writer only writes short names");
        block[..name.len()].copy_from_slice(name);
        let mode_text = format!("{:07o}\0", mode & 0o7777);
        block[100..108].copy_from_slice(mode_text.as_bytes());
        let uid = format!("{:07o}\0", 0);
        block[108..116].copy_from_slice(uid.as_bytes());
        block[116..124].copy_from_slice(uid.as_bytes());
        let size_text = format!("{:011o}\0", size);
        block[124..136].copy_from_slice(size_text.as_bytes());
        let mtime = format!("{:011o}\0", 0);
        block[136..148].copy_from_slice(mtime.as_bytes());
        block[156] = typeflag;
        block[257..263].copy_from_slice(b"ustar\0");
        block[263..265].copy_from_slice(b"00");
        // The checksum field is spaces while the sum is computed.
        for byte in &mut block[148..156] {
            *byte = b' ';
        }
        let sum: u64 = block.iter().map(|&byte| u64::from(byte)).sum();
        let checksum = format!("{:06o}\0 ", sum);
        block[148..156].copy_from_slice(checksum.as_bytes());
        block
    }

    /// Build a whole archive from (path, is_dir, bytes) triples.
    pub(crate) fn archive(entries: &[(&str, bool, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (path, is_dir, data) in entries {
            let name = if *is_dir {
                format!("{path}/")
            } else {
                (*path).to_string()
            };
            let typeflag = if *is_dir { b'5' } else { b'0' };
            let size = if *is_dir { 0 } else { data.len() as u64 };
            out.extend_from_slice(&header(&name, size, typeflag, 0o644));
            if !*is_dir {
                out.extend_from_slice(data);
                let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
                out.extend(std::iter::repeat_n(0u8, pad));
            }
        }
        out.extend_from_slice(&[0u8; BLOCK * 2]);
        out
    }

    /// The padding a payload of `length` bytes needs to fill a block.
    pub(crate) fn padding(length: usize) -> usize {
        (BLOCK - length % BLOCK) % BLOCK
    }

    /// Recompute a header's checksum after a test edited one of its fields.
    pub(crate) fn rehash(block: &mut [u8; BLOCK]) {
        for byte in &mut block[148..156] {
            *byte = b' ';
        }
        let sum: u64 = block.iter().map(|&byte| u64::from(byte)).sum();
        let checksum = format!("{:06o}\0 ", sum);
        block[148..156].copy_from_slice(checksum.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{archive, header, padding, rehash};
    use super::*;

    #[test]
    fn a_short_archive_round_trips() {
        let bytes = archive(&[
            ("plugin.json", false, b"{\"name\":\"x\"}"),
            ("lib", true, b""),
            ("lib/util.js", false, b"module.exports = {};"),
        ]);
        let parsed = parse_tar(&bytes).expect("parse");
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[0].path, "plugin.json");
        assert_eq!(parsed.entries[0].data, b"{\"name\":\"x\"}");
        assert!(parsed.entries[1].is_dir);
        assert_eq!(parsed.entries[1].path, "lib");
        assert_eq!(parsed.entries[2].path, "lib/util.js");
        assert_eq!(parsed.file_count(), 2);
    }

    #[test]
    fn a_flipped_header_byte_fails_the_checksum() {
        let mut bytes = archive(&[("a.txt", false, b"hello")]);
        bytes[0] = b'b';
        let err = parse_tar(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_TAR_CHECKSUM), "{err}");
    }

    #[test]
    fn a_truncated_archive_is_refused() {
        let bytes = archive(&[("a.txt", false, b"0123456789")]);
        let err = parse_tar(&bytes[..600]).unwrap_err();
        // Either the data is short or the end marker is missing; both are
        // refusals and neither is a partial extract.
        assert!(
            err.to_string().starts_with(ERR_TAR_TRUNCATED)
                || err.to_string().starts_with(ERR_TAR_HEADER),
            "{err}"
        );
    }

    #[test]
    fn a_dotdot_entry_is_refused_as_an_unsafe_path() {
        let bytes = archive(&[("../escape.txt", false, b"x")]);
        let err = parse_tar(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSAFE_PATH), "{err}");
        assert!(err.to_string().contains(".."), "{err}");
    }

    #[test]
    fn an_absolute_entry_is_refused() {
        for path in ["/etc/passwd", "C:/Windows/system32", "C:relative"] {
            let bytes = archive(&[(path, false, b"x")]);
            let err = parse_tar(&bytes).unwrap_err();
            assert!(
                err.to_string().starts_with(ERR_UNSAFE_PATH),
                "{path}: {err}"
            );
        }
    }

    #[test]
    fn a_nested_escape_is_refused_even_when_it_starts_inside() {
        let bytes = archive(&[("lib/../../outside.txt", false, b"x")]);
        let err = parse_tar(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSAFE_PATH), "{err}");
    }

    #[test]
    fn dot_components_are_dropped_so_the_record_has_one_name() {
        let bytes = archive(&[("./lib/./util.js", false, b"x")]);
        let parsed = parse_tar(&bytes).expect("parse");
        assert_eq!(parsed.entries[0].path, "lib/util.js");
    }

    #[test]
    fn a_symlink_entry_is_refused_rather_than_followed() {
        let mut bytes = Vec::new();
        let mut block = header("link", 0, b'2', 0o777);
        block[157..161].copy_from_slice(b"/etc");
        rehash(&mut block);
        bytes.extend_from_slice(&block);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);
        let err = parse_tar(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSUPPORTED_ENTRY), "{err}");
    }

    #[test]
    fn a_gnu_long_name_is_applied_to_the_next_entry() {
        let long = format!("{}.txt", "n".repeat(150));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header("././@LongLink", long.len() as u64 + 1, b'L', 0o644));
        let mut payload = long.clone().into_bytes();
        payload.push(0);
        bytes.extend_from_slice(&payload);
        let pad = (BLOCK - payload.len() % BLOCK) % BLOCK;
        bytes.extend(std::iter::repeat_n(0u8, pad));
        bytes.extend_from_slice(&header("short", 2, b'0', 0o644));
        bytes.extend_from_slice(b"hi");
        bytes.extend_from_slice(&[0u8; BLOCK - 2]);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);

        let parsed = parse_tar(&bytes).expect("parse");
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].path, long);
        assert_eq!(parsed.entries[0].data, b"hi");
    }

    #[test]
    fn a_pax_path_overrides_the_header_name() {
        let record = "30 path=lib/renamed-by-pax.js\n";
        assert_eq!(record.len(), 30);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header("PaxHeader/short", record.len() as u64, b'x', 0o644));
        bytes.extend_from_slice(record.as_bytes());
        bytes.extend(std::iter::repeat_n(0u8, padding(record.len())));
        bytes.extend_from_slice(&header("short", 3, b'0', 0o644));
        bytes.extend_from_slice(b"abc");
        bytes.extend_from_slice(&[0u8; BLOCK - 3]);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);

        let parsed = parse_tar(&bytes).expect("parse");
        assert_eq!(parsed.entries[0].path, "lib/renamed-by-pax.js");
    }

    #[test]
    fn a_pax_long_name_that_escapes_is_refused_too() {
        // The pax override is a path like any other: a long name is not a
        // licence to leave the destination.
        let record = "20 path=../../x.txt\n";
        assert_eq!(record.len(), 20);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header("PaxHeader/short", record.len() as u64, b'x', 0o644));
        bytes.extend_from_slice(record.as_bytes());
        bytes.extend(std::iter::repeat_n(0u8, padding(record.len())));
        bytes.extend_from_slice(&header("short", 1, b'0', 0o644));
        bytes.extend_from_slice(b"a");
        bytes.extend_from_slice(&[0u8; BLOCK - 1]);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);

        let err = parse_tar(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSAFE_PATH), "{err}");
    }

    #[test]
    fn a_prefix_field_is_joined_for_the_posix_variant() {
        let mut block = header("util.js", 1, b'0', 0o644);
        // Replace the name with one that fits, and fill the prefix field.
        for byte in &mut block[0..100] {
            *byte = 0;
        }
        block[..7].copy_from_slice(b"util.js");
        block[345..352].copy_from_slice(b"lib/sub");
        rehash(&mut block);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&block);
        bytes.extend_from_slice(b"a");
        bytes.extend_from_slice(&[0u8; BLOCK - 1]);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);
        let parsed = parse_tar(&bytes).expect("parse");
        assert_eq!(parsed.entries[0].path, "lib/sub/util.js");
    }

    #[test]
    fn a_gnu_header_does_not_read_the_prefix_field_as_a_name() {
        // GNU tar stores atime/ctime where ustar stores the prefix; reading them
        // as a directory would rename every file in a GNU-format archive.
        let mut block = header("util.js", 1, b'0', 0o644);
        block[257..263].copy_from_slice(b"ustar ");
        block[263..265].copy_from_slice(b" \0");
        block[345..352].copy_from_slice(b"1atime\0");
        rehash(&mut block);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&block);
        bytes.extend_from_slice(b"a");
        bytes.extend_from_slice(&[0u8; BLOCK - 1]);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);
        let parsed = parse_tar(&bytes).expect("parse");
        assert_eq!(parsed.entries[0].path, "util.js");
    }

    #[test]
    fn the_same_path_twice_is_refused() {
        let mut bytes = archive(&[("a.txt", false, b"one")]);
        let mut second = archive(&[("a.txt", false, b"two")]);
        // Splice a second archive's entry before the end marker.
        bytes.truncate(bytes.len() - BLOCK * 2);
        second.truncate(second.len() - BLOCK * 2);
        bytes.extend_from_slice(&second);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);
        let err = parse_tar(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_DUPLICATE_ENTRY), "{err}");
    }

    #[test]
    fn a_gnu_base256_size_is_read() {
        // Size 300 encoded the GNU way: high bit set on the first byte.
        let mut field = [0u8; 12];
        field[0] = 0x80;
        field[10] = 0x01;
        field[11] = 0x2c; // 300
        assert_eq!(parse_numeric(&field).expect("base256"), 300);
    }

    #[test]
    fn an_octal_size_with_leading_zeros_and_a_nul_is_read() {
        assert_eq!(parse_numeric(b"00000000240\0").expect("octal"), 160);
        assert_eq!(parse_numeric(b"00000000000\0").expect("zero"), 0);
        assert_eq!(parse_numeric(b"        ").expect("blank"), 0);
        assert!(parse_numeric(b"00000000009\0").is_err(), "9 is not octal");
    }

    #[test]
    fn an_archive_of_only_directories_is_not_a_bundle() {
        let bytes = archive(&[("lib", true, b"")]);
        let err = parse_tar(&bytes).unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_EMPTY_BUNDLE),
            "{err}"
        );
    }

    #[test]
    fn a_non_zero_byte_after_the_end_marker_is_refused() {
        let mut bytes = archive(&[("a.txt", false, b"x")]);
        bytes.extend_from_slice(b"junk");
        let err = parse_tar(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(ERR_TAR_TRAILING), "{err}");
    }
}
