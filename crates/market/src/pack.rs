//! The producer half: turn a directory into a bundle the consumer half accepts.
//!
//! [`crate::install`] is deliberately the only way bytes become an installed
//! bundle, and it verifies a SHA-256 and unpacks a tar/gzip. This module produces
//! exactly that artifact, using the *same* format constants the reader checks, so
//! a packed bundle is one the installer can read by construction rather than by
//! luck. The tar writer emits ustar headers; the gzip wrapper uses stored
//! (uncompressed) DEFLATE blocks, which [`crate::inflate`] accepts — a plugin is
//! small and already-compressed text, so a real compressor would buy little and
//! cost a second encoder to audit.
//!
//! What this module will not do: follow a symbolic link, store a path the reader
//! would refuse (absolute, `..`, backslash, NUL, a drive letter), or emit a name
//! too long for a ustar header. Each of those is a refusal naming the path, not a
//! silently different archive.

use std::fs;
use std::path::Path;

use forge_core::Result;

use crate::crc32::crc32;
use crate::error::{
    not_found_with, refuse_with, ERR_PACK_EMPTY, ERR_PACK_PATH, ERR_PACK_SIZE, ERR_PACK_SOURCE,
    ERR_PACK_SYMLINK,
};
use crate::hash::sha256_hex;

/// One tar block.
const BLOCK: usize = 512;
/// The largest payload a stored DEFLATE block can carry.
const STORED_MAX: usize = 65_535;

/// A directory packed into a distributable bundle.
#[derive(Debug, Clone)]
pub struct Packed {
    /// The gzipped tarball, ready to write to `<name>-<version>.tar.gz`.
    pub tarball: Vec<u8>,
    /// Lowercase hex SHA-256 of `tarball` — the value the index must declare.
    pub sha256: String,
    /// The exact byte length of `tarball` — the value the index must declare.
    pub size: u64,
    /// The relative file paths written, sorted, `/` separators.
    pub files: Vec<String>,
}

/// Pack every regular file under `dir` into a gzipped tarball.
///
/// Subdirectories are walked; their paths become nested entry names. Empty
/// directories contribute nothing (the reader creates parents from file paths).
pub fn pack_dir(dir: &Path) -> Result<Packed> {
    let meta = fs::metadata(dir)
        .map_err(|err| not_found_with(ERR_PACK_SOURCE, format!("{}: {err}", dir.display())))?;
    if !meta.is_dir() {
        return Err(refuse_with(
            ERR_PACK_SOURCE,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
    collect(dir, dir, &mut files)?;
    if files.is_empty() {
        return Err(refuse_with(
            ERR_PACK_EMPTY,
            format!("{} has no files to pack", dir.display()),
        ));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let tar = write_tar(&files)?;
    let tarball = gzip_stored(&tar);
    let sha256 = sha256_hex(&tarball);
    let size = tarball.len() as u64;
    let names = files.into_iter().map(|(path, _, _)| path).collect();
    Ok(Packed {
        tarball,
        sha256,
        size,
        files: names,
    })
}

/// Walk `current`, appending (relative path, bytes, mode) for each regular file.
fn collect(root: &Path, current: &Path, out: &mut Vec<(String, Vec<u8>, u32)>) -> Result<()> {
    let entries = fs::read_dir(current)
        .map_err(|err| not_found_with(ERR_PACK_SOURCE, format!("{}: {err}", current.display())))?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            not_found_with(ERR_PACK_SOURCE, format!("{}: {err}", current.display()))
        })?;
        let path = entry.path();
        // file_type does not follow the link, so a symlink is seen as one.
        let kind = entry
            .file_type()
            .map_err(|err| not_found_with(ERR_PACK_SOURCE, format!("{}: {err}", path.display())))?;
        if kind.is_symlink() {
            return Err(refuse_with(ERR_PACK_SYMLINK, format!("{}", path.display())));
        }
        if kind.is_dir() {
            collect(root, &path, out)?;
            continue;
        }
        if !kind.is_file() {
            // A fifo, socket or device is not a bundle member.
            return Err(refuse_with(
                ERR_PACK_PATH,
                format!("{} is not a regular file", path.display()),
            ));
        }
        let relative = relative_path(root, &path)?;
        let data = fs::read(&path)
            .map_err(|err| not_found_with(ERR_PACK_SOURCE, format!("{}: {err}", path.display())))?;
        let mode = file_mode(&entry.metadata().map_err(|err| {
            not_found_with(ERR_PACK_SOURCE, format!("{}: {err}", path.display()))
        })?);
        out.push((relative, data, mode));
    }
    Ok(())
}

/// The path of `file` relative to `root`, with `/` separators, checked against
/// the same rules the tar reader enforces so a packed name is always readable.
fn relative_path(root: &Path, file: &Path) -> Result<String> {
    let relative = file.strip_prefix(root).map_err(|_| {
        refuse_with(
            ERR_PACK_PATH,
            format!("{} escaped its root", file.display()),
        )
    })?;
    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        let text = component.as_os_str().to_string_lossy().into_owned();
        if text.is_empty() || text == "." {
            continue;
        }
        if text == ".." || text.contains('\\') || text.contains('\0') {
            return Err(refuse_with(
                ERR_PACK_PATH,
                format!("{} cannot be stored safely", file.display()),
            ));
        }
        parts.push(text);
    }
    if parts.is_empty() {
        return Err(refuse_with(
            ERR_PACK_PATH,
            format!("{} names no file", file.display()),
        ));
    }
    let joined = parts.join("/");
    // A drive letter or stream in the first component is absolute on Windows.
    if joined
        .split('/')
        .next()
        .is_some_and(|first| first.contains(':'))
    {
        return Err(refuse_with(
            ERR_PACK_PATH,
            format!("{} names a drive or a stream", file.display()),
        ));
    }
    Ok(joined)
}

