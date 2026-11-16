//! What a sandboxed tool process is allowed to touch outside its workspace.
//!
//! Two facts force this file to exist:
//!
//! * On Windows an AppContainer starts with *nothing* readable, so a contained `git`
//!   cannot open `~/.gitconfig`, `npm` cannot read `%APPDATA%\npm`, and a build that
//!   wants a scratch file cannot write to `%TEMP%`. The sandbox is then working
//!   exactly as designed and the tools are still unusable.
//! * The set that fixes this is a product decision, not a kernel detail, so it has to
//!   be visible, reviewable and replaceable. `sandboxAccess.read` / `.write` in
//!   `config.json` replaces the defaults; an explicit empty list means "grant nothing
//!   extra", which is the only way to ask for the strict behaviour.
//!
//! Paths that do not exist are dropped rather than refused (see `resolve.rs`): the
//! lists below name what a *typical* machine has, and a machine without `.npmrc` must
//! not be a machine where the sandbox refuses to start.

use std::path::{Path, PathBuf};

use forge_config::Config;

/// Read grants for the platforms that deny reads by default.
///
/// Deliberately a list of tool-owned paths rather than the whole home directory: the
/// point is to let tools find their configuration, not to hand a model access to
/// every credential on the machine. `~/.ssh` is a directory, and nothing here grants
/// it.
const READ_CANDIDATES: &[&str] = &[
    // git
    ".gitconfig",
    ".gitignore_global",
    ".gitmodules",
    // rust
    ".cargo",
    ".rustup",
    // node
    ".npmrc",
    ".npm",
    "AppData/Roaming/npm",
    "AppData/Roaming/npm-cache",
    // python
    ".python_history",
    "AppData/Roaming/pip",
    "AppData/Local/pip",
    // generic
    ".config",
];

/// Write grants. `%TEMP%` is the one directory tools genuinely need to write: an
/// editor, a linker and a package manager all put scratch files there, and none of
/// them ask first.
const WRITE_CANDIDATES: &[&str] = &["tmp"];

/// Resolve the effective grants for this configuration and host.
///
/// Returns `(read, write)`, both absolute and both possibly empty.
pub fn effective(cfg: &Config) -> (Vec<String>, Vec<String>) {
    let home = home_dir();
    let read = match &cfg.sandbox_access.read {
        Some(explicit) => expand_all(explicit, home.as_deref()),
        None => candidates(home.as_deref(), READ_CANDIDATES),
    };
    let write = match &cfg.sandbox_access.write {
        Some(explicit) => expand_all(explicit, home.as_deref()),
        None => candidates(home.as_deref(), WRITE_CANDIDATES),
    };
    (read, write)
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

fn temp_dir() -> Option<PathBuf> {
    std::env::var_os("TEMP")
        .or_else(|| std::env::var_os("TMPDIR"))
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| Some(std::env::temp_dir()).filter(|p| p.is_dir()))
}

/// Join each candidate onto the home directory and keep the ones that exist.
///
/// A candidate of `"tmp"` refers to the temp directory, not to `~/tmp`, because that is
/// what tools actually use.
fn candidates(home: Option<&Path>, list: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in list {
        let path = if *entry == "tmp" {
            temp_dir()
        } else {
            home.map(|root| root.join(entry))
        };
        if let Some(path) = path {
            if path.exists() {
                out.push(path.display().to_string());
            }
        }
    }
    out
}

/// Expand `~` and drop everything that does not exist.
fn expand_all(entries: &[String], home: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        let expanded = if let Some(rest) = entry.strip_prefix("~/") {
            match home {
                Some(root) => root.join(rest),
                None => continue,
            }
        } else if entry == "~" {
            match home {
                Some(root) => root.to_path_buf(),
                None => continue,
            }
        } else {
            PathBuf::from(entry)
        };
        if expanded.exists() {
            out.push(expanded.display().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_lists_replace_the_defaults() {
        let mut cfg = Config::defaults();
        cfg.sandbox_access.read = Some(Vec::new());
        cfg.sandbox_access.write = Some(Vec::new());
        let (read, write) = effective(&cfg);
        // Empty means "grant nothing extra"; it must not fall back to the built-in
        // list, or narrowing the sandbox would be impossible.
        assert!(read.is_empty(), "got {read:?}");
        assert!(write.is_empty(), "got {write:?}");
    }

    #[test]
    fn missing_paths_are_dropped_not_refused() {
        let mut cfg = Config::defaults();
        cfg.sandbox_access.read = Some(vec![
            "/definitely/not/here/one".to_string(),
            "/definitely/not/here/two".to_string(),
        ]);
        let (read, _) = effective(&cfg);
        assert!(read.is_empty(), "got {read:?}");
    }

    #[test]
    fn the_defaults_are_absolute_and_exist() {
        let (read, write) = effective(&Config::defaults());
        for path in read.iter().chain(write.iter()) {
            assert!(Path::new(path).is_absolute(), "{path} is not absolute");
            assert!(Path::new(path).exists(), "{path} does not exist");
        }
    }
}
