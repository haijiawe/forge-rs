//! The `forge` binary: argument parsing, help text, and the doctor command.
//!
//! Layer L3. [`run`] takes the argument list without the program name and
//! writes through caller-supplied streams, so the whole surface is testable
//! without spawning a process. `main.rs` is a two-line adapter around it.
//!
//! The usage text is a frozen contract (`docs/contracts/cli.md` §2.2). It is
//! reproduced byte for byte, including the em dash and the two trailing spaces
//! that align the descriptions, because a test compares the bytes.

// deny, not orbid: exactly one function reaches the Windows API for disk
// free space (doctor.rs disk_free_windows), because the alternative is
// spawning PowerShell for a number the kernel already knows. That function
// carries its own #[allow(unsafe_code)] and a safety comment; everything else
// in this crate is safe Rust.
#![deny(unsafe_code)]
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io::Write;
use std::path::PathBuf;

pub mod chat;
pub mod doctor;
pub mod importcmd;
pub mod indexcmd;
pub mod sessioncmd;
pub mod web;
pub mod wiring;

/// Prints `value` as one JSON document with two-space indent and a trailing
/// newline, the shape `docs/contracts/cli.md` §4.2 fixes for `--json`.
///
/// `command` names the subcommand in the diagnostic, so every command shares one
/// writer instead of inventing its own indentation.
pub fn write_json<T: serde::Serialize>(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    command: &str,
    value: &T,
) -> i32 {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            let _ = writeln!(stdout, "{text}");
            0
        }
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge {command}: write json: {}",
                one_line(&err.to_string())
            );
            1
        }
    }
}

/// Collapses a value that may contain newlines into one line, so a diagnostic or
/// a table row stays one row. Tabs become spaces for the same reason.
pub fn one_line(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' | '\r' | '\t' => out.push(' '),
            other => out.push(other),
        }
    }
    out.trim().to_string()
}

/// Reports whether the caller asked for help before any parsing happens, so
/// `-h`/`--help` prints usage to stdout and exits 0 while a usage *mistake*
/// prints the same text to stderr and exits 2. Everything after a bare `--` is
/// positional and never a help request.
pub fn wants_help(args: &[String]) -> bool {
    for arg in args {
        match arg.as_str() {
            "--" => return false,
            "-h" | "-help" | "--help" => return true,
            _ => {}
        }
    }
    false
}

/// The configured data directory, without requiring a valid sandbox mode:
/// `import` and `index` need a place to write and nothing else from the config,
/// so a sandbox typo must not stop them.
pub fn default_data_dir() -> forge_core::Result<PathBuf> {
    let path = forge_config::default_path()?;
    let cfg = forge_config::Config::load(&path)?;
    if cfg.data_dir.trim().is_empty() {
        return Err(forge_core::Error::new(
            forge_core::ErrorKind::Invalid,
            "no data directory resolved; pass --data-dir",
        ));
    }
    Ok(PathBuf::from(cfg.data_dir.trim()))
}

/// Explodes a Unix timestamp into UTC civil fields: `(year, month, day, hour,
/// minute, second)`.
///
/// One implementation, because a session id, a table cell and a log line must
/// agree about what "now" looks like, and three copies of the civil-from-days
/// algorithm would eventually disagree by a day.
pub fn civil_from_epoch(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (year, month, day, rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// `YYYYMMDD-HHMMSS` in UTC: the sortable stamp a new session id starts with.
pub fn epoch_stamp(secs: i64) -> String {
    let (year, month, day, hour, minute, second) = civil_from_epoch(secs);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

/// A fresh session id: a timestamped stamp plus three random bytes.
///
/// Lives here rather than in one command because `chat`, `session fork` and
/// `import` all mint ids, and two generators would eventually disagree about the
/// format `forge_core::SessionId::parse` accepts.
pub fn new_session_id() -> forge_core::Result<String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let stamp = epoch_stamp(secs);
    let mut bytes = [0u8; 3];
    getrandom::fill(&mut bytes).map_err(|err| {
        forge_core::Error::new(forge_core::ErrorKind::Io, format!("random: {err}"))
    })?;
    Ok(format!(
        "{stamp}-{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2]
    ))
}

/// Civil date from a count of days since the Unix epoch (Howard Hinnant's
/// algorithm), UTC only: no timezone database is worth a dependency here.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
}

/// Build identity, injected at build time by the Go build (and kept here so
/// the version line stays stable across the port).
pub const VERSION: &str = "0.0.0-dev";
/// Commit hash; `unknown` for a working-tree build.
pub const COMMIT: &str = "unknown";

/// The frozen `forge help` text from `docs/contracts/cli.md` §2.2.
pub const USAGE: &str = "forge — an agent harness

usage:
  forge --version              print version (fast path, no config)
  forge chat [flags] [prompt]  run one turn and stream the answer to stdout
  forge \"<prompt>\"             shorthand for forge chat
  forge web [flags]            serve the browser UI on loopback
  forge session <command>      operate on a session log without running a turn
  forge import <old-dir>       import a session tree from the old harness
  forge index [flags]          rebuild or query the search index
  forge doctor [--json]        check the environment
  forge help                   this message

docs: docs/README.md
";

/// The compiler that built this binary, captured by `build.rs`.
pub fn rust_version() -> &'static str {
    option_env!("FORGE_RUSTC_VERSION").unwrap_or("unknown")
}

