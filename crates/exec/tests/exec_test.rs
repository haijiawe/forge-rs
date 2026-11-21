//! Executable contract tests for `hearth-exec`.
//!
//! The child is the `hearth-exec-helper` binary built from `src/bin/`, so argv,
//! exit codes and environment assertions do not depend on a shell or on
//! libtest's own argument parsing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hearth_core::ErrorKind;
use hearth_exec::{Command, Pipe, Sandbox, Stream, ENV_INHERIT};

fn helper(mode: &str) -> Command {
    Command::new(env!("CARGO_BIN_EXE_hearth-exec-helper")).arg(mode)
}

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "hearth-exec-test-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
        label
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn non_zero_exit_is_data_not_error() {
    let result = Pipe
        .run(helper("exit").arg("3"))
        .expect("non-zero exit must not be an execution error");
    assert_eq!(result.exit_code, 3);
    assert!(result.duration > Duration::ZERO);
}

#[test]
fn captures_stdout_and_stderr_separately() {
    let result = Pipe.run(helper("stdout").arg("out")).unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, b"out");
    assert!(result.stderr.is_empty());

    let result = Pipe.run(helper("stderr").arg("err")).unwrap();
    assert_eq!(result.stderr, b"err");
    assert!(result.stdout.is_empty());
}

#[test]
fn observer_sees_output_while_the_process_is_still_running() {
    // The child writes, sleeps, then writes again: if the observer only ran
    // after the wait loop returned, both chunks would share one timestamp and
    // the first would arrive too late.
    let started = Instant::now();
    let first = Arc::new(Mutex::new(None::<(Duration, Stream)>));
    let sink = Arc::clone(&first);
    let result =
        Pipe.run(helper("drip").arg("400").arg("hello").observe(Arc::new(
            move |stream, _chunk| {
                let mut slot = sink.lock().unwrap();
                if slot.is_none() {
                    *slot = Some((started.elapsed(), stream));
                }
            },
        )))
        .unwrap();
    let (when, stream) = first.lock().unwrap().expect("observer must run");
    assert_eq!(stream, Stream::Out);
    // The run lasts ~400ms; a live observer fires in the first half.
    assert!(
        when < result.duration / 2,
        "first chunk at {when:?}, whole run {:?}",
        result.duration
    );
}

#[test]
fn observer_sees_both_streams_tagged_and_capture_still_works() {
    let chunks = Arc::new(Mutex::new(Vec::<(Stream, Vec<u8>)>::new()));
    let sink = Arc::clone(&chunks);
    let result = Pipe
        .run(
            helper("drip")
                .arg("20")
                .arg("xy")
                .observe(Arc::new(move |stream, chunk| {
                    sink.lock().unwrap().push((stream, chunk.to_vec()));
                })),
        )
        .unwrap();
    assert_eq!(result.stdout, b"xy");
    assert_eq!(result.stderr, b"xy");
    let chunks = chunks.lock().unwrap();
    let joined = |want: Stream| -> Vec<u8> {
        chunks
            .iter()
            .filter(|(stream, _)| *stream == want)
            .flat_map(|(_, chunk)| chunk.clone())
            .collect()
    };
    assert_eq!(joined(Stream::Out), b"xy");
    assert_eq!(joined(Stream::Err), b"xy");
}

#[test]
fn observer_panic_does_not_truncate_the_capture() {
    // A viewer bug must not cost the caller the result it asked for.
    let result = Pipe
        .run(
            helper("emit")
                .arg("512")
                .max_output_bytes(1024)
                .observe(Arc::new(|_stream, _chunk| panic!("observer exploded"))),
        )
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.len(), 512);
    assert!(!result.truncated);
}

#[test]
fn empty_path_is_rejected_without_running() {
    let error = Pipe
        .run(Command::default())
        .expect_err("empty path must fail");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.message().contains("empty command path"), "{}", error);
}

#[test]
fn missing_executable_reports_io_error() {
    let error = Pipe
        .run(Command::new("definitely-not-a-real-executable-hearth"))
        .expect_err("missing executable must fail");
    assert_eq!(error.kind(), ErrorKind::Io);
    assert!(error
        .message()
        .contains("definitely-not-a-real-executable-hearth"));
}

