//! CLI surface tests: no process spawning, because `run` takes its streams.
//! Env-mutating tests share one lock; the config path is the only global state
//! doctor reads, and leaking a test's `FORGE_HOME` into another would make the
//! suite lie about what it exercised.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use forge_cli::{reorder_flags, run, USAGE};

const EXPECTED_USAGE: &str = "forge — an agent harness\n\nusage:\n  forge --version              print version (fast path, no config)\n  forge chat [flags] [prompt]  run one turn and stream the answer to stdout\n  forge \"<prompt>\"             shorthand for forge chat\n  forge web [flags]            serve the browser UI on loopback\n  forge session <command>      operate on a session log without running a turn\n  forge import <old-dir>       import a session tree from the old harness\n  forge index [flags]          rebuild or query the search index\n  forge goal <command>         record, inspect or close a goal\n  forge schedule <command>     record recurring prompts and run the ones due\n  forge doctor [flags]         check the environment\n  forge help                   this message\n\ndocs: docs/README.md\n";

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "forge-cli-test-{}-{tag}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.path.join(name), contents).expect("write fixture");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct ForgeHome {
    old: Option<OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ForgeHome {
    fn set(dir: &TempDir) -> Self {
        let lock = env_lock()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let old = std::env::var_os("FORGE_HOME");
        std::env::set_var("FORGE_HOME", &dir.path);
        ForgeHome { old, _lock: lock }
    }
}

impl Drop for ForgeHome {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => std::env::set_var("FORGE_HOME", value),
            None => std::env::remove_var("FORGE_HOME"),
        }
    }
}

fn run_cli(args: &[&str]) -> (i32, String, String) {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run(&args, &mut stdout, &mut stderr);
    (
        code,
        String::from_utf8(stdout).expect("stdout is UTF-8"),
        String::from_utf8(stderr).expect("stderr is UTF-8"),
    )
}

#[test]
fn usage_is_byte_equal_to_the_frozen_contract() {
    assert_eq!(USAGE, EXPECTED_USAGE);
}

#[test]
fn version_is_a_fast_path_that_does_not_read_a_broken_config() {
    let dir = TempDir::new("version-fast");
    dir.write("config.json", "{ this is not json at all");
    let _home = ForgeHome::set(&dir);
    for flag in ["--version", "-v", "version"] {
        let (code, stdout, stderr) = run_cli(&[flag]);
        assert_eq!(code, 0, "{flag}");
        assert!(stderr.is_empty(), "{flag} wrote stderr: {stderr}");
        assert!(
            stdout.starts_with("forge 0.0.0-dev (unknown) "),
            "{flag}: {stdout}"
        );
        assert!(stdout.ends_with('\n'));
        assert_eq!(stdout.lines().count(), 1);
    }
}

#[test]
fn help_prints_the_usage_text_without_a_config() {
    let dir = TempDir::new("help");
    let _home = ForgeHome::set(&dir);
    for args in [vec![], vec!["help"], vec!["-h"], vec!["--help"]] {
        let (code, stdout, stderr) = run_cli(&args);
        assert_eq!(code, 0);
        assert_eq!(stdout, EXPECTED_USAGE);
        assert!(stderr.is_empty());
    }
}

