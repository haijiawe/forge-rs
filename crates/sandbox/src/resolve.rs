//! Spec validation. Paths are resolved before any platform sees them because a
//! rule attached to a symlink applies to the link's target, and the target is
//! normally the broader thing.

use std::path::{Path, PathBuf};

use hearth_core::{Error, ErrorKind, Result as CoreResult};

use crate::{canonical_path, is_filesystem_root, Mode, Spec};

/// Validate a spec and return the canonical paths the platform implementations
/// must use. `full-access` ignores workspace and extra paths on purpose: it is
/// the one mode that is always available, and requiring paths would reject it.
pub(crate) fn resolve_spec(spec: Spec) -> CoreResult<Spec> {
    if !spec.mode.valid() {
        return Err(invalid(format!(
            "unknown mode {:?} (want {:?}, {:?} or {:?})",
            spec.mode.as_str(),
            Mode::ReadOnly.as_str(),
            Mode::WorkspaceWrite.as_str(),
            Mode::FullAccess.as_str()
        )));
    }
    if spec.mode == Mode::FullAccess {
        return Ok(spec);
    }

    let workspace = resolve_path("workspace", &spec.workspace)?;
    let metadata = std::fs::metadata(&workspace).map_err(|error| {
        invalid(format!(
            "workspace {:?}: {}",
            workspace.display().to_string(),
            error
        ))
    })?;
    if !metadata.is_dir() {
        return Err(invalid(format!(
            "workspace {:?} is not a directory",
            workspace.display().to_string()
        )));
    }
    if is_filesystem_root(&workspace) {
        return Err(invalid(format!(
            "workspace {:?} is a filesystem root, which workspace-write cannot mean anything narrower than full-access; request full-access explicitly",
            workspace.display().to_string()
        )));
    }

    let mut extra = Vec::with_capacity(spec.extra_write.len());
    for path in &spec.extra_write {
        let resolved = resolve_path("extra write path", path)?;
        if let Err(error) = std::fs::metadata(&resolved) {
            return Err(invalid(format!(
                "extra write path {:?}: {}",
                resolved.display().to_string(),
                error
            )));
        }
        extra.push(resolved.display().to_string());
    }

    // A read grant is resolved the same way, but a path that does not exist is
    // skipped rather than refused: these lists are assembled from what a machine
    // happens to have (`~/.gitconfig`, `%APPDATA%`), and a missing optional file
    // must not turn a working sandbox into a refusal to start.
    let mut reads = Vec::with_capacity(spec.extra_read.len());
    for path in &spec.extra_read {
        if let Ok(resolved) = resolve_path("extra read path", path) {
            if std::fs::metadata(&resolved).is_ok() {
                reads.push(resolved.display().to_string());
            }
        }
    }

    Ok(Spec {
        mode: spec.mode,
        workspace: workspace.display().to_string(),
        allow_net: spec.allow_net,
        extra_write: extra,
        extra_read: reads,
        wrapper: spec.wrapper.clone(),
    })
}

fn invalid(detail: String) -> Error {
    Error::new(
        ErrorKind::Invalid,
        format!("{}: {}", crate::ERR_INVALID_SPEC, detail),
    )
}

/// Require an absolute path that resolves. The resolved form is what gets used;
/// the string the caller passed is never attached to a rule.
fn resolve_path(field: &str, path: &str) -> CoreResult<PathBuf> {
    if path.is_empty() {
        return Err(invalid(format!("{} is empty", field)));
    }
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return Err(invalid(format!("{} {:?} is not absolute", field, path)));
    }
    canonical_path(candidate)
        .map_err(|error| invalid(format!("{} {:?} does not resolve: {}", field, path, error)))
}

impl Mode {
    pub(crate) fn valid(self) -> bool {
        matches!(
            self,
            Mode::ReadOnly | Mode::WorkspaceWrite | Mode::FullAccess
        )
    }
}
