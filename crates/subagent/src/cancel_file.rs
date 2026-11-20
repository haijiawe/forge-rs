//! Cross-process interrupt requests, carried as a file in the data directory.
//!
//! `Runner::interrupt` cancels a child that is running in *this* process by
//! tripping the cancel token it registered under the child's id. A `forge
//! subagent interrupt <id>` invocation is a different process, so it cannot see
//! that token. Instead it drops a marker file next to the child's log:
//! `<data_dir>/sessions/<id>.cancel`. A `Runner` that is running that child
//! watches for the marker (see `runner::Runner::watch_cancel`) and trips its own
//! token, which is the same path the in-process interrupt takes. The marker is
//! removed once honoured, so it cannot cancel a later run of the same child.

use std::path::{Path, PathBuf};

use forge_core::{Error, ErrorKind, Result};

use crate::child::is_child_id;

/// `<data_dir>/sessions/<child_id>.cancel`.
pub fn cancel_path(data_dir: &Path, child_id: &str) -> PathBuf {
    data_dir.join("sessions").join(format!("{child_id}.cancel"))
}

/// Write the interrupt marker. Rejects a hostile id before building a path, so
/// the marker can never be written outside the sessions directory.
pub fn request(data_dir: &Path, child_id: &str) -> Result<PathBuf> {
    if !is_child_id(child_id) {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("subagent: not a child id: {child_id:?}"),
        ));
    }
    let path = cancel_path(data_dir, child_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            Error::new(
                ErrorKind::Io,
                format!("subagent: cannot create {}: {e}", dir.display()),
            )
        })?;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::fs::write(&path, format!("{stamp}\n")).map_err(|e| {
        Error::new(
            ErrorKind::Io,
            format!("subagent: cannot write {}: {e}", path.display()),
        )
    })?;
    Ok(path)
}

/// Whether an interrupt has been requested for this child.
pub fn is_requested(data_dir: &Path, child_id: &str) -> bool {
    cancel_path(data_dir, child_id).is_file()
}

/// Remove the marker (after it has been honoured, or to abandon a request).
/// Returns whether a marker existed.
pub fn clear(data_dir: &Path, child_id: &str) -> bool {
    std::fs::remove_file(cancel_path(data_dir, child_id)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("forge-cancel-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn a_request_creates_the_marker_and_clear_removes_it() {
        let root = dir("roundtrip");
        let id = "20260101-000000-deadbeef";
        assert!(!is_requested(&root, id));
        let path = request(&root, id).expect("request");
        assert!(path.is_file());
        assert!(is_requested(&root, id));
        assert!(clear(&root, id));
        assert!(!is_requested(&root, id));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hostile_id_is_refused_before_any_path_is_built() {
        let root = dir("hostile");
        let err = request(&root, "../../etc/passwd").expect_err("must refuse");
        assert_eq!(err.kind(), ErrorKind::Invalid);
        // Nothing was written outside the sessions dir.
        assert!(!root.join("sessions").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
