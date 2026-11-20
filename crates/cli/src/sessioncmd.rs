//! `forge session`: commands that operate on a session log without running a
//! turn. The first one is `fork`, which is a rewrite-free copy and says nothing
//! about a provider, a model or stdin — so an operator can branch a session
//! before deciding what to ask it.

use std::io::Write;
use std::path::PathBuf;

use forge_core::{Error, ErrorKind};
use forge_session::{fork, ForkOptions};

use crate::{one_line, reorder_flags};

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "usage: forge session <command> [flags]
Try 'forge session fork --help' for one command.

Commands:
  fork <parentId>     copy a session up to one of its seqs into a new session
  archive <sessionId> record that the session starts no new work

Flags for fork:
  --at <seq>        last record to copy (default: the parent's last turn/end,
                    so a fork never starts mid-turn and never inherits an open turn)
  --id <newId>      id for the copy (default: a new timestamped id)
  --json            print one JSON object instead of the new session id

Flags for archive:
  --reason <text>   why, kept in the record so a later reader is not guessing
  --json            print one JSON object instead of a sentence
";

/// Flags of `fork` that consume the next argument.
const VALUE_FLAGS: &[&str] = &["at", "id"];

#[derive(Debug, Default)]
struct Options {
    at: u64,
    at_set: bool,
    new_id: String,
    json: bool,
}

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(first) = args.first() else {
        let _ = stderr.write_all(USAGE.as_bytes());
        return EXIT_USAGE;
    };
    match first.as_str() {
        "-h" | "--help" | "help" => {
            let _ = stdout.write_all(USAGE.as_bytes());
            EXIT_OK
        }
        "fork" => run_fork(&args[1..], stdout, stderr),
        "archive" => run_archive(&args[1..], stdout, stderr),
        other => {
            // Naming the word it did not recognise, because a typo'd subcommand is
            // the common case and "unknown command" alone leaves the operator
            // guessing which of their words was wrong.
            let _ = writeln!(
                stderr,
                "forge session: unknown command {other:?}\n\n{USAGE}"
            );
            EXIT_USAGE
        }
    }
}

/// Records that a session starts no new work.
///
/// This is the only writer of `session/archived`. It is idempotent on purpose: an
/// operator who re-runs it after a failure asked for the same state, and writing
/// a second record would only make `is_archived` do more work for the same
/// answer.
fn run_archive(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let mut id: Option<String> = None;
    let mut reason = String::new();
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        index += 1;
        match arg.as_str() {
            "--json" => json = true,
            "--reason" => {
                let Some(value) = args.get(index) else {
                    let _ = writeln!(
                        stderr,
                        "forge session archive: --reason needs a value\n\n{USAGE}"
                    );
                    return EXIT_USAGE;
                };
                reason = value.clone();
                index += 1;
            }
            other if other.starts_with("--reason=") => {
                reason = other["--reason=".len()..].to_string();
            }
            other if other.starts_with('-') => {
                let _ = writeln!(
                    stderr,
                    "forge session archive: unknown flag {other:?}\n\n{USAGE}"
                );
                return EXIT_USAGE;
            }
            other => {
                if id.is_some() {
                    let _ = writeln!(
                        stderr,
                        "forge session archive: one session id at a time\n\n{USAGE}"
                    );
                    return EXIT_USAGE;
                }
                id = Some(other.to_string());
            }
        }
    }
    let Some(id) = id else {
        let _ = writeln!(
            stderr,
            "forge session archive: needs a session id\n\n{USAGE}"
        );
        return EXIT_USAGE;
    };

    let data_dir = match crate::default_data_dir() {
        Ok(dir) => dir,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge session archive: read config: {}",
                one_line(err.message())
            );
            return EXIT_FAILURE;
        }
    };
    let path = data_dir.join("sessions").join(format!("{id}.jsonl"));
    if !path.is_file() {
        // Named, not silent: "archived" for a session that does not exist would
        // let a typo turn into a confident claim.
        let _ = writeln!(
            stderr,
            "forge session archive: no such session log: {}",
            path.display()
        );
        return EXIT_FAILURE;
    }
    let store = match forge_session::Store::open(&path, forge_session::Options::default()) {
        Ok(store) => store,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge session archive: open {}: {}",
                path.display(),
                one_line(err.message())
            );
            return EXIT_FAILURE;
        }
    };
    let already = forge_subagent::archive::is_archived(&data_dir, &id).unwrap_or(false);
    if !already {
        let event = forge_core::Event::new(
            &id,
            forge_core::EventType::SESSION_ARCHIVED,
            Some(forge_subagent::archive::payload(&reason)),
        );
        if let Err(err) = store.append(&event) {
            let _ = writeln!(
                stderr,
                "forge session archive: append: {}",
                one_line(err.message())
            );
            return EXIT_FAILURE;
        }
    }

    if json {
        let doc = serde_json::json!({
            "session": id,
            "archived": true,
            "already": already,
            "reason": reason.trim(),
        });
        return match serde_json::to_string(&doc) {
            Ok(line) => {
                let _ = writeln!(stdout, "{line}");
                EXIT_OK
            }
            Err(err) => {
                let _ = writeln!(
                    stderr,
                    "forge session archive: report: {}",
                    one_line(&err.to_string())
                );
                EXIT_FAILURE
            }
        };
    }
    let note = if already { " (already archived)" } else { "" };
    if let Err(err) = writeln!(stdout, "archived {id}{note}") {
        let _ = writeln!(
            stderr,
            "forge session archive: write: {}",
            one_line(&err.to_string())
        );
        return EXIT_FAILURE;
    }
    EXIT_OK
}

