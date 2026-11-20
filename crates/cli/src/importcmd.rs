//! `hearth import`: copy one previous-generation session directory into this
//! build's data directory as one v1 event log.
//!
//! The source is only read: nothing under the source is created, modified or
//! deleted. Every diagnostic — what was dropped and why — goes to stderr, because
//! stdout carries the machine-readable answer a script reads.

use std::io::Write;
use std::path::{Path, PathBuf};

use hearth_import as importold;

use crate::{one_line, reorder_flags, write_json};

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "usage: hearth import <old-dir> [flags]

Import one previous-generation session directory (the retired TypeScript hearth's
session.jsonl / session.v<N>.jsonl layout) into this build's data directory as
one new v1 event log. The source is only read: nothing under <old-dir> is
created, modified or deleted, and <old-dir> may be a single session directory or
a whole old data root (the newest session is selected, with a warning).

flags:
  --dry-run          print the plan and write nothing
  --session <id>     id for the imported session (default: a fresh timestamped id)
  --data-dir <dir>   data directory to write into; the log lands in
                     <dir>/sessions/<id>.jsonl (default: the hearth home's
                     configured dataDir)
  --json             print the plan (with --dry-run) or the result as one JSON
                     document on stdout
  -h, --help         this message

stdout, success: the new session id, then the written log path, one per line;
with --json, one JSON document instead. stdout, --dry-run: the plan (source,
format, session, events, log, dropped records with their reasons, warnings),
or the same plan as JSON. Diagnostics go to stderr.

exit codes: 0 ok, 1 failure, 2 usage
";

/// The flags that consume the next argument, so `hearth import DIR --session x`
/// parses as documented instead of treating the flags as more positionals.
const VALUE_FLAGS: &[&str] = &["session", "data-dir"];

