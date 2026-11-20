//! Behaviour tests for the sandbox crate on the host that runs them.
//!
//! The mode matrix is asserted against the real platform. On this Windows host an
//! AppContainer makes `read-only` and `workspace-write` both enforceable, and the
//! job object adds the process tree and memory caps on top. The measured job-object
//! tests start the helper binary so a grandchild is a real grandchild, not a mock.
//!
//! Every child here runs inside the container, so a path it writes has to be a path
//! the container may write: the markers live in the workspace, and the program
//! itself is copied into the workspace first, because a binary under the build
//! directory is not readable from inside a package identity.
//!
//! Each test gets its own scratch directory. Sharing one would make the profile
//! (named after the workspace) and the copied helper common property, and two tests
//! creating a profile or replacing an executable at the same moment is a race the
//! sandbox is not responsible for.

#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use hearth_sandbox::available;
use hearth_sandbox::{new, ErrorKind, Mode, Spec};

// Only the Windows job-object tests start a child process, so these helpers would
// be dead code when the test binary is cross-compiled to Linux or macOS.
#[cfg(windows)]
fn scratch(name: &str) -> PathBuf {
    // No pid in the path on purpose. The package identity is derived from the
    // workspace path, so a stable path keeps the grants (and the profile) across
    // runs: a random one would pay the one-time ACL cost of its ancestors every
    // single run, which measured tens of seconds per directory.
    let path = std::env::temp_dir().join("hearth-sandbox-it").join(name);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch directory");
    std::fs::canonicalize(path).expect("canonical scratch directory")
}

#[cfg(windows)]
fn spec_in(mode: Mode, workspace: &Path) -> Spec {
    let mut spec = Spec::new(mode);
    spec.workspace = workspace.display().to_string();
    // The wrapper must be a program whose first statement is `maybe_exec_helper`;
    // in production that is `hearth`, and in these tests it is the helper child.
    spec.wrapper = Some(helper().to_string());
    spec
}

#[cfg(windows)]
fn helper() -> &'static str {
    env!("CARGO_BIN_EXE_hearth-sandbox-helper")
}

/// The helper as the *container* sees it: a copy inside the workspace, because a
/// package identity may not read the build directory it was started from.
#[cfg(windows)]
fn contained_helper(workspace: &Path) -> String {
    let source = Path::new(helper());
    let destination = workspace.join(source.file_name().expect("helper file name"));
    std::fs::copy(source, &destination).expect("copy the helper into the workspace");
    destination.display().to_string()
}

/// A marker path inside the workspace, which is the only place a contained child
/// may write.
#[cfg(windows)]
fn marker(workspace: &Path, name: &str) -> PathBuf {
    let path = workspace.join(format!("marker-{name}.txt"));
    let _ = std::fs::remove_file(&path);
    path
}

