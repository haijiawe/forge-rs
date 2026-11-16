//! Captures the compiler version for `forge --version` and the doctor report.
//!
//! Why a build script: the Go build stamped `runtime.Version()` at build time.
//! Rust has no runtime version string, and printing a hard-coded one would go
//! stale the moment the toolchain changes, so the value is read once from the
//! compiler that actually builds this binary.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let version = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string()))
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=FORGE_RUSTC_VERSION={version}");
}