/// Copies a parent log into a new session id and prints that id, so a shell can
/// do `child=$(forge session fork "$parent")`. Every diagnostic, including the
/// summary of what was copied, goes to stderr.
fn run_fork(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let (opt, parent) = match parse(args, stderr) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };

    let new_id = if opt.new_id.trim().is_empty() {
        match crate::new_session_id() {
            Ok(id) => id,
            Err(err) => {
                let _ = writeln!(stderr, "forge session fork: {}", one_line(err.message()));
                return EXIT_FAILURE;
            }
        }
    } else {
        opt.new_id.trim().to_string()
    };

    let data_dir = match crate::default_data_dir() {
        Ok(dir) => dir,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge session fork: read config: {}",
                one_line(err.message())
            );
            return EXIT_FAILURE;
        }
    };
    // The sessions directory is deliberately not created here: a fork can only
    // succeed when the parent log already exists in it, so creating it would only
    // ever be the first symptom of a mistyped id.
    let sessions = data_dir.join("sessions");
    let options = ForkOptions {
        parent_path: sessions.join(format!("{parent}.jsonl")),
        target_path: sessions.join(format!("{new_id}.jsonl")),
        target_id: new_id.clone(),
        at: opt.at,
    };

    let result = match fork(options) {
        Ok(result) => result,
        Err(err) => {
            let message = describe_fork_error(&err, &parent, &new_id);
            let _ = writeln!(stderr, "forge session fork: {}", one_line(&message));
            // A missing parent or a bad cut is the caller's mistake, not a
            // runtime failure: exit 2 tells a script not to retry it as-is.
            return if is_usage_error(&err) {
                EXIT_USAGE
            } else {
                EXIT_FAILURE
            };
        }
    };

    if opt.json {
        let doc = ForkDoc {
            parent: result.parent_id.clone(),
            session: result.session_id.clone(),
            at_seq: result.forked_at_seq,
            events: result.events,
            bytes: result.bytes,
        };
        return match serde_json::to_string(&doc) {
            Ok(line) => {
                let _ = writeln!(stdout, "{line}");
                EXIT_OK
            }
            Err(err) => {
                let _ = writeln!(
                    stderr,
                    "forge session fork: report: {}",
                    one_line(&err.to_string())
                );
                EXIT_FAILURE
            }
        };
    }

    if let Err(err) = writeln!(stdout, "{}", result.session_id) {
        let _ = writeln!(
            stderr,
            "forge session fork: write session id: {}",
            one_line(&err.to_string())
        );
        return EXIT_FAILURE;
    }
    // The summary is stderr so stdout stays a bare id. An operator who asked for
    // the id still gets told which record the copy stops at, which is the one fact
    // that makes the fork reproducible.
    let _ = writeln!(
        stderr,
        "forge session fork: forked {} at seq {} into {} ({} events, {} bytes)",
        result.parent_id, result.forked_at_seq, result.session_id, result.events, result.bytes
    );
    EXIT_OK
}

