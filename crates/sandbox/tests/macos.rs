//! macOS sandbox tests that exercise `sandbox-exec` against the running host.
//!
//! These are the macOS counterpart of the Linux Landlock tests in `linux.rs`.
//! They start the helper binary under a Seatbelt profile written by
//! `crates/sandbox/src/darwin.rs`, then observe what that child can actually do.
//! A profile that merely parses proves nothing: only the sandboxed child can
//! report whether a write or a socket was allowed.
//!
//! `darwin.rs` grants `file-read*` unconditionally, so reads are never confined
//! and `extra_read` is a no-op there; the tests below therefore exercise the
//! write and network dimensions the implementation actually enforces. Unlike
//! Landlock there is no partial kernel capability to skip around: if
//! `/usr/bin/sandbox-exec` is missing or refuses to run, every test fails with
//! an explicit message rather than passing without evidence.

#![cfg(target_os = "macos")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hearth_sandbox::{available, new, Mode, Spec};

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir()
        .join("hearth-sandbox-macos-it")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch directory");
    std::fs::canonicalize(path).expect("canonical scratch directory")
}

fn helper() -> &'static str {
    env!("CARGO_BIN_EXE_hearth-sandbox-helper")
}

fn spec_in(mode: Mode, workspace: &Path) -> Spec {
    let mut spec = Spec::new(mode);
    spec.workspace = workspace.display().to_string();
    // `darwin.rs` does not re-exec the harness: `/usr/bin/sandbox-exec` is the
    // wrapper, so `Spec.wrapper` is inert here. It is still set so a future
    // darwin implementation that gains a re-exec path wraps the helper child,
    // not the test harness, which would answer its own argument parser.
    spec.wrapper = Some(helper().to_string());
    spec
}

fn run(
    mode: Mode,
    workspace: &Path,
    extra_write: &[&Path],
    allow_net: bool,
    args: &[&str],
) -> Output {
    let mut spec = spec_in(mode, workspace);
    spec.allow_net = allow_net;
    spec.extra_write = extra_write
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let enforcer = new(spec, None).expect("sandbox-exec must install on macOS");
    let mut command = Command::new(helper());
    command.args(args);
    enforcer
        .apply(&mut command)
        .expect("apply the seatbelt profile");
    let output = command.output().expect("run the helper");
    enforcer.close().expect("close the enforcer");
    output
}

