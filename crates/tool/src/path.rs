//! Workspace-relative path resolution, ported from `forge-v2/internal/tool/path.go`.
//!
//! An explicit workspace is the only source of truth. Missing children are
//! reattached to the deepest existing ancestor after symlink resolution so a
//! symlinked parent cannot smuggle a path outside the workspace.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum PathError {
    EmptyWorkspace,
    Outside {
        workspace: PathBuf,
        candidate: PathBuf,
    },
    Io(io::Error),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWorkspace => f.write_str("workspace is empty"),
            Self::Outside {
                workspace,
                candidate,
            } => write!(
                f,
                "path {} is outside workspace {}",
                candidate.display(),
                workspace.display()
            ),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PathError {}

impl From<io::Error> for PathError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

pub fn is_outside(err: &PathError) -> bool {
    matches!(err, PathError::Outside { .. })
}

/// Resolve `p` under `workspace` after symlink expansion and containment checks.
pub fn resolve(
    workspace: impl AsRef<Path>,
    p: impl AsRef<Path>,
) -> std::result::Result<PathBuf, PathError> {
    let ws_raw = workspace.as_ref();
    if ws_raw.as_os_str().is_empty() {
        return Err(PathError::EmptyWorkspace);
    }

    let ws_abs = absolute(ws_raw)?;
    let ws_real = real_workspace(&ws_abs)?;

    let raw = p.as_ref();
    let raw = if raw.as_os_str().is_empty() {
        Path::new(".")
    } else {
        raw
    };
    let candidate = if raw.is_absolute() {
        clean(raw)
    } else {
        clean(&ws_abs.join(raw))
    };
    let resolved = resolve_existing(&candidate)?;

    if !contained(&ws_real, &resolved) {
        return Err(PathError::Outside {
            workspace: ws_real,
            candidate: resolved,
        });
    }
    Ok(resolved)
}

/// A slash-rendered workspace-relative path, or `None` when outside.
pub fn rel(workspace: impl AsRef<Path>, abs: impl AsRef<Path>) -> Option<String> {
    let ws = canonical_display(workspace.as_ref())?;
    let candidate = canonical_display(abs.as_ref())?;
    if candidate == ws {
        return Some(".".to_string());
    }
    let prefix = if ws.ends_with('/') {
        ws.clone()
    } else {
        format!("{ws}/")
    };
    candidate
        .strip_prefix(&prefix)
        .map(|tail| tail.trim_end_matches('/').to_string())
}

fn absolute(path: &Path) -> std::result::Result<PathBuf, PathError> {
    if path.is_absolute() {
        return Ok(clean(path));
    }
    let cwd = std::env::current_dir()?;
    Ok(clean(&cwd.join(path)))
}

fn real_workspace(abs: &Path) -> std::result::Result<PathBuf, PathError> {
    match resolve_existing(abs) {
        Ok(path) => Ok(path),
        Err(PathError::Io(err)) if err.kind() == io::ErrorKind::NotFound => Ok(abs.to_path_buf()),
        Err(err) => Err(err),
    }
}

fn resolve_existing(path: &Path) -> std::result::Result<PathBuf, PathError> {
    let mut cursor = path;
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        match std::fs::symlink_metadata(cursor) {
            Ok(_) => {
                let mut resolved = std::fs::canonicalize(cursor)?;
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else {
                    return Err(PathError::Io(err));
                };
                suffix.push(name.to_os_string());
                match cursor.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => cursor = parent,
                    _ => return Err(PathError::Io(err)),
                }
            }
            Err(err) => return Err(PathError::Io(err)),
        }
    }
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                let last_is_parent = out
                    .file_name()
                    .map(|name| name == std::ffi::OsStr::new(".."))
                    .unwrap_or(false);
                if last_is_parent || !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn canonical_display(path: &Path) -> Option<String> {
    let resolved = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(_) => absolute(path).ok()?,
    };
    let mut text = resolved.to_string_lossy().replace('\\', "/");
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    if cfg!(windows) {
        Some(text.to_lowercase())
    } else {
        Some(text)
    }
}

fn contained(workspace: &Path, candidate: &Path) -> bool {
    let (Some(ws), Some(path)) = (canonical_display(workspace), canonical_display(candidate))
    else {
        return false;
    };
    if ws == path {
        return true;
    }
    let prefix = if ws.ends_with('/') {
        ws
    } else {
        format!("{ws}/")
    };
    path.starts_with(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_escape() {
        let err = resolve(".", "../outside").unwrap_err();
        assert!(is_outside(&err), "{err}");
    }

    #[test]
    fn allows_missing_child_inside_workspace() {
        let path = resolve(".", "a/b/c.txt").unwrap();
        assert!(path.ends_with("a/b/c.txt") || path.ends_with("a\\b\\c.txt"));
    }
}