fn parse(args: &[String], stderr: &mut dyn Write) -> Result<(Options, String), i32> {
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
            "--json" => opt.json = true,
            "--at" | "--id" => {
                let value = match inline {
                    Some(value) => value,
                    None => match ordered.get(i) {
                        Some(next) => {
                            i += 1;
                            next.clone()
                        }
                        None => {
                            let _ = writeln!(
                                stderr,
                                "forge session fork: {name} needs a value\n\n{USAGE}"
                            );
                            return Err(EXIT_USAGE);
                        }
                    },
                };
                if name == "--at" {
                    match value.trim().parse::<u64>() {
                        Ok(at) => {
                            opt.at = at;
                            opt.at_set = true;
                        }
                        Err(_) => {
                            let _ = writeln!(
                                stderr,
                                "forge session fork: --at {value:?} is not a record number\n"
                            );
                            return Err(EXIT_USAGE);
                        }
                    }
                } else {
                    opt.new_id = value;
                }
            }
            other => {
                let _ = writeln!(
                    stderr,
                    "forge session fork: unknown flag {other:?}\n\n{USAGE}"
                );
                return Err(EXIT_USAGE);
            }
        }
    }

    // `--at 0` is not "the first record": record numbers are dense from 1, so zero
    // means the caller forgot a value. Falling back to the default cut would fork
    // a different point than the one asked for.
    if opt.at_set && opt.at == 0 {
        let _ = writeln!(
            stderr,
            "forge session fork: --at 0 names no record: seqs are dense from 1\n"
        );
        return Err(EXIT_USAGE);
    }
    if !opt.new_id.trim().is_empty() && !valid_session_id(opt.new_id.trim()) {
        let _ = writeln!(
            stderr,
            "forge session fork: --id {:?} is not a usable session id\n",
            opt.new_id.trim()
        );
        return Err(EXIT_USAGE);
    }
    match rest.len() {
        1 => Ok((opt, rest.remove(0))),
        many => {
            let _ = writeln!(
                stderr,
                "forge session fork: expected exactly one parent session id, got {many}\n\n{USAGE}"
            );
            Err(EXIT_USAGE)
        }
    }
}

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

/// Turns a fork error into one sentence that names the ids the operator used, so
/// "not found" is never ambiguous about *what* was not found.
fn describe_fork_error(err: &Error, parent: &str, new_id: &str) -> String {
    let text = err.message();
    // `forge_session::fork` reports a missing parent as "fork parent log does not
    // exist"; naming the id turns that into something an operator can act on.
    if text.contains("does not exist") || text.contains("no session log at") {
        return format!("no session {parent} in this data directory ({text})");
    }
    if text.contains("already exists") {
        return format!("session {new_id} already exists; pass --id to fork under another name");
    }
    text.to_string()
}

/// Which fork failures are the caller's mistake rather than a runtime failure.
fn is_usage_error(err: &Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::NotFound | ErrorKind::Invalid | ErrorKind::Conflict
    )
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ForkDoc {
    parent: String,
    session: String,
    at_seq: u64,
    events: usize,
    bytes: u64,
}