/// Preserve the unix permission bits (so an executable entry stays executable);
/// on Windows the mode is ignored by the unpacker, so a plain 0o644 is honest.
fn file_mode(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o644
    }
}

/// Build a ustar archive from (path, bytes, mode) triples, files only.
fn write_tar(files: &[(String, Vec<u8>, u32)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (path, data, mode) in files {
        let (name, prefix) = split_name(path)?;
        out.extend_from_slice(&header(&name, &prefix, data.len() as u64, *mode)?);
        out.extend_from_slice(data);
        let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    // The end-of-archive marker: two zero blocks.
    out.extend_from_slice(&[0u8; BLOCK * 2]);
    Ok(out)
}

/// Split a path into ustar `name` (≤100) and `prefix` (≤155) at a `/` boundary.
fn split_name(path: &str) -> Result<(String, String)> {
    if path.len() <= 100 {
        return Ok((path.to_string(), String::new()));
    }
    for (index, ch) in path.char_indices() {
        if ch != '/' {
            continue;
        }
        let prefix = &path[..index];
        let name = &path[index + 1..];
        if !name.is_empty() && name.len() <= 100 && prefix.len() <= 155 {
            return Ok((name.to_string(), prefix.to_string()));
        }
    }
    Err(refuse_with(
        ERR_PACK_PATH,
        format!("{path:?} is too long for a ustar header"),
    ))
}

/// One 512-byte ustar header with a valid checksum.
fn header(name: &str, prefix: &str, size: u64, mode: u32) -> Result<[u8; BLOCK]> {
    let mut block = [0u8; BLOCK];
    block[..name.len()].copy_from_slice(name.as_bytes());
    put_octal(&mut block[100..108], u64::from(mode))?;
    put_octal(&mut block[108..116], 0)?;
    put_octal(&mut block[116..124], 0)?;
    put_octal(&mut block[124..136], size)?;
    put_octal(&mut block[136..148], 0)?;
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    if !prefix.is_empty() {
        block[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    }
    // The checksum field counts as spaces while the sum is taken.
    for byte in &mut block[148..156] {
        *byte = b' ';
    }
    let sum: u64 = block.iter().map(|&byte| u64::from(byte)).sum();
    let checksum = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(checksum.as_bytes());
    Ok(block)
}

/// Write a NUL-terminated, zero-padded octal field, refusing an overflow.
fn put_octal(field: &mut [u8], value: u64) -> Result<()> {
    let width = field.len() - 1;
    let text = format!("{value:0width$o}");
    if text.len() > width {
        return Err(refuse_with(
            ERR_PACK_SIZE,
            format!("{value} does not fit a {width}-digit octal field"),
        ));
    }
    field[..text.len()].copy_from_slice(text.as_bytes());
    field[width] = 0;
    Ok(())
}

/// Wrap bytes in a gzip member using stored DEFLATE blocks (no compression).
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // magic, method=deflate, flags=0, mtime=0, xfl=0, OS=unix.
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0x03]);
    if data.is_empty() {
        // A single empty final stored block.
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let mut offset = 0;
        while offset < data.len() {
            let len = (data.len() - offset).min(STORED_MAX);
            let is_final = offset + len >= data.len();
            out.push(if is_final { 0x01 } else { 0x00 });
            out.extend_from_slice(&(len as u16).to_le_bytes());
            out.extend_from_slice(&(!(len as u16)).to_le_bytes());
            out.extend_from_slice(&data[offset..offset + len]);
            offset += len;
        }
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&((data.len() as u64 & 0xffff_ffff) as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gunzip, parse_tar};

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-pack-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch");
        dir
    }

    #[test]
    fn a_packed_directory_round_trips_through_the_consumer_codec() {
        let dir = scratch("roundtrip");
        fs::write(dir.join("plugin.json"), b"{\"name\":\"x\"}").expect("write");
        fs::create_dir_all(dir.join("lib")).expect("mkdir");
        fs::write(dir.join("lib/util.js"), b"module.exports = {};").expect("write");

        let packed = pack_dir(&dir).expect("pack");
        // The exact path the installer takes: gunzip, then parse_tar.
        let tar = gunzip(&packed.tarball).expect("gunzip");
        let archive = parse_tar(&tar).expect("parse");
        assert_eq!(archive.file_count(), 2);
        assert!(archive.has_file("plugin.json"));
        assert!(archive.has_file("lib/util.js"));
        assert_eq!(packed.files, vec!["lib/util.js", "plugin.json"]);
        assert_eq!(packed.size, packed.tarball.len() as u64);
        assert_eq!(packed.sha256, sha256_hex(&packed.tarball));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_large_payload_crosses_the_stored_block_boundary() {
        let dir = scratch("large");
        // > 65535 bytes forces more than one stored DEFLATE block.
        let big = "x".repeat(150_000);
        fs::write(dir.join("big.txt"), &big).expect("write");
        let packed = pack_dir(&dir).expect("pack");
        let tar = gunzip(&packed.tarball).expect("gunzip");
        let archive = parse_tar(&tar).expect("parse");
        let entry = archive
            .entries
            .iter()
            .find(|e| e.path == "big.txt")
            .expect("big.txt");
        assert_eq!(entry.data.len(), big.len());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_directory_is_refused() {
        let dir = scratch("empty");
        let err = pack_dir(&dir).unwrap_err();
        assert!(err.to_string().starts_with(ERR_PACK_EMPTY), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_is_refused() {
        let dir = scratch("missing");
        let _ = fs::remove_dir_all(&dir);
        let err = pack_dir(&dir).unwrap_err();
        assert!(err.to_string().starts_with(ERR_PACK_SOURCE), "{err}");
    }

    #[test]
    fn a_long_nested_path_uses_the_ustar_prefix_and_still_reads_back() {
        let dir = scratch("longpath");
        // A path over 100 bytes that can split into prefix + name.
        let nested = format!("{}/file.js", "sub".repeat(30));
        let full = dir.join(&nested);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(&full, b"ok").expect("write");
        let packed = pack_dir(&dir).expect("pack");
        let tar = gunzip(&packed.tarball).expect("gunzip");
        let archive = parse_tar(&tar).expect("parse");
        assert!(archive.has_file(&nested), "{:?}", packed.files);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_rather_than_followed() {
        let dir = scratch("symlink");
        fs::write(dir.join("real.txt"), b"ok").expect("write");
        std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).expect("symlink");
        let err = pack_dir(&dir).unwrap_err();
        assert!(err.to_string().starts_with(ERR_PACK_SYMLINK), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