#[test]
fn the_mode_matrix_matches_what_sandbox_exec_can_do() {
    let support = available();
    assert!(
        support.filesystem && support.network,
        "the macOS tests need /usr/bin/sandbox-exec; available() says: {}",
        support.detail
    );
    let workspace = scratch("matrix");

    let read_only = new(spec_in(Mode::ReadOnly, &workspace), None)
        .expect("read-only must install when sandbox-exec is available");
    assert!(read_only.support().enforced);
    assert!(read_only.support().filesystem);
    read_only.close().expect("close read-only");

    let workspace_write = new(spec_in(Mode::WorkspaceWrite, &workspace), None)
        .expect("workspace-write must install when sandbox-exec is available");
    let installed = workspace_write.support();
    assert!(installed.enforced);
    assert!(installed.filesystem);
    assert!(installed.network);
    assert!(
        installed.detail.contains("sandbox-exec"),
        "the installed enforcer must name its mechanism, got {:?}",
        installed.detail
    );
    workspace_write.close().expect("close workspace-write");

    // A written profile is not evidence that sandbox-exec accepted it. Run a
    // trivial command through both modes so a refusal is a red test here, not a
    // puzzling "write denied" result in a later test.
    for mode in [Mode::ReadOnly, Mode::WorkspaceWrite] {
        let output = run(mode, &workspace, &[], false, &["sleep", "10"]);
        assert!(
            output.status.success(),
            "sandbox-exec must run a trivial command under {mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // The label was removed on 2026-11-20, after this file ran green in the
    // `sandbox macos-latest` job. Asserting its absence is what keeps the label
    // from returning without evidence.
    assert!(
        !installed.detail.contains("unverified"),
        "the macOS enforcer is exercised by this file now, so it must not call itself unverified"
    );
}

#[test]
fn workspace_write_can_write_inside_and_not_outside() {
    let workspace = scratch("write-scope");
    let outside = scratch("write-scope-outside");
    let inside = workspace.join("inside.txt");
    let outside_file = outside.join("outside.txt");

    let output = run(
        Mode::WorkspaceWrite,
        &workspace,
        &[],
        false,
        &["write", inside.to_str().expect("utf-8 path")],
    );
    assert!(
        output.status.success(),
        "a write inside the workspace must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&inside).expect("read back"),
        "written"
    );

    let output = run(
        Mode::WorkspaceWrite,
        &workspace,
        &[],
        false,
        &["write", outside_file.to_str().expect("utf-8 path")],
    );
    assert!(
        !output.status.success(),
        "a write outside the workspace must be refused by seatbelt"
    );
    assert!(
        !outside_file.exists(),
        "the refused write must not leave a file behind"
    );
}

#[test]
fn read_only_refuses_a_write_inside_the_workspace() {
    let workspace = scratch("read-only");
    let inside = workspace.join("inside.txt");
    let output = run(
        Mode::ReadOnly,
        &workspace,
        &[],
        false,
        &["write", inside.to_str().expect("utf-8 path")],
    );
    assert!(
        !output.status.success(),
        "read-only must refuse every write, including one inside the workspace"
    );
    assert!(!inside.exists());
}

#[test]
fn extra_write_paths_are_granted_by_name() {
    let workspace = scratch("extra-workspace");
    let granted = scratch("extra-granted");
    let file = granted.join("granted.txt");
    let output = run(
        Mode::WorkspaceWrite,
        &workspace,
        &[granted.as_path()],
        false,
        &["write", file.to_str().expect("utf-8 path")],
    );
    assert!(
        output.status.success(),
        "an explicitly granted path must be writable: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("read back"),
        "written"
    );
}

#[test]
fn network_denies_by_default_and_allows_when_requested() {
    assert!(
        available().network,
        "the macOS tests need /usr/bin/sandbox-exec; available() says: {}",
        available().detail
    );
    let workspace = scratch("network");

    // Denied direction: allow_net = false must make the connect fail.
    let denied_marker = workspace.join("denied.txt");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a listener");
    let address = listener.local_addr().expect("local address").to_string();
    let _ = std::thread::spawn(move || {
        let _ = listener.accept();
    });
    let output = run(
        Mode::WorkspaceWrite,
        &workspace,
        &[],
        false,
        &[
            "connect",
            &address,
            denied_marker.to_str().expect("utf-8 path"),
        ],
    );
    assert!(
        output.status.success(),
        "the helper itself must run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome = std::fs::read_to_string(&denied_marker).expect("read the outcome");
    assert!(
        outcome.to_lowercase().contains("permission denied"),
        "allow_net = false must be refused by seatbelt, got {outcome:?}"
    );

    // Allowed direction: without this half, a profile that broke all networking
    // would still pass the test above.
    let allowed_marker = workspace.join("allowed.txt");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a listener");
    let address = listener.local_addr().expect("local address").to_string();
    let _ = std::thread::spawn(move || {
        let _ = listener.accept();
    });
    let output = run(
        Mode::WorkspaceWrite,
        &workspace,
        &[],
        true,
        &[
            "connect",
            &address,
            allowed_marker.to_str().expect("utf-8 path"),
        ],
    );
    assert!(
        output.status.success(),
        "the helper itself must run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome = std::fs::read_to_string(&allowed_marker).expect("read the outcome");
    assert_eq!(
        outcome, "connected",
        "allow_net = true must leave the socket usable"
    );
}
