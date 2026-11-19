//! The doctor: ten read-only environment checks, in a fixed order.
//!
//! A check that cannot be performed is `skip`, never `ok` — the harness must
//! not claim to have verified something it did not. Only a `fail` changes the
//! exit code; warnings are information.

use std::fmt::Write as FmtWrite;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;
use std::time::{Duration, Instant};

use forge_config::Config;
use forge_core::{Error, ErrorKind};
use serde::Serialize;

/// A check verdict, lowercase on the wire as the Go report had it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Status {
    /// The text report uppercases the status; JSON keeps the lowercase wire
    /// spelling. Both are contract (`cli.md` §2.4).
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

/// One row of the doctor report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Check {
    fn new(name: &str, status: Status, detail: impl Into<String>) -> Self {
        Check {
            name: name.to_string(),
            status,
            detail: detail.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// The stable JSON object printed by `forge doctor --json`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub rust_version: String,
    pub config_path: String,
    pub workspace: String,
    pub duration_ms: u64,
    pub checks: Vec<Check>,
}

impl Report {
    fn failed(&self) -> bool {
        self.checks.iter().any(|c| c.status == Status::Fail)
    }

    /// Human table: status and name columns are as wide as their widest cell
    /// plus two spaces, exactly the Go tabwriter configuration.
    fn print(&self, out: &mut dyn Write) -> std::io::Result<()> {
        writeln!(
            out,
            "forge {}  {}/{}  {}",
            self.version, self.os, self.arch, self.rust_version
        )?;
        writeln!(out, "config     {}", self.config_path)?;
        writeln!(out, "workspace  {}", self.workspace)?;
        writeln!(out)?;

        let status_width = self
            .checks
            .iter()
            .map(|c| c.status.label().len())
            .max()
            .unwrap_or(0)
            + 2;
        let name_width = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0) + 2;
        for check in &self.checks {
            writeln!(
                out,
                "{:<status_width$}{:<name_width$}{}",
                check.status.label(),
                check.name,
                check.detail
            )?;
            if let Some(hint) = &check.hint {
                writeln!(
                    out,
                    "{:>padding$}-> {}",
                    "",
                    hint,
                    padding = status_width + name_width
                )?;
            }
        }
        writeln!(out)?;
        writeln!(
            out,
            "{} checks in {} ms",
            self.checks.len(),
            self.duration_ms
        )
    }
}

struct DoctorOptions<'a> {
    config_path: String,
    config: Option<&'a Config>,
    config_err: Option<&'a Error>,
    workspace: String,
}

/// Parses doctor flags and runs the checks. Returns 0 unless a check failed.
pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let mut json = false;
    let mut prune = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            // The Go flag package stops at `--`; remaining words are
            // positional and doctor ignores positionals.
            break;
        }
        if arg == "-json" || arg == "--json" {
            json = true;
            i += 1;
            continue;
        }
        if arg == "-prune-profiles" || arg == "--prune-profiles" {
            prune = true;
            i += 1;
            continue;
        }
        if let Some(value) = arg
            .strip_prefix("--json=")
            .or_else(|| arg.strip_prefix("-json="))
        {
            match value {
                "true" => json = true,
                "false" => json = false,
                _ => {
                    write_bad_flag(stderr, "json");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        if arg.starts_with('-') && arg != "-" {
            write_bad_flag(stderr, arg.trim_start_matches('-'));
            return 2;
        }
        i += 1;
    }

    if prune {
        // Maintenance runs instead of the checks, not after them: the point of the
        // flag is to free the profile budget, and a caller who asked for it does not
        // need ten green lines first.
        return prune_profiles(json, stdout, stderr);
    }

    let config_path = match forge_config::default_path() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(err) => {
            let _ = writeln!(stderr, "forge doctor: {err}");
            return 1;
        }
    };

    let loaded = Config::load(Path::new(&config_path));
    let (config, config_err) = match loaded {
        Ok(cfg) => (Some(cfg), None),
        Err(err) => (None, Some(err)),
    };
    let workspace = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    // The clock starts after loading: the report says how long the checks
    // took, and the Go doctor received an already-loaded config for the same
    // reason.
    let start = Instant::now();

    let options = DoctorOptions {
        config_path,
        config: config.as_ref(),
        config_err: config_err.as_ref(),
        workspace,
    };
    let checks = vec![
        check_config(&options),
        check_data_dir(&options),
        check_workspace(&options),
        check_git(),
        check_sandbox(&options),
        check_disk(&options),
        check_model(&options),
        check_mcp(&options),
        check_proxy(),
        check_pty(),
    ];

    let report = Report {
        version: format!("{} ({})", crate::VERSION, crate::COMMIT),
        os: crate::os_name().to_string(),
        arch: crate::arch_name().to_string(),
        rust_version: crate::rust_version().to_string(),
        config_path: options.config_path.clone(),
        workspace: options.workspace.clone(),
        duration_ms: start.elapsed().as_millis() as u64,
        checks,
    };

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => {
                let _ = writeln!(stdout, "{text}");
            }
            Err(err) => {
                let _ = writeln!(stderr, "forge doctor: {err}");
                return 1;
            }
        }
    } else if let Err(err) = report.print(stdout) {
        let _ = writeln!(stderr, "forge doctor: {err}");
        return 1;
    }

    if report.failed() {
        1
    } else {
        0
    }
}

