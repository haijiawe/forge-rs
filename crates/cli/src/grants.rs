//! What a sandboxed tool process is allowed to touch outside its workspace, and
//! whether it may use the network.
//!
//! Three facts force this file to exist:
//!
//! * On Windows an AppContainer starts with *nothing* readable, so a contained `git`
//!   cannot open `~/.gitconfig`, `npm` cannot read `%APPDATA%\npm`, and a build that
//!   wants a scratch file cannot write to `%TEMP%`. The sandbox is then working
//!   exactly as designed and the tools are still unusable.
//! * The set that fixes this is a product decision, not a kernel detail, so it has to
//!   be visible, reviewable and replaceable. `sandboxAccess.read` / `.write` in
//!   `config.json` replaces the defaults; an explicit empty list means "grant nothing
//!   extra", which is the only way to ask for the strict behaviour.
//! * Network access is the same kind of decision and travels with the other two, so
//!   one call answers all of it: [`Grants`]. Splitting it into two calls would let a
//!   caller set the paths and forget the switch, which is the bug this file exists to
//!   avoid.
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

/// Everything one sandboxed execution gets from the config.
///
/// One struct rather than three return values: the three answer one question ("what
/// may this child touch") and a caller that had to remember a third name to set
/// `Spec::allow_net` would eventually forget it, leaving a switch that never moves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    /// Absolute paths the child may read outside its workspace.
    pub read: Vec<String>,
    /// Absolute paths the child may write outside its workspace.
    pub write: Vec<String>,
    /// Whether the child may use the network. Travels with the paths so the
    /// sandbox spec can be filled from one source.
    pub allow_net: bool,
}

/// Resolve the effective grants for this configuration and host.
///
/// Both lists are absolute and possibly empty. `allow_net` comes from
/// [`Config::sandbox_allow_net`], which is `false` unless the file says `true`.
pub fn effective(cfg: &Config) -> Grants {
    let home = home_dir();
    let read = match &cfg.sandbox_access.read {
        Some(explicit) => expand_all(explicit, home.as_deref()),
        None => candidates(home.as_deref(), READ_CANDIDATES),
    };
    let write = match &cfg.sandbox_access.write {
        Some(explicit) => expand_all(explicit, home.as_deref()),
        None => candidates(home.as_deref(), WRITE_CANDIDATES),
    };
    Grants {
        read,
        write,
        allow_net: cfg.sandbox_allow_net(),
    }
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
        let grants = effective(&cfg);
        // Empty means "grant nothing extra"; it must not fall back to the built-in
        // list, or narrowing the sandbox would be impossible.
        assert!(grants.read.is_empty(), "got {:?}", grants.read);
        assert!(grants.write.is_empty(), "got {:?}", grants.write);
    }

    #[test]
    fn missing_paths_are_dropped_not_refused() {
        let mut cfg = Config::defaults();
        cfg.sandbox_access.read = Some(vec![
            "/definitely/not/here/one".to_string(),
            "/definitely/not/here/two".to_string(),
        ]);
        let grants = effective(&cfg);
        assert!(grants.read.is_empty(), "got {:?}", grants.read);
    }

    #[test]
    fn the_defaults_are_absolute_and_exist() {
        let grants = effective(&Config::defaults());
        for path in grants.read.iter().chain(grants.write.iter()) {
            assert!(Path::new(path).is_absolute(), "{path} is not absolute");
            assert!(Path::new(path).exists(), "{path} does not exist");
        }
    }

    // -----------------------------------------------------------------------
    // The network switch travels with the paths
    // -----------------------------------------------------------------------

    #[test]
    fn the_network_is_denied_by_default() {
        // Nothing in the file means no outbound sockets, which is the sandbox's
        // own default. A default that granted the network would make the switch
        // meaningless on the day it was added.
        let grants = effective(&Config::defaults());
        assert!(!grants.allow_net, "the default must deny the network");
    }

    #[test]
    fn an_explicit_true_wins_over_the_default() {
        let mut cfg = Config::defaults();
        cfg.sandbox_access.network = Some(true);
        let grants = effective(&cfg);
        assert!(grants.allow_net, "an explicit true must be honoured");
        // And it does not disturb the paths: one member's presence is not a
        // statement about the other two.
        assert!(
            !grants.read.is_empty(),
            "the default read grants must survive `network: true`"
        );
    }

    #[test]
    fn an_explicit_false_stays_false_even_under_a_permissive_default() {
        // The scenario this shape exists for: a caller (or a future default) hands
        // in a config whose `network` member is written as false while everything
        // else is wide open. The written false must win, not be re-derived.
        let mut cfg = Config::defaults();
        cfg.sandbox = forge_config::SANDBOX_FULL_ACCESS.to_string();
        cfg.sandbox_access.read = Some(Vec::new());
        cfg.sandbox_access.write = Some(Vec::new());
        cfg.sandbox_access.network = Some(false);
        let grants = effective(&cfg);
        assert!(!grants.allow_net, "an explicit false must stay false");
        assert_eq!(grants.allow_net, cfg.sandbox_allow_net());
    }
}