#[cfg(windows)]
fn wait_for(path: &Path, timeout: Duration) -> bool {
    // Wait for a non-empty marker, not merely an existing file. The helper creates
    // the file before it connects and writes the verdict after, so under parallel
    // load an empty file can exist for a moment; treating that as "ready" would read
    // a blank verdict and report a false failure. The assertion still requires the
    // real outcome, so this only removes the race, it does not weaken the check.
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.trim().is_empty() {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[test]
fn full_access_installs_the_noop_enforcer() {
    let enforcer = new(Spec::new(Mode::FullAccess), None).expect("full access is always available");
    assert_eq!(enforcer.name(), "none");
    let support = enforcer.support();
    assert!(!support.enforced);
    assert!(!support.filesystem);
    assert!(enforcer.close().is_ok());
}

#[test]
fn invalid_spec_is_an_invalid_error() {
    let mut spec = Spec::new(Mode::ReadOnly);
    spec.workspace = "not-absolute".to_string();
    let error = new(spec, None).err().expect("invalid spec must fail");
    assert_eq!(error.kind(), ErrorKind::Invalid);
}

#[cfg(windows)]
#[test]
fn windows_mode_matrix_on_this_host() {
    // The claim and the mechanism must agree. Before the AppContainer landed this
    // test asserted `!support.filesystem` — the honest answer while the job object
    // was the whole story — and a Windows box without package identities still has
    // to say exactly that.
    let support = available();
    assert_eq!(
        support.filesystem,
        hearth_sandbox::appcontainer_available(),
        "filesystem containment must be claimed exactly when a container can be built"
    );
    assert!(support.process_tree, "job objects always contain the tree");

    let workspace = scratch("matrix");
    if !support.filesystem {
        let error = new(spec_in(Mode::ReadOnly, &workspace), None)
            .err()
            .expect("read-only must be refused without a container");
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(error.message().contains("filesystem"));
        let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None)
            .expect("partial enforcement is accepted");
        assert!(!enforcer.support().filesystem);
        assert!(enforcer.support().detail.contains("NOT confined"));
        let _ = enforcer.close();
        return;
    }

    assert!(support.network, "capabilities name the network");
    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None)
        .expect("workspace-write is enforcible");
    let installed = enforcer.support();
    assert!(installed.enforced);
    assert!(installed.filesystem);
    assert!(installed.process_tree);
    assert!(enforcer.close().is_ok());

    let read_only =
        new(spec_in(Mode::ReadOnly, &workspace), None).expect("read-only is enforcible");
    assert!(read_only.support().filesystem);
    assert!(read_only.close().is_ok());
}

#[cfg(windows)]
#[test]
fn windows_job_starts_suspended_and_resumes_after_adoption() {
    let workspace = scratch("resume");
    let marker = marker(&workspace, "resume");

    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");
    let mut command = std::process::Command::new(contained_helper(&workspace));
    command.args(["emit", marker.to_str().expect("marker path")]);
    enforcer.apply(&mut command).expect("apply");
    let mut child = command.spawn().expect("spawn suspended child");

    std::thread::sleep(Duration::from_millis(800));
    assert!(
        !marker.exists(),
        "the child executed before AfterStart; CREATE_SUSPENDED was lost"
    );

    enforcer.after_start(child.id()).expect("adopt");
    assert!(
        wait_for(&marker, Duration::from_secs(5)),
        "the child never resumed after job adoption"
    );

    let _ = enforcer.close();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
#[test]
fn windows_job_close_kills_grandchildren() {
    let workspace = scratch("grandchildren");
    let parent_marker = marker(&workspace, "parent");
    let grandchild_marker = marker(&workspace, "grandchild");

    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");
    let mut command = std::process::Command::new(contained_helper(&workspace));
    command.args([
        "spawn",
        parent_marker.to_str().expect("parent marker"),
        grandchild_marker.to_str().expect("grandchild marker"),
    ]);
    enforcer.apply(&mut command).expect("apply");
    let mut child = command.spawn().expect("spawn");
    enforcer.after_start(child.id()).expect("adopt");

    let spawned = PathBuf::from(format!("{}.grandchild", parent_marker.display()));
    assert!(
        wait_for(&spawned, Duration::from_secs(5)),
        "the helper never reported its grandchild"
    );

    enforcer.close().expect("close job");
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !grandchild_marker.exists(),
        "the grandchild outlived the job; process-tree containment is broken"
    );
    let _ = child.wait();
}