/// Delete the Windows package profiles this tool created, then say what happened.
///
/// Separate from the check report on purpose: the checks describe the environment and
/// must stay read-only, while this changes it. A user who asked for the checks did not
/// ask for a delete.
fn prune_profiles(json: bool, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    #[cfg(target_os = "windows")]
    {
        let report = match forge_sandbox::prune_appcontainer_profiles() {
            Ok(report) => report,
            Err(error) => {
                let _ = writeln!(stderr, "forge doctor: {}", error.message());
                return 1;
            }
        };
        if json {
            let payload = serde_json::json!({
                "found": report.found,
                "removed": report.removed,
                "failed": report
                    .failed
                    .iter()
                    .map(|(name, hr)| serde_json::json!({ "name": name, "hr": format!("0x{hr:08x}") }))
                    .collect::<Vec<_>>(),
                "stoppedEarly": report.stopped_early,
            });
            match serde_json::to_string_pretty(&payload) {
                Ok(text) => {
                    let _ = writeln!(stdout, "{text}");
                }
                Err(error) => {
                    let _ = writeln!(stderr, "forge doctor: {error}");
                    return 1;
                }
            }
        } else {
            let _ = writeln!(
                stdout,
                "sandbox profiles: {} found, {} removed, {} failed",
                report.found,
                report.removed.len(),
                report.failed.len()
            );
            for (name, hr) in &report.failed {
                let _ = writeln!(stdout, "  {name}: 0x{hr:08x}");
            }
            if report.stopped_early {
                // Say what the number means, because "0 removed, 1 failed" out of 57
                // found would otherwise read as "the tool gave up after one try".
                let _ = writeln!(
                    stderr,
                    "forge doctor: the profile service is refusing deletes right now; \
                     the rest were not attempted — retrying in a few minutes is the fix"
                );
            }
        }
        if report.clean() {
            0
        } else {
            1
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = json;
        let _ = stderr;
        let _ = writeln!(
            stdout,
            "sandbox profiles: nothing to prune on this platform"
        );
        0
    }
}

fn write_bad_flag(stderr: &mut dyn Write, name: &str) {
    let _ = writeln!(stderr, "flag provided but not defined: -{name}");
    let _ = writeln!(stderr, "Usage of doctor:");
    let _ = writeln!(stderr, "  -json");
    let _ = writeln!(stderr, "    \tmachine-readable output");
    let _ = writeln!(stderr, "  -prune-profiles");
    let _ = writeln!(
        stderr,
        "    \tdelete the Windows sandbox profiles this tool created"
    );
}

fn check_config(options: &DoctorOptions<'_>) -> Check {
    if let Some(err) = options.config_err {
        return Check::new("config", Status::Fail, err.to_string())
            .with_hint(format!("fix or delete {}", options.config_path));
    }
    let Some(config) = options.config else {
        return Check::new("config", Status::Warn, "no config loaded");
    };
    if !Path::new(&options.config_path).exists() {
        return Check::new("config", Status::Ok, "using defaults (no config file)");
    }
    match config.validate() {
        Ok(()) => Check::new("config", Status::Ok, "valid"),
        Err(err) => {
            Check::new("config", Status::Fail, err.to_string()).with_hint("run: forge config init")
        }
    }
}

fn check_data_dir(options: &DoctorOptions<'_>) -> Check {
    let Some(config) = options.config else {
        return Check::new("data dir", Status::Skip, "no config");
    };
    match config.ensure_data_dir() {
        Ok(()) => Check::new(
            "data dir",
            Status::Ok,
            format!("{} (writable)", config.data_dir),
        ),
        Err(err) => Check::new("data dir", Status::Fail, err.to_string()).with_hint(format!(
            "pick a writable dataDir in {}",
            options.config_path
        )),
    }
}

fn check_workspace(options: &DoctorOptions<'_>) -> Check {
    if options.workspace.is_empty() {
        return Check::new(
            "workspace",
            Status::Fail,
            "cannot determine working directory",
        );
    }
    let path = Path::new(&options.workspace);
    if !path.is_dir() {
        return Check::new(
            "workspace",
            Status::Fail,
            format!("{} is not a directory", options.workspace),
        );
    }
    if path.join(".git").exists() {
        Check::new(
            "workspace",
            Status::Ok,
            format!("{} (git repo)", options.workspace),
        )
    } else {
        Check::new(
            "workspace",
            Status::Warn,
            format!("{} (not a git repo)", options.workspace),
        )
        .with_hint("sessions work without git, but undo/diff features need it")
    }
}

fn check_git() -> Check {
    match find_on_path("git") {
        Some(path) => Check::new("git", Status::Ok, path.to_string_lossy()),
        None => Check::new("git", Status::Warn, "not found on PATH")
            .with_hint("install git to enable diff/undo tools"),
    }
}

fn check_sandbox(options: &DoctorOptions<'_>) -> Check {
    let Some(config) = options.config else {
        return Check::new(
            "sandbox",
            Status::Skip,
            "no config: nothing says which sandbox mode to check",
        );
    };
    let Some(mode) = forge_sandbox::mode_from_str(&config.sandbox) else {
        return Check::new(
            "sandbox",
            Status::Skip,
            format!(
                "cannot build a sandbox spec for mode {:?}: invalid sandbox mode",
                config.sandbox
            ),
        );
    };
    let mut spec = forge_sandbox::Spec::new(mode);
    spec.workspace = options.workspace.clone();
    // Ask about the spec a turn would actually build: a report on a layout the
    // product never uses would describe a sandbox nobody runs.
    let grants = crate::grants::effective(config);
    spec.extra_read = grants.read;
    spec.extra_write = grants.write;
    spec.allow_net = grants.allow_net;
    match forge_sandbox::new(spec, None) {
        Ok(enforcer) => {
            let support = enforcer.support();
            let detail = if support.detail.is_empty() {
                format!("{}: the enforcer reported no detail", enforcer.name())
            } else {
                format!("{}: {}", enforcer.name(), support.detail)
            };
            if mode.as_str() == forge_config::SANDBOX_FULL_ACCESS {
                Check::new("sandbox", Status::Skip, detail)
            } else if !support.filesystem {
                Check::new("sandbox", Status::Warn, detail).with_hint(
                    "the enforcer installed, but filesystem writes are not confined on this host; \
                     the detail is its own account of what it does enforce",
                )
            } else {
                Check::new("sandbox", Status::Ok, detail)
            }
        }
        Err(err) if err.is(ErrorKind::Refused) => {
            // The typed error carries the host's support sentence; printing
            // `err.message()` would repeat the `sandbox: mode ...` wrapper the
            // Go report did not show in the detail column.
            let detail = std::error::Error::source(&err)
                .and_then(|source| source.downcast_ref::<forge_sandbox::UnavailableError>())
                .map(|unavailable| unavailable.support.detail.clone())
                .unwrap_or_else(|| err.message().to_string());
            Check::new("sandbox", Status::Fail, detail).with_hint(format!(
                "mode {:?} cannot be installed on this host: run with --sandbox=full-access \
                 to accept no containment, or use a host whose filesystem can be confined",
                config.sandbox
            ))
        }
        Err(err) if err.is(ErrorKind::Invalid) => Check::new(
            "sandbox",
            Status::Skip,
            format!(
                "cannot build a sandbox spec for mode {:?}: {err}",
                config.sandbox
            ),
        ),
        Err(err) => Check::new("sandbox", Status::Fail, err.to_string()).with_hint(
            "the enforcer failed to install; the detail is its own error, and the harness \
             would refuse to start in this mode",
        ),
    }
}

fn check_disk(options: &DoctorOptions<'_>) -> Check {
    let Some(config) = options.config else {
        return Check::new(
            "disk",
            Status::Skip,
            "no config: no data directory to measure",
        );
    };
    if config.data_dir.is_empty() {
        return Check::new("disk", Status::Skip, "no data directory configured");
    }
    let (free, total) = match disk_free(&config.data_dir) {
        Ok(values) => values,
        Err(err) => {
            return Check::new(
                "disk",
                Status::Skip,
                format!("cannot measure free space for {}: {err}", config.data_dir),
            )
        }
    };
    let detail = format!(
        "{} free of {} on the volume holding {}",
        human_bytes(free),
        human_bytes(total),
        config.data_dir
    );
    if free < 512 * 1024 * 1024 {
        Check::new("disk", Status::Fail, detail).with_hint(
            "free disk space or point dataDir at a larger volume: the append-only event log \
             cannot record a turn it cannot write",
        )
    } else if free < 2 * 1024 * 1024 * 1024 {
        Check::new("disk", Status::Warn, detail).with_hint(
            "sessions need room for the event log and tool output; free space before a long session",
        )
    } else {
        Check::new("disk", Status::Ok, detail)
    }
}

fn check_model(options: &DoctorOptions<'_>) -> Check {
    let Some(config) = options.config else {
        return Check::new(
            "model",
            Status::Skip,
            "no config: nothing says which model to check",
        );
    };
    if config.model.is_empty() {
        return Check::new("model", Status::Skip, "no model configured");
    }
    let base = std::env::var("FORGE_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let url = format!("{}/models", base.trim_end_matches('/'));
    match http_head(&url, std::env::var("FORGE_API_KEY").ok().as_deref()) {
        Err(HeadError::Build(err)) => Check::new(
            "model",
            Status::Warn,
            format!("model {:?}: cannot build a request for {url}: {err}", config.model),
        )
        .with_hint(
            "check FORGE_BASE_URL: it must be an http(s) base URL such as https://api.openai.com/v1",
        ),
        Err(HeadError::Transport(err)) => Check::new(
            "model",
            Status::Warn,
            format!("model {:?}: {url} is not reachable: {err}", config.model),
        )
        .with_hint(
            "offline is supported; start the endpoint (or fix FORGE_BASE_URL) before the first turn",
        ),
        Ok(status) if (200..300).contains(&status) => Check::new(
            "model",
            Status::Ok,
            format!("model {:?}: {url} answered (HTTP {status})", config.model),
        ),
        Ok(status @ (401 | 403)) => Check::new(
            "model",
            Status::Warn,
            format!(
                "model {:?}: {url} refused the request (HTTP {status})",
                config.model
            ),
        )
        .with_hint(
            "set FORGE_API_KEY to a key the endpoint accepts; the request either had no key \
             or a rejected one",
        ),
        Ok(status) => Check::new(
            "model",
            Status::Warn,
            format!("model {:?}: {url} answered HTTP {status}", config.model),
        )
        .with_hint(format!(
            "the endpoint is reachable but {url} did not succeed; check FORGE_BASE_URL points \
             at an OpenAI-compatible API"
        )),
    }
}

fn check_mcp(options: &DoctorOptions<'_>) -> Check {
    let Some(config) = options.config else {
        return Check::new("mcp", Status::Skip, "no config: no MCP servers to report");
    };
    let configured = config.mcp.servers.len();
    if configured == 0 {
        return Check::new("mcp", Status::Skip, "no MCP servers configured");
    }
    let usable = config.usable_mcp_servers();
    let dropped = configured - usable.len();
    if dropped > 0 {
        let names: Vec<&str> = usable.iter().map(|server| server.name.as_str()).collect();
        let detail = format!(
            "{} configured, {} usable ({} dropped)",
            configured,
            names.len(),
            dropped
        );
        let warnings = config.mcp_warnings().join("; ");
        return Check::new("mcp", Status::Warn, detail).with_hint(warnings);
    }
    let names: Vec<&str> = usable.iter().map(|server| server.name.as_str()).collect();
    Check::new(
        "mcp",
        Status::Ok,
        format!("{} usable: {}", names.len(), names.join(", ")),
    )
}

fn check_proxy() -> Check {
    const VARS: [&str; 4] = ["HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY", "ALL_PROXY"];
    let mut set = Vec::new();
    for name in VARS {
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                set.push(format!("{name}={value}"));
            }
        }
    }
    if set.is_empty() {
        Check::new("proxy", Status::Ok, "none set")
    } else {
        Check::new("proxy", Status::Ok, set.join(" "))
    }
}

