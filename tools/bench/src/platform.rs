//! Platform identity and repo-root discovery.
//!
//! The baseline document is language-neutral: Rust arch names are mapped to
//! the Go names (`x86_64` -> `amd64`, `aarch64` -> `arm64`) so a record can be
//! compared across implementations of the same harness.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn os() -> &'static str {
    std::env::consts::OS
}

pub fn arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "amd64".to_string(),
        "aarch64" => "arm64".to_string(),
        "x86" => "386".to_string(),
        other => other.to_string(),
    }
}

/// The running compiler version, or a stable fallback when `rustc` is not on
/// PATH. Only used in the human-readable rustVersion note; it never changes a
/// gate decision.
pub fn rust_version() -> String {
    match Command::new("rustc").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                fallback_rust_version()
            } else {
                text
            }
        }
        _ => fallback_rust_version(),
    }
}

fn fallback_rust_version() -> String {
    format!("rustc {}", env!("CARGO_PKG_RUST_VERSION"))
}

/// Walk up from `cwd` to the repo root, identified by both `Cargo.toml` and
/// `architecture.yaml` so a nested crate directory cannot be mistaken for it.
pub fn discover_root(cwd: &Path) -> Result<PathBuf, String> {
    let mut current = cwd.to_path_buf();
    loop {
        if current.join("Cargo.toml").is_file() && current.join("architecture.yaml").is_file() {
            return Ok(current);
        }
        if !current.pop() {
            return Err(
                "bench: cannot find repo root (Cargo.toml + architecture.yaml)".to_string(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_names_are_non_empty_and_arch_is_go_shaped() {
        assert!(!os().is_empty());
        let a = arch();
        assert!(!a.is_empty());
        assert_ne!(a, "x86_64");
        assert_ne!(a, "aarch64");
    }

    #[test]
    fn rust_version_never_empty() {
        assert!(!rust_version().is_empty());
        assert!(!fallback_rust_version().is_empty());
    }

    #[test]
    fn discover_root_finds_repository_from_a_nested_path() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            discover_root(here).unwrap(),
            here.parent().unwrap().parent().unwrap()
        );
    }
}
