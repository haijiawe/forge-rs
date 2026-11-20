//! Filesystem access behind a seam, so every refusal branch is testable.
//!
//! A roster is read from a directory the user owns; that means permission
//! errors, an unreadable subdirectory and an absent root are all normal states,
//! not programming errors. They must be *tested*, and a test that needs a real
//! ACL is a test that silently skips on the one machine that matters. So the
//! production [`RealFs`] and the test fake implement one tiny trait, and the
//! branches are exercised by the fake rather than by luck.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::error::{Error, ErrorKind, Result};
use crate::util::{describe_io, display};

/// The largest definition file this reader will take, in bytes (1 MiB).
///
/// A prompt is content, not data: a file over the cap is **refused**, never
/// truncated, because a truncated prompt is a prompt nobody reviewed. The cap
/// exists because an unbounded read on a shared roster directory is how a
/// hundred-megabyte file becomes a hundred-megabyte allocation.
pub(crate) const MAX_FILE_BYTES: usize = 1 << 20;

/// The only metadata the walker needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    /// True when the target is a directory (symlinks are followed).
    pub is_dir: bool,
}

/// Everything this crate needs from the filesystem.
pub(crate) trait Fs {
    /// `stat`-style metadata, following symlinks.
    fn metadata(&self, path: &Path) -> io::Result<Meta>;
    /// One directory listing, as bare names, unsorted.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<String>>;
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

    fn read_dir(&self, path: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for item in fs::read_dir(path)? {
            names.push(item?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn open(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
}

/// Read at most `MAX_FILE_BYTES + 1` bytes and report whether more remained.
///
/// The extra byte is a sentinel only: it is never part of the returned buffer,
/// so a caller that sees `true` refuses the file instead of parsing half of it.
pub(crate) fn read_capped(fs: &dyn Fs, path: &Path) -> Result<(Vec<u8>, bool)> {
    let file = fs.open(path).map_err(|err| io_error("open", path, &err))?;
    let mut data = Vec::new();
    let mut limited = file.take(MAX_FILE_BYTES as u64 + 1);
    limited
        .read_to_end(&mut data)
        .map_err(|err| io_error("read", path, &err))?;
    if data.len() > MAX_FILE_BYTES {
        data.truncate(MAX_FILE_BYTES);
        Ok((data, true))
    } else {
        Ok((data, false))
    }
}

fn io_error(action: &str, path: &Path, err: &io::Error) -> Error {
    Error::new(
        ErrorKind::Unreadable,
        format!("{} {}: {}", action, display(path), describe_io(err)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct FakeFs {
        open_errors: Vec<(String, io::ErrorKind)>,
    }

    impl Fs for FakeFs {
        fn metadata(&self, path: &Path) -> io::Result<Meta> {
            RealFs.metadata(path)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<String>> {
            RealFs.read_dir(path)
        }

        fn open(&self, path: &Path) -> io::Result<File> {
            let text = display(path);
            for (suffix, kind) in &self.open_errors {
                if text.ends_with(suffix) {
                    return Err(io::Error::from(*kind));
                }
            }
            RealFs.open(path)
        }

        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            RealFs.canonicalize(path)
        }
    }

    #[test]
    fn open_failure_becomes_an_unreadable_refusal() {
        let fs = FakeFs {
            open_errors: vec![("/x.md".to_string(), io::ErrorKind::PermissionDenied)],
        };
        let err = read_capped(&fs, Path::new("roster/x.md")).expect_err("must refuse");
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert_eq!(err.message(), "open roster/x.md: permission denied");
    }

    #[test]
    fn a_file_past_the_cap_is_reported_as_truncated() {
        let dir = std::env::temp_dir().join(format!("hearth-experts-cap-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("big.md");
        let mut file = File::create(&path).expect("create file");
        file.write_all(&vec![b'x'; MAX_FILE_BYTES + 1])
            .expect("write file");
        let (data, truncated) = read_capped(&RealFs, &path).expect("read");
        assert!(truncated);
        assert_eq!(data.len(), MAX_FILE_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_under_the_cap_is_not_reported_as_truncated() {
        let dir = std::env::temp_dir().join(format!("hearth-experts-capok-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("small.md");
        fs::write(&path, b"---\n---\nbody\n").expect("write file");
        let (data, truncated) = read_capped(&RealFs, &path).expect("read");
        assert!(!truncated);
        assert_eq!(data, b"---\n---\nbody\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