fn check_pty() -> Check {
    match std::env::consts::OS {
        "windows" | "linux" | "macos" => Check::new(
            "pty",
            Status::Skip,
            "shell runs on pipes, not a pty: interactive programs will not render",
        )
        .with_hint(
            "ConPTY/pty is available on this platform and can be wired when a terminal is needed",
        ),
        other => Check::new("pty", Status::Warn, format!("unsupported platform {other}"))
            .with_hint("interactive terminal will fall back to pipes"),
    }
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let direct = dir.join(program);
        if direct.is_file() {
            return Some(direct);
        }
        #[cfg(windows)]
        {
            // Windows resolves a bare name through PATHEXT; `git` is `git.exe`
            // on this box, and saying "not found" because of a missing suffix
            // would be a false warning.
            for ext in ["exe", "cmd", "bat", "com"] {
                let candidate = dir.join(format!("{program}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(windows)]
fn disk_free(dir: &str) -> Result<(u64, u64), String> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let root = volume_root(dir);
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    let mut total: u64 = 0;
    // SAFETY: `wide` is a nul-terminated UTF-16 buffer that outlives the call;
    // `free`/`total` are valid out-pointers. `GetDiskFreeSpaceExW` only reads
    // the path and writes those two values. A failure is returned as an error,
    // never used as a zero-valued measurement.
    #[allow(unsafe_code)]
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free as *mut u64,
            &mut total as *mut u64,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "GetDiskFreeSpaceExW({root:?}) failed with last error {}",
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
        ));
    }
    if total == 0 {
        return Err("reported a zero-byte volume".to_string());
    }
    Ok((free, total))
}

/// Run a probe process with a deadline.
///
/// `Command::output()` waits forever, and `doctor` is the command an operator
/// runs *when something is already wrong* — a hung helper would turn a
/// diagnostic into a second outage. The child is killed at the deadline and the
/// caller reports SKIP; the stdio pipes are ours and hold a handful of bytes, so
/// polling `try_wait` cannot deadlock on a full pipe buffer.
#[cfg(not(windows))]
fn run_probe(mut command: Command) -> Result<std::process::Output, String> {
    const LIMIT: Duration = Duration::from_secs(5);
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", LIMIT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(windows)]
fn volume_root(dir: &str) -> String {
    // Take the volume prefix from the path itself. `canonicalize` would return
    // a `\\?\` verbatim path, which DriveInfo does not always accept; the raw
    // prefix (`C:`, `\\server\share`) is what the .NET API wants.
    let path = Path::new(dir);
    if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
        let mut root = prefix.as_os_str().to_os_string();
        root.push("\\");
        return root.to_string_lossy().into_owned();
    }
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(dir));
    if let Some(std::path::Component::Prefix(prefix)) = absolute.components().next() {
        let mut root = prefix.as_os_str().to_os_string();
        root.push("\\");
        return root.to_string_lossy().into_owned();
    }
    dir.to_string()
}

