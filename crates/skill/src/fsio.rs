//! Filesystem access behind a tiny seam.
//!
//! Production uses [`RealFs`]. Unit tests inject a fake implementation so the
//! error branches (permission denied, unreadable, unlistable) can be exercised
//! on any host without relying on local privileges.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorKind, Result};
use crate::util::{describe_io, display, trim_incomplete_rune};

/// Maximum bytes read from a `SKILL.md` (1 MiB), matching the Go oracle.
pub(crate) const MAX_SKILL_BYTES: usize = 1 << 20;
/// The single file this crate discovers.
pub(crate) const SKILL_FILE_NAME: &str = "SKILL.md";

/// A directory entry as seen by the scanner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// File name, not a full path.
    pub name: String,
    /// True when the entry itself is a directory.
    pub is_dir: bool,
    /// True when the entry is a symbolic link.
    pub is_symlink: bool,
}

/// The only metadata the scanner needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    /// True when the target is a directory (symlinks are followed).
    pub is_dir: bool,
}

/// Everything this crate needs from the filesystem.
pub(crate) trait Fs {
    /// `os.Stat`-style metadata: follows symlinks.
    fn metadata(&self, path: &Path) -> io::Result<Meta>;
    /// One directory listing, unsorted.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<Entry>>;
    /// Open a file for reading.
    fn open(&self, path: &Path) -> io::Result<File>;
    /// Resolve a path to a canonical form, failing if it cannot be resolved.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
}

/// The real, standard-library filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RealFs;

impl Fs for RealFs {
    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        fs::metadata(path).map(|info| Meta {
            is_dir: info.is_dir(),
        })
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for item in fs::read_dir(path)? {
            let item = item?;
            let kind = item.file_type()?;
            entries.push(Entry {
                name: item.file_name().to_string_lossy().into_owned(),
                is_dir: kind.is_dir(),
                is_symlink: kind.is_symlink(),
            });
        }
        Ok(entries)
    }

    fn open(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
}

/// Read at most `MAX_SKILL_BYTES + 1` bytes and report truncation.
///
/// The extra byte is only a sentinel: when present it is discarded together
/// with any incomplete UTF-8 rune at the cap, exactly as the Go reader does.
pub(crate) fn read_capped(fs: &dyn Fs, path: &Path) -> Result<(Vec<u8>, bool)> {
    let file = fs.open(path).map_err(|err| io_error("open", path, &err))?;
    let mut data = Vec::new();
    let mut limited = file.take(MAX_SKILL_BYTES as u64 + 1);
    limited
        .read_to_end(&mut data)
        .map_err(|err| io_error("read", path, &err))?;
    if data.len() > MAX_SKILL_BYTES {
        Ok((
            trim_incomplete_rune(&data[..MAX_SKILL_BYTES]).to_vec(),
            true,
        ))
    } else {
        Ok((data, false))
    }
}

fn io_error(action: &str, path: &Path, err: &io::Error) -> Error {
    Error::new(
        ErrorKind::Io,
        format!("{} {}: {}", action, display(path), describe_io(err)),
    )
}