#[test]
fn truncates_stdout_and_stderr_independently() {
    let result = Pipe
        .run(helper("emit").arg("4096").max_output_bytes(128))
        .unwrap();
    assert_eq!(result.stdout.len(), 128);
    assert!(result.truncated);
    assert_eq!(result.stdout, vec![b'x'; 128]);

    let result = Pipe
        .run(helper("emit-err").arg("4096").max_output_bytes(64))
        .unwrap();
    assert_eq!(result.stderr.len(), 64);
    assert!(result.truncated);
    assert!(result.stdout.is_empty());
}

#[test]
fn default_cap_is_one_mebibyte() {
    let result = Pipe.run(helper("emit").arg("2097152")).unwrap();
    assert_eq!(result.stdout.len(), 1 << 20);
    assert!(result.truncated);
}

#[test]
fn timeout_is_a_refused_error() {
    let started = Instant::now();
    let error = Pipe
        .run(helper("sleep").timeout(Duration::from_millis(250)))
        .expect_err("timeout must fail");
    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(error.message().contains("timed out"), "{}", error);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn timeout_kills_the_grandchild_tree() {
    let dir = temp_dir("timeout-tree");
    let sentinel = dir.join("grandchild-survived.txt");
    let error = Pipe
        .run(
            helper("spawn-writer")
                .arg(sentinel.to_string_lossy().into_owned())
                .timeout(Duration::from_millis(1500)),
        )
        .expect_err("timeout must fail");
    assert!(error.message().contains("timed out"), "{}", error);

    // The grandchild writes two seconds after it was spawned. If the job object
    // did not kill it, the file appears during this window.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !sentinel.exists(),
        "grandchild survived the timeout tree kill: {}",
        sentinel.display()
    );
}

#[test]
fn cancellation_kills_the_grandchild_tree() {
    let dir = temp_dir("cancel-tree");
    let sentinel = dir.join("grandchild-survived.txt");
    let marker = PathBuf::from(format!("{}.spawned", sentinel.display()));

    let cancelled = Arc::new(AtomicBool::new(false));
    let stopper_flag = Arc::clone(&cancelled);
    let child_sentinel = sentinel.clone();
    let runner = std::thread::spawn(move || {
        let command = helper("spawn-writer").arg(child_sentinel.to_string_lossy().into_owned());
        Pipe.run_with_cancel(command, &stopper_flag)
    });

    // Wait until the grandchild exists before cancelling; cancelling earlier
    // would test nothing about tree accounting.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(marker.exists(), "helper never reported its grandchild");

    cancelled.store(true, Ordering::SeqCst);
    let error = runner
        .join()
        .expect("runner thread")
        .expect_err("cancel must fail");
    assert!(error.message().contains("cancelled"), "{}", error);

    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !sentinel.exists(),
        "grandchild survived the cancellation tree kill: {}",
        sentinel.display()
    );
}

#[test]
fn stdin_is_delivered_and_none_is_eof() {
    let result = Pipe.run(helper("stdin").stdin(b"hello".to_vec())).unwrap();
    assert_eq!(result.stdout, b"got:hello");

    let result = Pipe.run(helper("stdin")).unwrap();
    assert_eq!(result.stdout, b"got:");
}

#[test]
fn args_are_passed_verbatim() {
    let result = Pipe
        .run(helper("printargs").arg("a b").arg("q\"uote"))
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim_end(),
        "a b|q\"uote"
    );
}

#[test]
fn working_directory_is_respected() {
    let dir = temp_dir("dir");
    let result = Pipe
        .run(
            helper("touch")
                .arg("touched.txt")
                .dir(dir.to_string_lossy().into_owned()),
        )
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(dir.join("touched.txt").exists());
}

#[test]
fn default_env_is_the_minimal_whitelist() {
    std::env::set_var("HEARTH_EXEC_LEAK_DEFAULT", "1");
    let result = Pipe.run(helper("printenv")).unwrap();
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(output.contains("PATH="), "PATH missing:\n{output}");
    assert!(
        !output.contains("HEARTH_EXEC_LEAK_DEFAULT"),
        "default env leaked a parent variable:\n{output}"
    );
}