#[cfg(not(windows))]
fn disk_free(dir: &str) -> Result<(u64, u64), String> {
    // POSIX `df -kP` gives 1024-byte blocks and a stable one-line-per-fs
    // format; parsing it avoids a platform syscall module.
    let mut command = Command::new("df");
    command.args(["-kP", dir]);
    let output = run_probe(command)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().nth(1).ok_or("df returned no data line")?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    let total = fields
        .get(1)
        .ok_or("df has no total")?
        .parse::<u64>()
        .map_err(|e| e.to_string())?
        * 1024;
    let free = fields
        .get(3)
        .ok_or("df has no available")?
        .parse::<u64>()
        .map_err(|e| e.to_string())?
        * 1024;
    Ok((free, total))
}

#[derive(Debug)]
enum HeadError {
    Build(String),
    Transport(String),
}

struct UrlParts {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<UrlParts, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "missing scheme".to_string())?;
    let tls = match scheme {
        "http" => false,
        "https" => true,
        other => return Err(format!("unsupported scheme {other:?}")),
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("missing host".to_string());
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        // IPv6 literal: [::1]:443
        let end = inner
            .find(']')
            .ok_or_else(|| "unterminated IPv6 host".to_string())?;
        let host = &inner[..end];
        let tail = &inner[end + 1..];
        let port = match tail.strip_prefix(':') {
            Some(port) => port.parse::<u16>().map_err(|e| e.to_string())?,
            None => default_port(tls),
        };
        (host.to_string(), port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            (
                host.to_string(),
                port.parse::<u16>().map_err(|e| e.to_string())?,
            )
        } else {
            (authority.to_string(), default_port(tls))
        }
    } else {
        (authority.to_string(), default_port(tls))
    };
    if host.is_empty() {
        return Err("missing host".to_string());
    }
    Ok(UrlParts {
        tls,
        host,
        port,
        path: path.to_string(),
    })
}