#[derive(Debug, Default)]
struct Options {
    dry_run: bool,
    json: bool,
    session: String,
    data_dir: String,
    source: String,
}

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let opt = match parse(args, stderr) {
        Ok(opt) => opt,
        Err(code) => return code,
    };

    // Detect first: a bad source is the common failure, and it must not depend on
    // the data directory being resolvable.
    let source = match importold::detect(Path::new(&opt.source)) {
        Ok(source) => source,
        Err(err) => {
            let _ = writeln!(stderr, "hearth import: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };
    let mut plan = match importold::make_plan(&source) {
        Ok(plan) => plan,
        Err(err) => {
            let _ = writeln!(stderr, "hearth import: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };
    // `--session` overrides the id the plan allocated. `write` re-derives the
    // mapping under this id and refuses the plan if the event count changed.
    if !opt.session.trim().is_empty() {
        plan.session_id = opt.session.trim().to_string();
    }

    let data_dir = match resolve_data_dir(&opt) {
        Ok(dir) => dir,
        Err(err) => {
            let _ = writeln!(stderr, "hearth import: {}", one_line(&err));
            return EXIT_FAILURE;
        }
    };
    let log_path = data_dir
        .join("sessions")
        .join(format!("{}.jsonl", plan.session_id));

    if opt.dry_run {
        if opt.json {
            let code = write_json(
                stdout,
                stderr,
                "import",
                &PlanDoc {
                    source: source.dir.display().to_string(),
                    format: source.format.clone(),
                    session_id: plan.session_id.clone(),
                    events: plan.events,
                    log: log_path.display().to_string(),
                    dropped: plan.dropped.clone(),
                    warnings: source.warnings.clone(),
                },
            );
            if code != EXIT_OK {
                return code;
            }
        } else {
            write_plan(stdout, &source, &plan, &log_path);
        }
        // The hint is a diagnostic: stdout stays the plan (or the JSON document),
        // so a script can diff two plans without stripping prose.
        let _ = writeln!(
            stderr,
            "hearth import: dry run, nothing was written (drop --dry-run to import)"
        );
        return EXIT_OK;
    }

    let written = match importold::write(&data_dir, &source, &plan) {
        Ok(path) => path,
        Err(err) => {
            if importold::is_exists(&err) {
                // Naming the id is the point: the operator has to know which
                // session collided before deciding what to import under instead.
                let _ = writeln!(
                    stderr,
                    "hearth import: session {} already exists, nothing was written ({})",
                    plan.session_id,
                    one_line(err.message())
                );
                return EXIT_FAILURE;
            }
            let _ = writeln!(stderr, "hearth import: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };

    if opt.json {
        let code = write_json(
            stdout,
            stderr,
            "import",
            &ResultDoc {
                session_id: plan.session_id.clone(),
                log: written.clone(),
                events: plan.events,
                dropped: plan.dropped.clone(),
                warnings: source.warnings.clone(),
            },
        );
        if code != EXIT_OK {
            return code;
        }
    } else if let Err(err) = writeln!(stdout, "{}\n{}", plan.session_id, written) {
        let _ = writeln!(
            stderr,
            "hearth import: write result: {}",
            one_line(&err.to_string())
        );
        return EXIT_FAILURE;
    }

    let _ = writeln!(
        stderr,
        "hearth import: imported {} events from {} into session {}",
        plan.events,
        source.dir.display(),
        plan.session_id
    );
    // The old log is wider than v1: say what did not survive the mapping instead
    // of letting the new session look complete.
    for reason in &plan.dropped {
        let _ = writeln!(stderr, "hearth import: dropped: {}", one_line(reason));
    }
    for warning in &source.warnings {
        let _ = writeln!(stderr, "hearth import: warning: {}", one_line(warning));
    }
    EXIT_OK
}

fn parse(args: &[String], stderr: &mut dyn Write) -> Result<Options, i32> {
    let ordered = reorder_flags(args, VALUE_FLAGS);
    let mut opt = Options::default();
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;

    while i < ordered.len() {
        let arg = ordered[i].clone();
        i += 1;
        if arg == "--" {
            rest.extend(ordered[i..].iter().cloned());
            break;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        // Anything that is not a flag is a positional argument. Matching flags by
        // name alone would report every positional as an unknown flag.
        if !name.starts_with('-') {
            rest.push(arg);
            continue;
        }
        match name.as_str() {
            "-h" | "--help" => rest.push(arg),
            "--dry-run" => opt.dry_run = true,
            "--json" => opt.json = true,
            "--session" | "--data-dir" => {
                let value = match inline {
                    Some(value) => value,
                    None => match ordered.get(i) {
                        Some(next) => {
                            i += 1;
                            next.clone()
                        }
                        None => {
                            let _ =
                                writeln!(stderr, "hearth import: {name} needs a value\n\n{USAGE}");
                            return Err(EXIT_USAGE);
                        }
                    },
                };
                if name == "--session" {
                    opt.session = value;
                } else {
                    opt.data_dir = value;
                }
            }
            other => {
                let _ = writeln!(stderr, "hearth import: unknown flag {other:?}\n\n{USAGE}");
                return Err(EXIT_USAGE);
            }
        }
    }

    match rest.len() {
        0 => {
            let _ = writeln!(
                stderr,
                "hearth import: expected one source directory\n\n{USAGE}"
            );
            Err(EXIT_USAGE)
        }
        1 => {
            opt.source = rest.remove(0);
            if !opt.session.trim().is_empty() && !valid_session_id(opt.session.trim()) {
                let _ = writeln!(
                    stderr,
                    "hearth import: --session {opt:?} is not a usable session id\n\n{USAGE}",
                    opt = opt.session
                );
                return Err(EXIT_USAGE);
            }
            Ok(opt)
        }
        many => {
            let _ = writeln!(
                stderr,
                "hearth import: expected one source directory, got {many}\n\n{USAGE}"
            );
            Err(EXIT_USAGE)
        }
    }
}

/// The same rule `hearth_core::SessionId::parse` enforces, checked before anything
/// is read so a typo cannot half-import under an id nothing can open.
fn valid_session_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `--data-dir` wins; otherwise the configured data directory.
fn resolve_data_dir(opt: &Options) -> Result<PathBuf, String> {
    if !opt.data_dir.trim().is_empty() {
        return Ok(PathBuf::from(opt.data_dir.trim()));
    }
    crate::default_data_dir().map_err(|err| one_line(err.message()))
}

/// The plan report: one line per field, then the dropped records and warnings
/// indented under it.
fn write_plan(
    stdout: &mut dyn Write,
    source: &importold::Source,
    plan: &importold::Plan,
    log: &Path,
) {
    let _ = writeln!(stdout, "source\t{}", source.dir.display());
    let _ = writeln!(stdout, "format\t{}", source.format);
    let _ = writeln!(stdout, "session\t{}", plan.session_id);
    let _ = writeln!(stdout, "events\t{}", plan.events);
    let _ = writeln!(stdout, "log\t{}", log.display());
    let _ = writeln!(stdout, "dropped\t{}", plan.dropped.len());
    let _ = writeln!(stdout, "warnings\t{}", source.warnings.len());
    for reason in &plan.dropped {
        let _ = writeln!(stdout, "  - {reason}");
    }
    for warning in &source.warnings {
        let _ = writeln!(stdout, "  - {warning}");
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanDoc {
    source: String,
    format: String,
    session_id: String,
    events: usize,
    log: String,
    dropped: Vec<String>,
    warnings: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ResultDoc {
    session_id: String,
    log: String,
    events: usize,
    dropped: Vec<String>,
    warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn requires_exactly_one_source() {
        let mut err = Vec::new();
        assert_eq!(parse(&args(&[]), &mut err).unwrap_err(), EXIT_USAGE);
        let mut err = Vec::new();
        assert_eq!(parse(&args(&["a", "b"]), &mut err).unwrap_err(), EXIT_USAGE);
    }

    #[test]
    fn lifts_flags_written_after_the_source() {
        let mut err = Vec::new();
        let opt = parse(
            &args(&["old-dir", "--dry-run", "--session", "s1"]),
            &mut err,
        )
        .expect("parse");
        assert_eq!(opt.source, "old-dir");
        assert!(opt.dry_run);
        assert_eq!(opt.session, "s1");
    }

    #[test]
    fn rejects_an_unusable_session_id_before_reading() {
        let mut err = Vec::new();
        let code = parse(&args(&["old", "--session", "../escape"]), &mut err).unwrap_err();
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("not a usable session id"));
    }

    #[test]
    fn a_missing_source_is_a_runtime_failure_without_a_package_name() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&args(&[r"no\such\dir", "--dry-run"]), &mut out, &mut err);
        assert_eq!(code, EXIT_FAILURE);
        assert!(out.is_empty(), "stdout must stay empty");
        let text = String::from_utf8_lossy(&err).to_string();
        assert!(text.starts_with("hearth import: "), "{text}");
        assert!(text.contains("no such directory"), "{text}");
        assert!(
            !text.contains("importold:"),
            "leaked the package name: {text}"
        );
    }
}
