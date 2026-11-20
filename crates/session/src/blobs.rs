//! Attachment bodies, stored beside the log instead of inside it.
//!
//! An event line is a record of what happened, and every reader of the log —
//! projection, fork, the index, the browser — pays for its size. A base64 body
//! in a `user/message` line is paid for by all of them, forever, and it cannot
//! be deleted because the log is append-only. So a body lives here, under a
//! name, and the event carries the name.
//!
//! This module owns one directory and nothing else. It does not know what a
//! media type is, what session a blob belongs to, or whether anybody is allowed
//! to read it: those are the server's questions.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use hearth_core::{new_id, Error, ErrorKind, Result};

/// A directory of content-addressed-by-name file bodies.
pub struct Blobs {
    root: PathBuf,
}

impl Blobs {
    /// Open (creating on first write) the blob directory under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Blobs {
        Blobs { root: root.into() }
    }

    /// Where bodies live. Useful for a log line or a diagnostic.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store `bytes` and return the name to record in the log.
    ///
    /// The name is random, not a hash of the content: two identical uploads get
    /// two names, and deleting one (when deletion exists) cannot take the other
    /// with it. Deduplication would buy disk and cost that independence, which
    /// is the wrong trade for a file a person attached on purpose.
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        fs::create_dir_all(&self.root)?;
        let name = new_id()?;
        let target = self.root.join(&name);
        // Write beside the target and rename: a torn write must leave nothing
        // that a later `get` would serve as a whole attachment, and a rename is
        // the only step that makes the name visible.
        let temporary = self.root.join(format!(".{name}.tmp"));
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        match fs::rename(&temporary, &target) {
            Ok(()) => Ok(name),
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(Error::from(error))
            }
        }
    }

    /// Read a body back. A name that is not 32 hex characters is refused before
    /// it reaches the filesystem, so a caller cannot walk out of the directory.
    pub fn get(&self, name: &str) -> Result<Vec<u8>> {
        let path = self.path_for(name)?;
        Ok(fs::read(path)?)
    }

    /// The file a name refers to, checked to be inside the blob directory.
    pub fn path_for(&self, name: &str) -> Result<PathBuf> {
        if !is_valid_name(name) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("no attachment named {name:?}"),
            ));
        }
        Ok(self.root.join(name))
    }

    /// Whether a body is stored. Absent is not an error: a log can name a body
    /// that a partial copy of the data directory left behind.
    pub fn contains(&self, name: &str) -> bool {
        self.path_for(name)
            .map(|path| path.is_file())
            .unwrap_or(false)
    }
}

/// The only shape a blob name may have: 32 lowercase hex characters, which is
/// what [`new_id`] produces. Anything else is refused before it is joined to a
/// path, because `..` and separators must never reach the filesystem at all.
pub fn is_valid_name(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join("hearth-blobs-test").join(name);
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_body_round_trips_under_a_name_the_log_can_hold() {
        let root = scratch("round-trip");
        let blobs = Blobs::new(&root);
        let name = blobs.put(b"hello").expect("put");
        assert!(is_valid_name(&name), "{name}");
        assert_eq!(blobs.get(&name).expect("get"), b"hello");
        assert!(blobs.contains(&name));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_name_that_is_not_a_name_never_reaches_the_filesystem() {
        let blobs = Blobs::new(scratch("refuse"));
        for name in ["", "..", "../token", "abc", "../../etc/passwd", "ABCDEF"] {
            let error = blobs.get(name).expect_err("must refuse");
            assert!(error.message().contains("no attachment named"), "{error}");
        }
    }

    #[test]
    fn a_missing_body_is_reported_as_an_io_error_not_a_panic() {
        let blobs = Blobs::new(scratch("missing"));
        let name = new_id().expect("id");
        assert!(!blobs.contains(&name));
        assert!(blobs.get(&name).is_err());
    }

    #[test]
    fn a_failed_write_leaves_no_temporary_file_behind() {
        let root = scratch("no-temp");
        let blobs = Blobs::new(&root);
        let name = blobs.put(b"x").expect("put");
        let leftovers: Vec<_> = fs::read_dir(&root)
            .expect("read")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = fs::remove_file(blobs.path_for(&name).expect("path"));
        let _ = fs::remove_dir_all(&root);
    }
}