#[cfg(windows)]
#[test]
fn windows_job_caps_active_processes() {
    let workspace = scratch("active-processes");
    let marker = marker(&workspace, "count");

    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");
    let mut command = std::process::Command::new(contained_helper(&workspace));
    command.args(["spawn-many", "80", marker.to_str().expect("marker path")]);
    enforcer.apply(&mut command).expect("apply");
    let mut child = command.spawn().expect("spawn");
    enforcer.after_start(child.id()).expect("adopt");

    assert!(
        wait_for(&marker, Duration::from_secs(30)),
        "the helper never finished spawning"
    );
    let spawned: usize = std::fs::read_to_string(&marker)
        .expect("read marker")
        .trim()
        .parse()
        .expect("parse count");
    assert!(
        spawned < 80,
        "the job's 64-process cap did not reject any of 80 spawns (spawned {spawned})"
    );

    let _ = enforcer.close();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
#[test]
fn windows_job_caps_process_memory() {
    let workspace = scratch("memory");
    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");
    let mut command = std::process::Command::new(contained_helper(&workspace));
    command.args(["allocate", "1536"]);
    enforcer.apply(&mut command).expect("apply");
    let mut child = command.spawn().expect("spawn");
    enforcer.after_start(child.id()).expect("adopt");

    let start = Instant::now();
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(status) => break status,
            None if start.elapsed() > Duration::from_secs(60) => {
                let _ = child.kill();
                panic!("a 1.5 GiB allocation under a 1 GiB per-process cap did not terminate");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert!(
        !status.success(),
        "a 1.5 GiB allocation succeeded under a 1 GiB job-object memory cap"
    );
    let _ = enforcer.close();
}

#[cfg(windows)]
#[test]
fn windows_abort_apply_releases_the_queued_job() {
    let workspace = scratch("abort");
    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");

    let mut abandoned = std::process::Command::new(contained_helper(&workspace));
    abandoned.args(["sleep", "60000"]);
    enforcer.apply(&mut abandoned).expect("apply first");
    enforcer.abort_apply();

    let marker = marker(&workspace, "abort");
    let mut command = std::process::Command::new(contained_helper(&workspace));
    command.args(["emit", marker.to_str().expect("marker path")]);
    enforcer.apply(&mut command).expect("apply after abort");
    let mut child = command.spawn().expect("spawn");
    enforcer.after_start(child.id()).expect("adopt");
    assert!(
        wait_for(&marker, Duration::from_secs(5)),
        "the run after AbortApply adopted the stale job or never resumed"
    );

    let _ = enforcer.close();
    let _ = child.kill();
    let _ = child.wait();
}

/// Run the contained helper in `connect` mode against `address` and return what it
/// wrote into its marker: `connected`, or `refused: <kind>`.
#[cfg(windows)]
fn connect_outcome(spec: Spec, workspace: &Path, address: &str, name: &str) -> String {
    let marker = marker(workspace, name);
    let enforcer = new(spec, None).expect("install");
    let mut command = std::process::Command::new(contained_helper(workspace));
    command.args(["connect", address, marker.to_str().expect("marker path")]);
    enforcer.apply(&mut command).expect("apply");
    let mut child = command.spawn().expect("spawn");
    enforcer.after_start(child.id()).expect("adopt");
    assert!(
        wait_for(&marker, Duration::from_secs(30)),
        "the contained child never reported a connect result"
    );
    let outcome = std::fs::read_to_string(&marker).expect("read marker");
    let _ = enforcer.close();
    let _ = child.kill();
    let _ = child.wait();
    outcome
}

/// Whether this *host* (outside any sandbox) can open a TCP connection to `address`.
///
/// The positive half of [`windows_network_follows_the_spec_switch`] needs a target
/// that is reachable at all; a machine behind a network that drops it cannot tell a
/// working capability grant from a dead route, and inventing a pass there would be a
/// fixture that changes what is asserted.
#[cfg(windows)]
fn host_reaches(address: &str) -> bool {
    std::net::TcpStream::connect_timeout(
        &address.parse().expect("socket address"),
        Duration::from_secs(3),
    )
    .is_ok()
}

#[cfg(windows)]
#[test]
fn windows_network_is_absent_without_the_capability() {
    // A listener on loopback is the cheapest target, and an AppContainer without
    // `internetClient` may not reach it either: Windows treats loopback as network.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("listener address").to_string();

    let workspace = scratch("network");
    let marker = marker(&workspace, "network");

    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");
    assert!(
        enforcer.support().detail.contains("no capability"),
        "the spec under test must not grant the network: {}",
        enforcer.support().detail
    );
    let mut command = std::process::Command::new(contained_helper(&workspace));
    command.args(["connect", &address, marker.to_str().expect("marker path")]);
    enforcer.apply(&mut command).expect("apply");
    let mut child = command.spawn().expect("spawn");
    enforcer.after_start(child.id()).expect("adopt");

    assert!(
        wait_for(&marker, Duration::from_secs(20)),
        "the contained child never reported a connect result"
    );
    let outcome = std::fs::read_to_string(&marker).expect("read marker");
    assert!(
        outcome.starts_with("refused"),
        "a contained child reached {address} with no network capability: {outcome}"
    );

    let _ = enforcer.close();
    let _ = child.kill();
    let _ = child.wait();
    drop(listener);
}

#[cfg(windows)]
#[test]
fn windows_network_follows_the_spec_switch() {
    // `Spec::allow_net` has to change behaviour, not only the support string. This
    // test runs the same contained child against the same targets twice, and the only
    // difference between the runs is the switch.
    //
    // Measured on this host (2026-11-18, Windows 11, job object + AppContainer):
    //   * loopback 127.0.0.1 with `allow_net: false` → `refused: timed out`
    //   * 8.8.8.8:53 with `allow_net: false` → `refused: permission denied`
    //   * 8.8.8.8:53 with `allow_net: true`  → `connected`
    // Loopback stays refused even with the capability: reaching it also needs a
    // loopback exemption (`NetworkIsolationSetAppContainerConfig` in Firewallapi.dll,
    // administrator, process-wide), which this crate deliberately does not create, so
    // loopback is only usable as the deny direction here.
    if !hearth_sandbox::appcontainer_available() {
        eprintln!("SKIPPED: no AppContainer identity on this host, so nothing to grant");
        return;
    }

    let workspace = scratch("network-switch");

    // First, the two support strings. Cheap, deterministic, and they prove the switch
    // reached the platform layer rather than being dropped between spec and enforcer.
    let denied_enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");
    let denied_detail = denied_enforcer.support().detail.clone();
    assert!(
        denied_detail.contains("no capability, so no outbound sockets"),
        "a spec with the network off must say so: {denied_detail}"
    );
    let _ = denied_enforcer.close();

    let mut granted_spec = spec_in(Mode::WorkspaceWrite, &workspace);
    granted_spec.allow_net = true;
    let granted_enforcer = new(granted_spec, None).expect("install");
    let granted_detail = granted_enforcer.support().detail.clone();
    assert!(
        granted_detail.contains("internetClient capability allows outbound sockets"),
        "a spec with the network on must say so: {granted_detail}"
    );
    let _ = granted_enforcer.close();

    // Loopback, switch off: refused. Windows counts loopback as network, and no
    // exemption is installed here, so this direction is observable.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let loopback = listener.local_addr().expect("listener address").to_string();
    let refused = connect_outcome(
        spec_in(Mode::WorkspaceWrite, &workspace),
        &workspace,
        &loopback,
        "switch-loopback",
    );
    assert!(
        refused.starts_with("refused"),
        "a contained child with the network off reached {loopback}: {refused}"
    );
    drop(listener);

    // Then the discriminating pair: one public target, both directions. A plain DNS
    // port rather than a named host, so the probe needs no resolver of its own.
    //
    // The pair is retried because this test crosses the real network: on this host one
    // run reported `refused: host unreachable` for the granted case while the plain
    // host connection to the same address had just succeeded — a transient route, not
    // a policy decision. `host_reaches` is re-measured per attempt, so a skipped host
    // is reported as a skip and never mistaken for a denial. The requirement is the
    // discrimination itself: with the host able to reach the target, the child without
    // the capability is refused and the child with it connects. A `permission denied`
    // on the granted run is a real failure and never retried away.
    const PUBLIC_TARGET: &str = "8.8.8.8:53";
    let mut attempts: Vec<String> = Vec::new();
    let mut success = false;
    for attempt in 1..=3 {
        if !host_reaches(PUBLIC_TARGET) {
            eprintln!(
                "SKIPPED: this host cannot reach {PUBLIC_TARGET}, so a capability \
                 grant is unobservable"
            );
            return;
        }
        let denied = connect_outcome(
            spec_in(Mode::WorkspaceWrite, &workspace),
            &workspace,
            PUBLIC_TARGET,
            "switch-denied",
        );
        let mut granted = spec_in(Mode::WorkspaceWrite, &workspace);
        granted.allow_net = true;
        let connected = connect_outcome(granted, &workspace, PUBLIC_TARGET, "switch-granted");
        attempts.push(format!(
            "attempt {attempt}: deny={denied} grant={connected}"
        ));
        if denied.starts_with("refused") && connected == "connected" {
            success = true;
            break;
        }
        assert!(
            !connected.contains("permission denied"),
            "the internetClient capability was not granted to a spec with allow_net: \
             true: {attempts:?}"
        );
    }
    assert!(
        success,
        "with the network on the child never reached {PUBLIC_TARGET} while this host could: \
         {attempts:?}"
    );
}

#[cfg(windows)]
#[test]
fn windows_reads_are_granted_only_where_asked() {
    // An AppContainer starts with nothing readable, so a tool cannot even open its
    // own `~/.gitconfig` unless the host grants that path. This test proves the grant
    // is what makes the difference: the same child, the same directory, one spec with
    // the grant and one without.
    //
    // The granted file also sits three directories down, none of them granted on its
    // own: reaching it means walking those components, which is why the grant has to
    // include traverse on the ancestors. A real `git` failed with
    // `failed to stat 'C:/Users/.../AppData/Local': Permission denied` until it did.
    //
    // This is the one test that pays Windows' one-time price: rewriting the DACL of a
    // big directory above %TEMP% re-evaluates inheritance across its whole subtree
    // (9-32 s per level, measured). It is paid once per machine because the paths and
    // therefore the identity are stable, and every later run is milliseconds.
    let workspace = scratch("reads");
    let outside = scratch("reads-outside");
    let deep = outside.join("a").join("b").join("c");
    std::fs::create_dir_all(&deep).expect("create deep directory");
    let allowed = deep.join("allowed.txt");
    let withheld = outside.join("withheld.txt");
    std::fs::write(&allowed, "allowed-contents").expect("write allowed file");
    std::fs::write(&withheld, "withheld-contents").expect("write withheld file");

    let run = |path: &Path, granted: bool, name: &str| -> String {
        let mut spec = spec_in(Mode::WorkspaceWrite, &workspace);
        if granted {
            spec.extra_read = vec![path.display().to_string()];
        }
        let enforcer = new(spec, None).expect("install");
        let marker = marker(&workspace, name);
        let mut command = std::process::Command::new(contained_helper(&workspace));
        command.args([
            "read",
            path.to_str().expect("read path"),
            marker.to_str().expect("marker path"),
        ]);
        enforcer.apply(&mut command).expect("apply");
        let mut child = command.spawn().expect("spawn");
        enforcer.after_start(child.id()).expect("adopt");
        // Generous on purpose: the first run on a machine pays Windows' one-time
        // inheritance re-evaluation for the directories above %TEMP% (tens of seconds
        // each). Stable paths mean that cost is paid once, then runs are milliseconds.
        assert!(
            wait_for(&marker, Duration::from_secs(300)),
            "the contained child never reported a read result"
        );
        let outcome = std::fs::read_to_string(&marker).expect("read marker");
        let _ = enforcer.close();
        let _ = child.kill();
        let _ = child.wait();
        outcome
    };

    let granted = run(&allowed, true, "granted");
    assert!(
        granted.starts_with("read: allowed-contents"),
        "a granted path was still unreadable: {granted}"
    );

    let ungranted = run(&withheld, false, "ungranted");
    assert!(
        ungranted.starts_with("denied"),
        "an ungranted path outside the workspace was readable: {ungranted}"
    );
}

#[cfg(windows)]
#[test]
fn windows_apply_after_close_is_refused() {
    let workspace = scratch("closed");
    let enforcer = new(spec_in(Mode::WorkspaceWrite, &workspace), None).expect("install");
    enforcer.close().expect("close");
    let mut command = std::process::Command::new(contained_helper(&workspace));
    command.args(["sleep", "1000"]);
    let error = enforcer.apply(&mut command).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
}