#[test]
fn empty_env_is_loader_minimum_only() {
    std::env::set_var("HEARTH_EXEC_LEAK_EMPTY", "1");
    let result = Pipe.run(helper("printenv").env(Vec::new())).unwrap();
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(
        !output.contains("HEARTH_EXEC_LEAK_EMPTY"),
        "empty env leaked a parent variable:\n{output}"
    );
    assert!(
        !output.contains("PATH="),
        "empty env should not get PATH:\n{output}"
    );
}

#[test]
fn explicit_env_is_exactly_that() {
    std::env::set_var("HEARTH_EXEC_LEAK_EXPLICIT", "1");
    let result = Pipe
        .run(helper("printenv").env(vec!["HEARTH_ONLY=42".to_string()]))
        .unwrap();
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(
        output.contains("HEARTH_ONLY=42"),
        "explicit var missing:\n{output}"
    );
    assert!(
        !output.contains("HEARTH_EXEC_LEAK_EXPLICIT"),
        "explicit env leaked a parent variable:\n{output}"
    );
    assert!(
        !output.contains("PATH="),
        "explicit env should not get PATH:\n{output}"
    );
}

#[test]
fn env_inherit_is_the_only_opt_in() {
    std::env::set_var("HEARTH_EXEC_LEAK_INHERIT", "1");
    let result = Pipe
        .run(helper("printenv").env(vec![ENV_INHERIT.to_string(), "HEARTH_ONLY=7".to_string()]))
        .unwrap();
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(
        output.contains("HEARTH_EXEC_LEAK_INHERIT"),
        "EnvInherit did not inherit the parent:\n{output}"
    );
    assert!(
        output.contains("HEARTH_ONLY=7"),
        "explicit override missing:\n{output}"
    );
    assert!(
        !output.contains("hearth:inherit"),
        "the EnvInherit marker leaked into the child:\n{output}"
    );
}

#[cfg(windows)]
#[test]
fn cmd_line_delivers_quotes_verbatim() {
    let shell =
        std::env::var("COMSPEC").unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".to_string());
    let result = Pipe
        .run(Command::new(shell).cmd_line("/c echo \"hello world\""))
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        "\"hello world\"",
        "cmd.exe saw escaped quotes; stdout was {:?}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[cfg(windows)]
#[test]
fn cmd_line_keeps_metacharacters_inside_quotes() {
    let shell =
        std::env::var("COMSPEC").unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".to_string());
    let result = Pipe
        .run(Command::new(shell).cmd_line("/c echo \"a&b|c\""))
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), "\"a&b|c\"");
}

/// An enforcer that rebuilds the command from its program and args, the way a
/// wrapper-inserting one does (`crates/sandbox/src/lib.rs`, `wrap_command`).
///
/// Such a rebuild cannot see `raw_arg` text, so the runner hands the line over
/// in `RAW_CMDLINE_ENV`; this fake delivers it the way the real wrapper does,
/// verbatim, after dropping the variable. Applied the old way — appended before
/// the rebuild — the line was re-quoted into one argument and every output line
/// ended with a stray quote (`plain"`).
struct RebuildingSandbox;

impl Sandbox for RebuildingSandbox {
    fn name(&self) -> &str {
        "rebuilding"
    }

    fn apply(&self, command: &mut std::process::Command) -> hearth_core::Result<()> {
        use std::os::windows::process::CommandExt;

        let raw = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new(hearth_exec::RAW_CMDLINE_ENV))
            .and_then(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()));
        let mut rebuilt = std::process::Command::new(command.get_program());
        for arg in command.get_args() {
            rebuilt.arg(arg);
        }
        if let Some(dir) = command.get_current_dir() {
            rebuilt.current_dir(dir);
        }
        if let Some(raw) = &raw {
            rebuilt.env_remove(hearth_exec::RAW_CMDLINE_ENV);
            rebuilt.raw_arg(raw);
        }
        *command = rebuilt;
        Ok(())
    }
}