fn default_port(tls: bool) -> u16 {
    if tls {
        443
    } else {
        80
    }
}

fn http_head(url: &str, api_key: Option<&str>) -> Result<u16, HeadError> {
    let parts = parse_url(url).map_err(HeadError::Build)?;
    let address = format!("{}:{}", parts.host, parts.port);
    let addresses: Vec<_> = address
        .to_socket_addrs()
        .map_err(|e| HeadError::Transport(e.to_string()))?
        .collect();
    let mut last_error = None;
    for socket in addresses {
        match TcpStream::connect_timeout(&socket, Duration::from_secs(2)) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                return if parts.tls {
                    https_head(stream, &parts, api_key)
                } else {
                    plain_head(stream, &parts, api_key)
                };
            }
            Err(err) => last_error = Some(err.to_string()),
        }
    }
    Err(HeadError::Transport(
        last_error.unwrap_or_else(|| "no address resolved".to_string()),
    ))
}

fn request_bytes(parts: &UrlParts, api_key: Option<&str>) -> Vec<u8> {
    let host_header = if (parts.tls && parts.port == 443) || (!parts.tls && parts.port == 80) {
        parts.host.clone()
    } else {
        format!("{}:{}", parts.host, parts.port)
    };
    let mut request = format!(
        "HEAD {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n",
        parts.path, host_header
    );
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        let _ = write!(request, "Authorization: Bearer {key}\r\n");
    }
    request.push_str("\r\n");
    request.into_bytes()
}

