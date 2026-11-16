//! Behaviour tests for the sandbox crate on the host that runs them.
//!
//! The mode matrix is asserted against the real platform: on this Windows box
//! `read-only` must be refused (no filesystem confinement exists) and
//! `workspace-write` must be accepted only as a partial guarantee that says
//! `filesystem=false`. The measured job-object tests start the helper binary so
//! a grandchild is a real grandchild, not a mock.

#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use forge_sandbox::available;
use forge_sandbox::{new, ErrorKind, Mode, Spec};

// Only the Windows job-object tests start a child process, so these helpers
// would be dead code when the test binary is cross-compiled to Linux or macOS.#[cfg(windows)]
fn workspace() -> PathBuf {
    let path = std::env::temp_dir().join(format!("forge-sandbox-it-{}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create workspace");
    std::fs::canonicalize(path).expect("canonical workspace")
}

#[cfg(windows)]
fn spec(mode: Mode) -> Spec {
    let mut spec = Spec::new(mode);
    spec.workspace = workspace().display().to_string();
    spec
}

#[cfg(windows)]
fn helper() -> &'static str {
    env!("CARGO_BIN_EXE_forge-sandbox-helper")
}

#[cfg(windows)]
fn wait_for(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
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
    let support = available();
    assert!(!support.filesystem, "job objects cannot name directories");
    assert!(support.process_tree);
    assert!(support.detail.contains("filesystem"));

    let error = new(spec(Mode::ReadOnly), None)
        .err()
        .expect("read-only must be refused");
    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(error.message().contains("filesystem"));

    let enforcer = new(spec(Mode::WorkspaceWrite), None).expect("partial enforcement is accepted");
    let installed = enforcer.support();
    assert!(installed.enforced);
    assert!(!installed.filesystem);
    assert!(installed.process_tree);
    assert!(installed.detail.contains("NOT confined"));
    assert!(enforcer.close().is_ok());
}

#[cfg(windows)]
#[test]
fn windows_job_starts_suspended_and_resumes_after_adoption() {
    let marker =
        std::env::temp_dir().join(format!("forge-sandbox-resume-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&marker);

    let enforcer = new(spec(Mode::WorkspaceWrite), None).expect("install");
    let mut command = std::process::Command::new(helper());
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
    let parent_marker =
        std::env::temp_dir().join(format!("forge-sandbox-parent-{}.txt", std::process::id()));
    let grandchild_marker = std::env::temp_dir().join(format!(
        "forge-sandbox-grandchild-{}.txt",
        std::process::id()
    ));
    for path in [&parent_marker, &grandchild_marker] {
        let _ = std::fs::remove_file(path);
    }

    let enforcer = new(spec(Mode::WorkspaceWrite), None).expect("install");
    let mut command = std::process::Command::new(helper());
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
    let marker =
        std::env::temp_dir().join(format!("forge-sandbox-count-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&marker);

    let enforcer = new(spec(Mode::WorkspaceWrite), None).expect("install");
    let mut command = std::process::Command::new(helper());
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
    let enforcer = new(spec(Mode::WorkspaceWrite), None).expect("install");
    let mut command = std::process::Command::new(helper());
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
    let enforcer = new(spec(Mode::WorkspaceWrite), None).expect("install");

    let mut abandoned = std::process::Command::new(helper());
    abandoned.args(["sleep", "60000"]);
    enforcer.apply(&mut abandoned).expect("apply first");
    enforcer.abort_apply();

    let marker =
        std::env::temp_dir().join(format!("forge-sandbox-abort-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let mut command = std::process::Command::new(helper());
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

#[cfg(windows)]
#[test]
fn windows_apply_after_close_is_refused() {
    let enforcer = new(spec(Mode::WorkspaceWrite), None).expect("install");
    enforcer.close().expect("close");
    let mut command = std::process::Command::new(helper());
    command.args(["sleep", "1000"]);
    let error = enforcer.apply(&mut command).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
}