#[cfg(windows)]
#[test]
fn a_cmd_line_survives_a_sandbox_that_rebuilds_the_command() {
    let shell =
        std::env::var("COMSPEC").unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".to_string());
    // The quotes matter: without them a shell reads the pipe as a pipe.
    let result = Pipe
        .run(
            Command::new(&shell)
                .cmd_line("/c echo \"a&b|c\"")
                .sandbox(Arc::new(RebuildingSandbox)),
        )
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&result.stdout).trim(),
        "\"a&b|c\"",
        "the raw command line did not survive the rebuild; stdout was {:?}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[cfg(not(windows))]
#[test]
fn cmd_line_is_refused_off_windows() {
    let error = Pipe
        .run(Command::new("echo").cmd_line("/c echo hi"))
        .expect_err("cmd_line must be refused off Windows");
    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(
        error.message().contains("only supported on Windows"),
        "{}",
        error
    );
}

#[test]
fn cmd_line_and_args_conflict_on_every_platform() {
    let error = Pipe
        .run(Command::new("echo").arg("hi").cmd_line("/c echo hi"))
        .expect_err("conflict must be rejected");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.message().contains("mutually exclusive"), "{}", error);
}

#[derive(Default)]
struct FakeSandbox {
    name: String,
    applied: AtomicUsize,
    aborted: AtomicUsize,
    started: AtomicUsize,
    apply_error: Option<String>,
    hook_error: Option<String>,
    seen_program: Mutex<String>,
    seen_dir: Mutex<Option<String>>,
    seen_env_count: AtomicUsize,
}

impl FakeSandbox {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }
}

impl Sandbox for FakeSandbox {
    fn name(&self) -> &str {
        &self.name
    }

    fn apply(&self, command: &mut std::process::Command) -> hearth_core::Result<()> {
        self.applied.fetch_add(1, Ordering::SeqCst);
        *self.seen_program.lock().unwrap() = command.get_program().to_string_lossy().into_owned();
        *self.seen_dir.lock().unwrap() = command
            .get_current_dir()
            .map(|dir| dir.to_string_lossy().into_owned());
        self.seen_env_count
            .store(command.get_envs().count(), Ordering::SeqCst);
        if let Some(message) = &self.apply_error {
            return Err(hearth_core::Error::new(ErrorKind::Refused, message.clone()));
        }
        Ok(())
    }

    fn abort_apply(&self) {
        self.aborted.fetch_add(1, Ordering::SeqCst);
    }

    fn after_start(&self, _pid: u32) -> hearth_core::Result<()> {
        self.started.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = &self.hook_error {
            return Err(hearth_core::Error::new(ErrorKind::Refused, message.clone()));
        }
        Ok(())
    }
}

#[test]
fn abort_apply_runs_exactly_once_when_spawn_fails() {
    let sandbox = Arc::new(FakeSandbox::new("fake-abort"));
    let error = Pipe
        .run(Command::new("definitely-not-a-real-executable-hearth").sandbox(sandbox.clone()))
        .expect_err("spawn must fail");
    assert_eq!(error.kind(), ErrorKind::Io);
    assert_eq!(sandbox.applied.load(Ordering::SeqCst), 1);
    assert_eq!(sandbox.aborted.load(Ordering::SeqCst), 1);
    assert_eq!(sandbox.started.load(Ordering::SeqCst), 0);
}

#[test]
fn sandbox_apply_sees_the_configured_command_before_start() {
    let dir = temp_dir("sandbox-apply");
    let sandbox = Arc::new(FakeSandbox::new("fake-command"));
    let result = Pipe
        .run(
            helper("stdout")
                .arg("ok")
                .dir(dir.to_string_lossy().into_owned())
                .sandbox(sandbox.clone()),
        )
        .unwrap();
    assert_eq!(result.stdout, b"ok");
    assert_eq!(sandbox.applied.load(Ordering::SeqCst), 1);
    assert_eq!(sandbox.started.load(Ordering::SeqCst), 1);
    assert!(sandbox
        .seen_program
        .lock()
        .unwrap()
        .contains("hearth-exec-helper"));
    assert_eq!(
        sandbox.seen_dir.lock().unwrap().as_deref(),
        Some(dir.to_string_lossy().as_ref())
    );
    assert!(sandbox.seen_env_count.load(Ordering::SeqCst) > 0);
}

