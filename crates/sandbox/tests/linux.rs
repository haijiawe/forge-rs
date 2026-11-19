//! Linux sandbox tests that exercise Landlock against the running kernel.
//!
//! These are the Linux counterpart of the Windows AppContainer tests in
//! `sandbox_test.rs`. They start the helper binary as the child, let the wrapper
//! apply a Landlock ruleset to itself, and then observe what that child can
//! actually do. A policy that only compiles proves nothing: the kernel either
//! installed the ruleset or it did not, and only the child can report which.
//!
//! A kernel without Landlock is not a failure. `available()` says so, and the
//! mode matrix then asserts that the crate refuses a mode it cannot enforce
//! instead of quietly running unrestricted.

#![cfg(target_os = "linux")]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use forge_sandbox::{available, new, ErrorKind, Mode, Spec};

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir()
        .join("forge-sandbox-linux-it")
        .join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch directory");
    std::fs::canonicalize(path).expect("canonical scratch directory")
}

fn helper() -> &'static str {
    env!("CARGO_BIN_EXE_forge-sandbox-helper")
}

fn spec_in(mode: Mode, workspace: &Path) -> Spec {
    let mut spec = Spec::new(mode);
    spec.workspace = workspace.display().to_string();
    // The wrapper must be a program whose first statement is `maybe_exec_helper`;
    // in production that is `forge`, and here it is the helper child. The test
    // binary itself would answer its own argument parser instead.
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
    let enforcer = new(spec, None).expect("landlock must install on a supported kernel");
    let mut command = Command::new(helper());
    command.args(args);
    enforcer.apply(&mut command).expect("apply the wrapper");
    let output = command.output().expect("run the helper");
    enforcer.close().expect("close the enforcer");
    output
}

#[test]
fn the_mode_matrix_matches_what_the_kernel_can_do() {
    let support = available();
    let workspace = scratch("matrix");
    if support.filesystem {
        assert!(
            new(spec_in(Mode::ReadOnly, &workspace), None).is_ok(),
            "read-only must be enforceable when landlock reports filesystem support"
        );
        let enforcer =
            new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("workspace-write");
        assert!(enforcer.support().enforced);
        assert!(enforcer.support().filesystem);
        assert!(
            !enforcer.support().detail.contains("unverified"),
            "the Linux enforcer is exercised by this file now, so it must not call itself unverified"
        );
    } else {
        let error = new(spec_in(Mode::ReadOnly, &workspace), None)
            .err()
            .expect("read-only must be refused without landlock");
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(error.message().contains("landlock"));
        assert!(
            new(spec_in(Mode::WorkspaceWrite, &workspace), None).is_err(),
            "workspace-write has no real dimension to enforce here and must be refused"
        );
    }
}

#[test]
fn workspace_write_can_write_inside_and_not_outside() {
    if !available().filesystem {
        return;
    }
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
        "a write outside the workspace must be refused by the kernel"
    );
    assert!(
        !outside_file.exists(),
        "the refused write must not leave a file behind"
    );
}

#[test]
fn read_only_refuses_a_write_inside_the_workspace() {
    if !available().filesystem {
        return;
    }
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
    if !available().filesystem {
        return;
    }
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
fn network_is_confined_only_when_the_kernel_can_confine_it() {
    let support = available();
    if !support.filesystem {
        return;
    }
    if !support.network {
        eprintln!(
            "skipping the network half: landlock ABI < 4, so TCP bind/connect cannot be confined here"
        );
        return;
    }
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
        "allow_net = false must be refused by the kernel, got {outcome:?}"
    );

    // Allowed direction: without this half, a policy that broke all networking
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