fn plain_head(
    mut stream: TcpStream,
    parts: &UrlParts,
    api_key: Option<&str>,
) -> Result<u16, HeadError> {
    stream
        .write_all(&request_bytes(parts, api_key))
        .map_err(|e| HeadError::Transport(e.to_string()))?;
    read_status(&mut stream)
}

fn https_head(
    stream: TcpStream,
    parts: &UrlParts,
    api_key: Option<&str>,
) -> Result<u16, HeadError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls_pki_types::ServerName::try_from(parts.host.clone())
        .map_err(|e| HeadError::Build(e.to_string()))?;
    let connection = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name)
        .map_err(|e| HeadError::Transport(e.to_string()))?;
    let mut tls = rustls::StreamOwned::new(connection, stream);
    tls.write_all(&request_bytes(parts, api_key))
        .map_err(|e| HeadError::Transport(e.to_string()))?;
    read_status(&mut tls)
}

fn read_status(stream: &mut dyn Read) -> Result<u16, HeadError> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|e| HeadError::Transport(e.to_string()))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() >= 16 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let first = text
        .lines()
        .next()
        .ok_or_else(|| HeadError::Transport("empty response".into()))?;
    let mut fields = first.split_whitespace();
    let protocol = fields.next().unwrap_or("");
    if !protocol.starts_with("HTTP/") {
        return Err(HeadError::Transport(format!(
            "not an HTTP response: {first:?}"
        )));
    }
    fields
        .next()
        .ok_or_else(|| HeadError::Transport("response has no status code".into()))?
        .parse::<u16>()
        .map_err(|e| HeadError::Transport(format!("bad status code: {e}")))
}
