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

pub mod doctor;

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
        other => {
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