#[test]
fn unknown_command_is_exit_two_with_usage_on_stderr() {
    let (code, stdout, stderr) = run_cli(&["nope"]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert!(stderr.starts_with("forge: unknown command \"nope\"\n\n"));
    assert!(stderr.ends_with(EXPECTED_USAGE));
}

#[test]
fn doctor_json_has_the_stable_key_set_and_ten_checks_in_order() {
    let dir = TempDir::new("doctor-json");
    dir.write(
        "config.json",
        r#"{"model": "", "sandbox": "workspace-write", "dataDir": ""}"#,
    );
    let _home = ForgeHome::set(&dir);
    let (code, stdout, stderr) = run_cli(&["doctor", "--json"]);
    assert_eq!(code, 0, "unexpected failure: {stdout}\n{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let object = value.as_object().expect("report object");
    for key in [
        "version",
        "os",
        "arch",
        "rustVersion",
        "configPath",
        "workspace",
        "durationMs",
        "checks",
    ] {
        assert!(object.contains_key(key), "missing report key {key}");
    }
    assert_eq!(object.len(), 8, "no extra top-level members");
    assert!(object.get("goVersion").is_none());
    let checks = object["checks"].as_array().expect("checks array");
    let names: Vec<&str> = checks
        .iter()
        .map(|check| check["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec![
            "config",
            "data dir",
            "workspace",
            "git",
            "sandbox",
            "disk",
            "model",
            "mcp",
            "proxy",
            "pty"
        ]
    );
    for check in checks {
        assert!(!check["name"].as_str().unwrap_or("").is_empty());
        assert!(!check["status"].as_str().unwrap_or("").is_empty());
        assert!(!check["detail"].as_str().unwrap_or("").is_empty());
        let status = check["status"].as_str().unwrap();
        assert!(
            ["ok", "warn", "fail", "skip"].contains(&status),
            "unexpected status {status}"
        );
    }
}

#[test]
fn doctor_invalid_sandbox_config_is_exit_one_and_shows_fail() {
    let dir = TempDir::new("doctor-bad-sandbox");
    dir.write("config.json", r#"{"sandbox": "yolo"}"#);
    let _home = ForgeHome::set(&dir);
    let (code, stdout, _stderr) = run_cli(&["doctor"]);
    assert_eq!(code, 1);
    assert!(
        stdout.contains("FAIL"),
        "report must show a failed check: {stdout}"
    );
    assert!(stdout.contains("invalid sandbox mode"));
}

#[test]
fn doctor_skips_unconfigured_model_mcp_and_pty_checks() {
    let dir = TempDir::new("doctor-skips");
    dir.write("config.json", r#"{"model": "", "sandbox": "full-access"}"#);
    let _home = ForgeHome::set(&dir);
    let (code, stdout, _stderr) = run_cli(&["doctor", "--json"]);
    assert_eq!(code, 0);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let checks = value["checks"].as_array().expect("checks");
    let status = |name: &str| {
        checks
            .iter()
            .find(|check| check["name"] == name)
            .map(|check| check["status"].as_str().unwrap().to_string())
            .expect("check exists")
    };
    assert_eq!(status("model"), "skip");
    assert_eq!(status("mcp"), "skip");
    assert_eq!(status("pty"), "skip");
}

#[test]
fn doctor_unknown_flag_is_exit_two_with_the_flag_package_message() {
    let (code, stdout, stderr) = run_cli(&["doctor", "--nope"]);
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "flag provided but not defined: -nope\nUsage of doctor:\n  -json\n    \tmachine-readable output\n  -prune-profiles\n    \tdelete the Windows sandbox profiles this tool created\n"
    );
}

#[test]
fn doctor_ignores_positional_arguments() {
    let dir = TempDir::new("doctor-positional");
    dir.write("config.json", r#"{"sandbox": "full-access"}"#);
    let _home = ForgeHome::set(&dir);
    let (code, stdout, _stderr) = run_cli(&["doctor", "ignored", "--json"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(serde_json::from_str::<serde_json::Value>(&stdout).is_ok());
}

#[test]
fn flags_after_a_positional_are_reordered() {
    let args: Vec<String> = ["chat", "hi", "--model", "x"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    assert_eq!(
        reorder_flags(&args, &["model"]),
        vec!["--model", "x", "chat", "hi"]
    );
}

#[test]
fn value_flags_keep_their_value_even_when_it_looks_like_a_flag() {
    let args: Vec<String> = ["chat", "hi", "--system", "be nice", "--model", "x"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    assert_eq!(
        reorder_flags(&args, &["system", "model"]),
        vec!["--system", "be nice", "--model", "x", "chat", "hi"]
    );
}

#[test]
fn double_dash_terminator_keeps_the_rest_positional() {
    let args: Vec<String> = ["chat", "hi", "--", "--model", "x"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    assert_eq!(
        reorder_flags(&args, &["model"]),
        vec!["--", "chat", "hi", "--model", "x"]
    );
}