/// The platform name used on the wire: Go spelled macOS `darwin`, and the
/// version line is a contract, so the translation lives here.
pub fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// The architecture name used on the wire: Go spelled x86-64 `amd64`.
pub fn arch_name() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// The one-line version string printed by `--version`, `-v`, and `version`.
pub fn version_line() -> String {
    format!(
        "forge {} ({}) {}/{} {}",
        VERSION,
        COMMIT,
        os_name(),
        arch_name(),
        rust_version()
    )
}

fn write_usage(out: &mut dyn Write) {
    // A closed stdout is not an error worth reporting to a closed stdout; the
    // exit code already says whether the command succeeded.
    let _ = out.write_all(USAGE.as_bytes());
}

/// Runs the CLI. `args` excludes the program name, as `std::env::args` hands
/// it over after `skip(1)`. Returns the process exit code: 0 ok, 1 a check
/// failed, 2 usage.
pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(first) = args.first().map(String::as_str) else {
        write_usage(stdout);
        return 0;
    };

    match first {
        // Fast path: must not read the config file, because a broken config
        // must not make `forge --version` fail.
        "--version" | "-v" | "version" => {
            let _ = writeln!(stdout, "{}", version_line());
            0
        }
        "help" | "-h" | "--help" => {
            write_usage(stdout);
            0
        }
        "doctor" => doctor::run(&args[1..], stdout, stderr),
        "chat" => chat::run(&args[1..], stdout, stderr),
        "web" => web::run(&args[1..], stdout, stderr),
        "index" => indexcmd::run(&args[1..], stdout, stderr),
        "import" => importcmd::run(&args[1..], stdout, stderr),
        "session" => sessioncmd::run(&args[1..], stdout, stderr),
        other => {
            // Bare `forge "问题"` is shorthand for `forge chat`. A lone ASCII
            // word stays an unknown command so a typo fails fast instead of
            // spending a model call (contract §2.5, main.go isPromptInvocation).
            if chat::is_prompt_invocation(args) {
                return chat::run(args, stdout, stderr);
            }
            // `%q` in the Go build: a quoted, escaped command name.
            let _ = writeln!(stderr, "forge: unknown command {other:?}\n");
            let _ = stderr.write_all(USAGE.as_bytes());
            2
        }
    }
}

/// Moves flags ahead of positionals so a flag written after a prompt is still
/// seen by the command's parser.
///
/// This exists once, here, because the Go build had a separate parser in each
/// command and `forge chat "hi" --model x` silently ignored `--model`. Value
/// flags consume the following token (unless written `--name=value`), and a
/// bare `--` keeps everything after it positional.
pub fn reorder_flags(args: &[String], value_flags: &[&str]) -> Vec<String> {
    let mut flags: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut saw_terminator = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            positional.extend(args[i + 1..].iter().cloned());
            saw_terminator = true;
            break;
        }
        // A single dash is a positional (stdin convention), not a flag.
        if arg.len() < 2 || !arg.starts_with('-') {
            positional.push(arg.clone());
            i += 1;
            continue;
        }
        flags.push(arg.clone());
        let name = arg.trim_start_matches('-');
        if !name.contains('=') && value_flags.contains(&name) && i + 1 < args.len() {
            // The next token is this flag's value even if it starts with `-`:
            // `--model -o` is a model named `-o`, not a missing value.
            flags.push(args[i + 1].clone());
            i += 1;
        }
        i += 1;
    }

    if saw_terminator {
        flags.push("--".to_string());
    }
    flags.extend(positional);
    flags
}