/// The parent log path a fork reads, exposed so a caller can report it without
/// rebuilding the layout.
pub fn parent_path(data_dir: &std::path::Path, parent_id: &str) -> PathBuf {
    data_dir.join("sessions").join(format!("{parent_id}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn unknown_subcommand_names_the_word() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&args(&["frok"]), &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("\"frok\""));
    }

    #[test]
    fn session_help_goes_to_stdout() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(run(&args(&["--help"]), &mut out, &mut err), EXIT_OK);
        assert!(String::from_utf8_lossy(&out).contains("forge session <command>"));
        assert!(err.is_empty());
    }

    #[test]
    fn at_zero_names_no_record() {
        let mut err = Vec::new();
        let code = parse(&args(&["s1", "--at", "0"]), &mut err).unwrap_err();
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("dense from 1"));
    }

    #[test]
    fn requires_exactly_one_parent() {
        let mut err = Vec::new();
        assert_eq!(parse(&args(&[]), &mut err).unwrap_err(), EXIT_USAGE);
        let mut err = Vec::new();
        assert_eq!(parse(&args(&["a", "b"]), &mut err).unwrap_err(), EXIT_USAGE);
    }

    #[test]
    fn lifts_flags_written_after_the_parent_id() {
        let mut err = Vec::new();
        let (opt, parent) = parse(&args(&["s1", "--at", "7", "--json"]), &mut err).expect("parse");
        assert_eq!(parent, "s1");
        assert_eq!(opt.at, 7);
        assert!(opt.at_set);
        assert!(opt.json);
    }

    #[test]
    fn rejects_an_unusable_new_id() {
        let mut err = Vec::new();
        let code = parse(&args(&["s1", "--id", "../x"]), &mut err).unwrap_err();
        assert_eq!(code, EXIT_USAGE);
    }

    /// The usage is the only place an operator can discover `archive`, so it has
    /// to name it; a command that exists but is undocumented is a command nobody
    /// runs.
    #[test]
    fn session_help_names_archive() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(run(&args(&["--help"]), &mut out, &mut err), EXIT_OK);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("archive <sessionId>"), "{text}");
        assert!(text.contains("--reason <text>"), "{text}");
    }

    #[test]
    fn archive_without_a_session_id_is_a_usage_error() {
        let mut err = Vec::new();
        assert_eq!(
            run(&args(&["archive"]), &mut Vec::new(), &mut err),
            EXIT_USAGE
        );
        let text = String::from_utf8_lossy(&err);
        assert!(text.contains("needs a session id"), "{text}");
    }

    #[test]
    fn archive_refuses_an_unknown_flag() {
        let mut err = Vec::new();
        assert_eq!(
            run(
                &args(&["archive", "s-1", "--bogus"]),
                &mut Vec::new(),
                &mut err
            ),
            EXIT_USAGE
        );
        assert!(
            String::from_utf8_lossy(&err).contains("unknown flag \"--bogus\""),
            "{}",
            String::from_utf8_lossy(&err)
        );
    }

    #[test]
    fn archive_takes_one_session_at_a_time() {
        let mut err = Vec::new();
        assert_eq!(
            run(&args(&["archive", "s-1", "s-2"]), &mut Vec::new(), &mut err),
            EXIT_USAGE
        );
        assert!(
            String::from_utf8_lossy(&err).contains("one session id at a time"),
            "{}",
            String::from_utf8_lossy(&err)
        );
    }

    #[test]
    fn a_missing_parent_is_a_usage_error() {
        let dir = std::env::temp_dir().join(format!("forge-fork-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).expect("temp dir");
        let sessions = dir.join("sessions");

        let err = fork(ForkOptions {
            parent_path: parent_path(&dir, "nope"),
            target_path: sessions.join("child.jsonl"),
            target_id: "child".to_string(),
            at: 0,
        })
        .expect_err("missing parent");
        assert!(is_usage_error(&err), "kind = {:?}", err.kind());
        let described = describe_fork_error(&err, "nope", "child");
        assert!(described.contains("no session nope"), "{described}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