#[test]
fn sandbox_apply_failure_starts_nothing() {
    let dir = temp_dir("sandbox-refuse");
    let marker = dir.join("ran.txt");
    let mut sandbox = FakeSandbox::new("fake-refuse");
    sandbox.apply_error = Some("refused by policy".to_string());
    let sandbox = Arc::new(sandbox);
    let error = Pipe
        .run(
            helper("write-marker")
                .arg(marker.to_string_lossy().into_owned())
                .sandbox(sandbox.clone()),
        )
        .expect_err("apply failure must fail the run");
    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(error.message().contains("fake-refuse"), "{}", error);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !marker.exists(),
        "a process started despite sandbox apply failure"
    );
}

#[test]
fn sandbox_after_start_failure_kills_the_child() {
    let dir = temp_dir("sandbox-after");
    let marker = dir.join("survived.txt");
    let mut sandbox = FakeSandbox::new("fake-hook");
    sandbox.hook_error = Some("adopt refused".to_string());
    let sandbox = Arc::new(sandbox);
    let error = Pipe
        .run(
            helper("write-after")
                .arg(marker.to_string_lossy().into_owned())
                .arg("1500")
                .sandbox(sandbox.clone()),
        )
        .expect_err("after_start failure must fail the run");
    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(error.message().contains("fake-hook"), "{}", error);
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !marker.exists(),
        "child kept running after after_start failed"
    );
}

#[test]
fn concurrent_runs_do_not_interfere() {
    const WORKERS: usize = 8;
    let mut handles = Vec::new();
    for index in 0..WORKERS {
        handles.push(std::thread::spawn(move || {
            let result = Pipe
                .run(helper("printargs").arg("worker").arg(index.to_string()))
                .unwrap();
            let text = String::from_utf8_lossy(&result.stdout).trim().to_string();
            assert_eq!(text, format!("worker|{index}"));
        }));
    }
    for handle in handles {
        handle.join().expect("worker thread");
    }
}

#[test]
fn cancellation_already_requested_still_returns_an_error() {
    let cancelled = AtomicBool::new(true);
    let error = Pipe
        .run_with_cancel(helper("sleep"), &cancelled)
        .expect_err("pre-cancelled run must fail");
    assert!(error.message().contains("cancelled"), "{}", error);
}

#[test]
fn cancelling_keeps_what_the_command_printed() {
    // A person watching a build stop it halfway and still expects to see the
    // lines it managed to print; the event written after the session must not
    // claim the command printed nothing.
    let cancelled = Arc::new(AtomicBool::new(false));
    let stopper = Arc::clone(&cancelled);
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        stopper.store(true, Ordering::SeqCst);
    });
    let result = Pipe
        .run_with_cancel_capture(helper("drip").args(["5000", "before-cancel\n"]), &cancelled)
        .expect("a cancelled run that kept its capture is not an error");
    trigger.join().expect("trigger thread");
    assert!(result.cancelled, "the result must say it was cancelled");
    assert_eq!(result.exit_code, -1, "a killed child has no exit status");
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(stdout.contains("before-cancel"), "stdout was {:?}", stdout);
    assert!(
        !String::from_utf8_lossy(&result.stderr).contains("cancelled"),
        "the capture is a record of the streams, not a place for notes"
    );
}

#[test]
fn a_plain_cancel_still_reports_an_error_with_no_capture() {
    // The same run through the older path: a tool call that was cancelled must
    // fail loudly rather than hand back half a result that looks complete.
    let cancelled = Arc::new(AtomicBool::new(false));
    let stopper = Arc::clone(&cancelled);
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        stopper.store(true, Ordering::SeqCst);
    });
    let error = Pipe
        .run_with_cancel(helper("drip").args(["5000", "gone\n"]), &cancelled)
        .expect_err("the plain path reports cancellation as an error");
    trigger.join().expect("trigger thread");
    assert!(error.message().contains("cancelled"), "{}", error);
}
