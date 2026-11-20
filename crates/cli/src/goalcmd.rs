//! `hearth goal` — record, inspect and close the goal of a session.
//!
//! A goal is three events in the session log (`goal/created`, `goal/continued`,
//! `goal/ended`), folded into a status. It lives in the log rather than in a
//! side file so that `hearth chat --resume`, `hearth session fork` and the web UI
//! all see the same thing, and so that a goal outlives the process that started
//! it: the acceptance criterion for P2 is literally "a goal survives a restart
//! because it is in the log".
//!
//! Every command here opens the log, does one thing and drops the handle. The
//! turn loop is the only long-lived writer; the events these commands append
//! take their `seq` from the log, so a command never guesses a number.

use std::io::Write;
use std::path::{Path, PathBuf};

use hearth_core::Result as CoreResult;
use hearth_goal::{Action, CasOutcome, Goal, Status, StopReason};
use hearth_session::{Options as StoreOptions, Store};

use crate::{one_line, write_json, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};

pub const USAGE: &str = "usage: hearth goal <command> [flags]

Commands:
  start    record a new goal in a session log
  status   print the goal a session is pursuing
  end      record that a goal is over

Flags:
  --session <id>       session to act on (start generates one if absent)
  --text <text>        what to keep working on (start)
  --max-rounds <n>     hard cap on rounds (start, default 10)
  --reason <reason>    complete or blocked (end)
  --note <text>        one line of why (end)
  --json               machine-readable output
";

const USAGE_START: &str =
    "usage: hearth goal start --text <text> [--session <id>] [--max-rounds <n>] [--json]
";
const USAGE_STATUS: &str = "usage: hearth goal status --session <id> [--json]
";
const USAGE_END: &str =
    "usage: hearth goal end --session <id> --reason complete|blocked [--note <text>] [--json]
";

#[derive(Debug, Default)]
struct Flags {
    session: String,
    text: String,
    max_rounds: String,
    reason: String,
    note: String,
    json: bool,
}

fn parse(args: &[String], usage: &str, stderr: &mut dyn Write) -> std::result::Result<Flags, i32> {
    let mut flags = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name.trim_start_matches('-'), Some(value.to_string())),
            None => (arg.trim_start_matches('-'), None),
        };
        if !arg.starts_with('-') {
            let _ = writeln!(stderr, "hearth goal: unexpected argument {arg:?}");
            let _ = stderr.write_all(usage.as_bytes());
            return Err(EXIT_USAGE);
        }
        let takes_value = matches!(name, "session" | "text" | "max-rounds" | "reason" | "note");
        let value = if let Some(value) = inline {
            Some(value)
        } else if takes_value && i + 1 < args.len() {
            i += 1;
            Some(args[i].clone())
        } else {
            None
        };
        match name {
            "json" => flags.json = true,
            "session" | "text" | "max-rounds" | "reason" | "note" => {
                let value = match value {
                    Some(value) => value,
                    None => {
                        let _ = writeln!(stderr, "hearth goal: flag needs an argument: -{name}");
                        let _ = stderr.write_all(usage.as_bytes());
                        return Err(EXIT_USAGE);
                    }
                };
                match name {
                    "session" => flags.session = value,
                    "text" => flags.text = value,
                    "max-rounds" => flags.max_rounds = value,
                    "reason" => flags.reason = value,
                    "note" => flags.note = value,
                    _ => unreachable!(),
                }
            }
            other => {
                let _ = writeln!(stderr, "hearth goal: unknown flag -{other}");
                let _ = stderr.write_all(usage.as_bytes());
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(flags)
}

/// The log file for a session, in the same layout every other command uses.
pub fn log_path(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(format!("{session_id}.jsonl"))
}

fn data_dir(stderr: &mut dyn Write, command: &str) -> std::result::Result<PathBuf, i32> {
    match crate::default_data_dir() {
        Ok(dir) => Ok(dir),
        Err(err) => {
            let _ = writeln!(
                stderr,
                "hearth goal {command}: read config: {}",
                one_line(err.message())
            );
            Err(EXIT_FAILURE)
        }
    }
}

/// Open a session log for appending, creating the directory if needed.
fn open_for_write(path: &Path) -> CoreResult<Store> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            hearth_core::Error::new(
                hearth_core::ErrorKind::Io,
                format!("goal: cannot create {}: {err}", parent.display()),
            )
        })?;
    }
    Store::open(path, StoreOptions::default())
}

/// Append one goal event, then drop the handle: see the module comment.
fn append(path: &Path, event: &hearth_core::Event) -> CoreResult<()> {
    let store = open_for_write(path)?;
    store.append(event)?;
    Ok(())
}

/// Every event in the log, or an empty list when the log does not exist.
///
/// A missing log is not an error here: "no goal" and "no session" are the same
/// answer to `hearth goal status`, and the caller says which session it looked
/// at so the operator can spot a typo.
pub fn read_events(path: &Path) -> CoreResult<Vec<hearth_core::Event>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let store = Store::open(path, StoreOptions::default())?;
    let mut events = Vec::new();
    store.read_from(0, |event| {
        events.push(event.clone());
        Ok(())
    })?;
    Ok(events)
}

/// The goal a session is pursuing, if the log holds one.
pub fn open_goal(path: &Path) -> CoreResult<Option<Goal>> {
    let events = read_events(path)?;
    Ok(hearth_goal::fold(&events))
}

/// What this turn's tools said about the goal, if anything.
///
/// `goal_end` does not write events — it puts its verdict in the tool result's
/// `meta`, and the loop that owns the log turns that into `goal/ended`. Reading
/// it back out of the log (rather than out of the tool call) keeps one writer.
///
/// The last verdict wins: a model that changes its mind in the same turn meant
/// the later one, and the earlier one is still in the log for anyone reading.
pub fn round_verdict(events: &[hearth_core::Event]) -> Option<(StopReason, Option<String>)> {
    let mut verdict = None;
    for event in events {
        if event.typ != hearth_core::EventType::TOOL_RESULT.as_str() {
            continue;
        }
        let Some(payload) = event.payload.as_ref() else {
            continue;
        };
        let Some(meta) = payload.get("meta") else {
            continue;
        };
        let Some(raw) = meta
            .get(hearth_tool::GOAL_META_KEY)
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let Some(reason) = StopReason::parse(raw) else {
            continue;
        };
        let note = meta
            .get("note")
            .and_then(|v| v.as_str())
            .map(|text| text.to_string())
            .filter(|text| !text.trim().is_empty());
        verdict = Some((reason, note));
    }
    verdict
}

/// Record a new goal. Returns the goal as written.
pub fn start_goal(path: &Path, session_id: &str, text: &str, max_rounds: u32) -> CoreResult<Goal> {
    let goal = Goal::new(hearth_core::new_id()?, text.trim(), max_rounds);
    hearth_goal::validate(&goal)?;
    append(path, &hearth_goal::goal_created(session_id, &goal))?;
    Ok(goal)
}

/// Record that a round is being opened.
pub fn continue_goal(path: &Path, session_id: &str, goal: &Goal, round: u32) -> CoreResult<()> {
    append(path, &hearth_goal::goal_continued(session_id, goal, round))
}

/// Record that a goal is over.
pub fn end_goal(
    path: &Path,
    session_id: &str,
    goal: &Goal,
    reason: StopReason,
    note: Option<&str>,
) -> CoreResult<()> {
    append(
        path,
        &hearth_goal::goal_ended(session_id, goal, reason, note),
    )
}

/// Open the next round only if the log still shows the revision we folded.
///
/// The two entries that race for one goal each hold a `Goal` with the same
/// `revision`. This re-reads the log through the same handle that will write, so
/// whichever writes second sees `found != expected` and is turned away with
/// `Stale` instead of opening a second continuation for a single turn of work.
///
/// `expected` is `goal.revision`; on success the new revision is `expected + 1`,
/// matching the `+1` [`hearth_goal::goal_continued`] puts in the payload.
pub fn append_continued_if_current(
    store: &Store,
    session_id: &str,
    goal: &Goal,
    round: u32,
) -> CoreResult<CasOutcome> {
    // Read the whole log from disk (not the handle's cached seq): the other entry
    // may have written through a different handle since we folded.
    let mut events = Vec::new();
    store.read_from(0, |event| {
        events.push(event.clone());
        Ok(())
    })?;
    let found = hearth_goal::fold(&events)
        .map(|current| current.revision)
        .unwrap_or(0);
    if found != goal.revision {
        return Ok(CasOutcome::Stale {
            expected: goal.revision,
            found,
        });
    }
    store.append(&hearth_goal::goal_continued(session_id, goal, round))?;
    Ok(CasOutcome::Written(goal.revision + 1))
}

/// Path convenience for [`append_continued_if_current`]: open, check, write, drop.
pub fn append_continued_at(
    path: &Path,
    session_id: &str,
    goal: &Goal,
    round: u32,
) -> CoreResult<CasOutcome> {
    let store = open_for_write(path)?;
    append_continued_if_current(&store, session_id, goal, round)
}

/// Close an open goal because the operator paused the run.
///
/// Writes `goal/ended{reason:"cancelled"}` — `cancelled` is an end reason the
/// goal crate already parses to [`Status::Cancelled`], so a resumed session sees
/// the goal as stopped rather than still asking for another round. Returns
/// `true` when it wrote and `false` when there was nothing open to end, which
/// makes a pause that arrives twice harmless: the second call writes no second
/// `goal/ended`.
pub fn cancel_open_goal(path: &Path, session_id: &str, note: Option<&str>) -> CoreResult<bool> {
    let Some(goal) = open_goal(path)? else {
        return Ok(false);
    };
    if goal.status != Status::Active {
        return Ok(false);
    }
    end_goal(path, session_id, &goal, StopReason::Cancelled, note)?;
    Ok(true)
}

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(command) = args.first() else {
        let _ = stderr.write_all(USAGE.as_bytes());
        return EXIT_USAGE;
    };
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let rest = &args[1..];
    match command.as_str() {
        "start" => start(rest, stdout, stderr),
        "status" => status(rest, stdout, stderr),
        "end" => end(rest, stdout, stderr),
        other => {
            let _ = writeln!(stderr, "hearth goal: unknown command {other:?}");
            let _ = stderr.write_all(USAGE.as_bytes());
            EXIT_USAGE
        }
    }
}

fn start(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_START.as_bytes());
        return EXIT_OK;
    }
    let flags = match parse(args, USAGE_START, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    if flags.text.trim().is_empty() {
        let _ = writeln!(
            stderr,
            "hearth goal start: --text is required: a goal nobody wrote down cannot be finished"
        );
        let _ = stderr.write_all(USAGE_START.as_bytes());
        return EXIT_USAGE;
    }
    let max_rounds = match parse_rounds(&flags.max_rounds) {
        Ok(rounds) => rounds,
        Err(message) => {
            let _ = writeln!(stderr, "hearth goal start: {message}");
            return EXIT_USAGE;
        }
    };
    let dir = match data_dir(stderr, "start") {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let session_id = if flags.session.trim().is_empty() {
        match crate::new_session_id() {
            Ok(id) => id,
            Err(err) => {
                let _ = writeln!(stderr, "hearth goal start: {}", one_line(err.message()));
                return EXIT_FAILURE;
            }
        }
    } else {
        flags.session.trim().to_string()
    };
    let path = log_path(&dir, &session_id);
    let goal = match start_goal(&path, &session_id, &flags.text, max_rounds) {
        Ok(goal) => goal,
        Err(err) => {
            let _ = writeln!(stderr, "hearth goal start: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };

    if flags.json {
        return write_json(
            stdout,
            stderr,
            "goal start",
            &serde_json::json!({
                "session": session_id,
                "goalId": goal.id,
                "text": goal.text,
                "maxRounds": goal.max_rounds,
                "log": path.display().to_string(),
            }),
        );
    }
    let _ = writeln!(stdout, "{}", session_id);
    let _ = writeln!(
        stderr,
        "hearth goal: recorded {}/{} in session {} (max {} rounds)",
        goal.id, goal.text, session_id, goal.max_rounds
    );
    EXIT_OK
}

fn status(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_STATUS.as_bytes());
        return EXIT_OK;
    }
    let flags = match parse(args, USAGE_STATUS, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    if flags.session.trim().is_empty() {
        let _ = writeln!(stderr, "hearth goal status: --session is required");
        let _ = stderr.write_all(USAGE_STATUS.as_bytes());
        return EXIT_USAGE;
    }
    let session_id = flags.session.trim().to_string();
    let dir = match data_dir(stderr, "status") {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let path = log_path(&dir, &session_id);
    let goal = match open_goal(&path) {
        Ok(goal) => goal,
        Err(err) => {
            let _ = writeln!(stderr, "hearth goal status: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };

    if flags.json {
        let body = match &goal {
            Some(goal) => serde_json::json!({
                "session": session_id,
                "goal": {
                    "goalId": goal.id,
                    "text": goal.text,
                    "maxRounds": goal.max_rounds,
                    "rounds": goal.rounds,
                    "revision": goal.revision,
                    "status": goal.status.as_str(),
                },
            }),
            None => serde_json::json!({"session": session_id, "goal": null}),
        };
        return write_json(stdout, stderr, "goal status", &body);
    }

    match &goal {
        Some(goal) => {
            let _ = writeln!(stdout, "goal      {}", goal.text);
            let _ = writeln!(stdout, "id        {}", goal.id);
            let _ = writeln!(stdout, "status    {}", goal.status.as_str());
            // The CAS fence: two entries that both folded this revision must
            // agree on it before either opens a round, so it is worth showing.
            let _ = writeln!(stdout, "revision  {}", goal.revision);
            // The log records continuations, so a goal whose first turn is
            // running has zero of them; the turn it is on is one more. Printing
            // the raw count would tell the operator a goal with a two-turn
            // budget is on round 1 of 2 after both turns were spent.
            let _ = writeln!(
                stdout,
                "round     {} of {}",
                goal.rounds + 1,
                goal.max_rounds
            );
            match goal.next_action() {
                Action::Continue { round } => {
                    let _ = writeln!(stdout, "next      round {}", round + 1);
                }
                Action::Stop { reason } => {
                    let _ = writeln!(stdout, "next      none ({})", reason.as_str());
                }
            }
        }
        None => {
            // Saying "no goal" without saying which log was read is how a typo
            // in a session id looks like a finished goal.
            let _ = writeln!(
                stdout,
                "no goal recorded in session {session_id} ({})",
                path.display()
            );
        }
    }
    EXIT_OK
}

fn end(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_END.as_bytes());
        return EXIT_OK;
    }
    let flags = match parse(args, USAGE_END, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    if flags.session.trim().is_empty() {
        let _ = writeln!(stderr, "hearth goal end: --session is required");
        let _ = stderr.write_all(USAGE_END.as_bytes());
        return EXIT_USAGE;
    }
    let reason = match flags.reason.trim() {
        "complete" => StopReason::Complete,
        "blocked" => StopReason::Blocked,
        "" => {
            let _ = writeln!(
                stderr,
                "hearth goal end: --reason is required: complete or blocked"
            );
            let _ = stderr.write_all(USAGE_END.as_bytes());
            return EXIT_USAGE;
        }
        other => {
            let _ = writeln!(
                stderr,
                "hearth goal end: unknown --reason {other:?}: use complete or blocked"
            );
            return EXIT_USAGE;
        }
    };
    let session_id = flags.session.trim().to_string();
    let dir = match data_dir(stderr, "end") {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let path = log_path(&dir, &session_id);
    let goal = match open_goal(&path) {
        Ok(Some(goal)) => goal,
        Ok(None) => {
            let _ = writeln!(
                stderr,
                "hearth goal end: no goal recorded in session {session_id}"
            );
            return EXIT_FAILURE;
        }
        Err(err) => {
            let _ = writeln!(stderr, "hearth goal end: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };
    if goal.status != Status::Active {
        // Ending a goal twice would write a second reason for one goal, and the
        // fold would silently prefer the later one. Refuse instead.
        let _ = writeln!(
            stderr,
            "hearth goal end: the goal in session {session_id} is already {} ({})",
            goal.status.as_str(),
            goal.id
        );
        return EXIT_FAILURE;
    }
    let note = flags.note.trim();
    let note = if note.is_empty() { None } else { Some(note) };
    if let Err(err) = end_goal(&path, &session_id, &goal, reason, note) {
        let _ = writeln!(stderr, "hearth goal end: {}", one_line(err.message()));
        return EXIT_FAILURE;
    }
    if flags.json {
        return write_json(
            stdout,
            stderr,
            "goal end",
            &serde_json::json!({
                "session": session_id,
                "goalId": goal.id,
                "reason": reason.as_str(),
                "status": reason.status().as_str(),
            }),
        );
    }
    let _ = writeln!(stdout, "{} {}", reason.status().as_str(), goal.id);
    EXIT_OK
}

fn parse_rounds(raw: &str) -> std::result::Result<u32, String> {
    if raw.trim().is_empty() {
        return Ok(hearth_goal::DEFAULT_MAX_ROUNDS);
    }
    match raw.trim().parse::<u32>() {
        Ok(0) => Err("--max-rounds must be at least 1: a goal with no rounds can never run".into()),
        Ok(rounds) => Ok(rounds),
        Err(_) => Err(format!(
            "--max-rounds {raw:?} is not a number: give the round cap as a positive integer"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir()
            .join("hearth-goal-cmd-test")
            .join(format!("{}-{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create dir");
        path
    }

    fn run_with(args: &[&str]) -> (i32, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let code = run(&owned, &mut stdout, &mut stderr);
        (
            code,
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
        )
    }

    #[test]
    fn a_goal_started_then_read_then_ended_is_one_story() {
        let path = temp_dir("story").join("s.jsonl");
        let goal = start_goal(&path, "s", "ship P2", 3).expect("start");

        let folded = open_goal(&path).expect("read").expect("present");
        assert_eq!(folded.id, goal.id);
        assert_eq!(folded.text, "ship P2");
        assert_eq!(folded.max_rounds, 3);
        assert_eq!(folded.rounds, 0);
        assert!(folded.is_open());

        continue_goal(&path, "s", &goal, 1).expect("continue");
        let folded = open_goal(&path).expect("read").expect("present");
        assert_eq!(folded.rounds, 1);
        assert_eq!(folded.next_action(), Action::Continue { round: 2 });

        end_goal(&path, "s", &goal, StopReason::Blocked, Some("no key")).expect("end");
        let folded = open_goal(&path).expect("read").expect("present");
        assert_eq!(folded.status, Status::Blocked);
        assert!(!folded.is_open());
    }

    #[test]
    fn a_session_with_no_log_has_no_goal_and_no_error() {
        let path = temp_dir("missing").join("never.jsonl");
        assert_eq!(open_goal(&path).expect("read"), None);
    }

    #[test]
    fn the_budget_stops_the_fold_rather_than_the_loop() {
        let path = temp_dir("budget").join("s.jsonl");
        let goal = start_goal(&path, "s", "count to two", 2).expect("start");
        continue_goal(&path, "s", &goal, 1).expect("round 1");
        continue_goal(&path, "s", &goal, 2).expect("round 2");
        let folded = open_goal(&path).expect("read").expect("present");
        assert_eq!(
            folded.next_action(),
            Action::Stop {
                reason: StopReason::Exhausted
            }
        );
    }

    #[test]
    fn an_empty_goal_text_is_refused_before_the_log_is_touched() {
        let path = temp_dir("empty").join("s.jsonl");
        let error = start_goal(&path, "s", "   ", 3).expect_err("refused");
        assert!(error.message().contains("empty goal text"), "{error:?}");
        assert!(!path.exists(), "a rejected goal must not create a log");
    }

    #[test]
    fn the_round_cap_is_a_positive_integer() {
        assert_eq!(
            parse_rounds("").expect("default"),
            hearth_goal::DEFAULT_MAX_ROUNDS
        );
        assert_eq!(parse_rounds(" 4 ").expect("parses"), 4);
        assert!(parse_rounds("0").is_err());
        assert!(parse_rounds("many").is_err());
    }

    #[test]
    fn an_unknown_command_prints_the_usage_to_stderr() {
        let (code, stdout, stderr) = run_with(&["launch"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(stdout.is_empty());
        assert!(stderr.contains("unknown command"), "{stderr}");
        assert!(stderr.contains("usage: hearth goal"), "{stderr}");
    }

    #[test]
    fn help_goes_to_stdout_and_exits_zero() {
        let (code, stdout, _stderr) = run_with(&["--help"]);
        assert_eq!(code, EXIT_OK);
        assert!(stdout.contains("usage: hearth goal"));
    }

    /// A tool result carrying a goal verdict, the way the agent writes one.
    fn tool_result(seq: u64, meta: serde_json::Value) -> hearth_core::Event {
        let mut event = hearth_core::Event::new(
            "s",
            hearth_core::EventType::TOOL_RESULT,
            Some(
                serde_json::json!({"callId": "c1", "tool": "goal_end", "ok": true, "output": "x", "meta": meta}),
            ),
        );
        event.seq = seq;
        event
    }

    #[test]
    fn a_verdict_is_read_out_of_the_tool_result_meta() {
        let events = vec![tool_result(
            1,
            serde_json::json!({"goalEnd": "complete", "note": "all done"}),
        )];
        assert_eq!(
            round_verdict(&events),
            Some((StopReason::Complete, Some("all done".to_string())))
        );
    }

    #[test]
    fn a_verdict_without_a_note_is_still_a_verdict() {
        let events = vec![tool_result(1, serde_json::json!({"goalEnd": "blocked"}))];
        assert_eq!(round_verdict(&events), Some((StopReason::Blocked, None)));
    }

    /// Two calls in one turn: the model changed its mind, and the later word is
    /// the one that counts. The earlier line stays in the log for the reader.
    #[test]
    fn the_last_verdict_of_a_turn_wins() {
        let events = vec![
            tool_result(1, serde_json::json!({"goalEnd": "blocked"})),
            tool_result(
                2,
                serde_json::json!({"goalEnd": "complete", "note": "finished"}),
            ),
        ];
        assert_eq!(
            round_verdict(&events),
            Some((StopReason::Complete, Some("finished".to_string())))
        );
    }

    #[test]
    fn unrelated_results_and_unknown_reasons_are_not_verdicts() {
        let mut other = hearth_core::Event::new(
            "s",
            hearth_core::EventType::TOOL_RESULT,
            Some(serde_json::json!({"callId": "c2", "tool": "fs_read", "ok": true, "output": "x"})),
        );
        other.seq = 1;
        let events = vec![
            other,
            // A goal_end that somehow wrote a reason nobody can name must not
            // close a goal: an unreadable verdict is not permission to guess.
            tool_result(2, serde_json::json!({"goalEnd": "probably-fine"})),
        ];
        assert_eq!(round_verdict(&events), None);
    }

    /// Two entries both fold the goal, then both try to open round 1. Only the
    /// one whose expected revision still matches the log is allowed to write; the
    /// other is told it is stale and writes nothing. This is the single-goal
    /// race the CAS fence exists to lose safely on the second entrant.
    #[test]
    fn two_entrants_at_one_revision_only_the_first_writes() {
        let path = temp_dir("cas").join("s.jsonl");
        let _ = start_goal(&path, "s", "ship P3", 5).expect("start");

        // Two independent handles on the same log, each folded before any write.
        let entrant_a = Store::open(&path, StoreOptions::default()).expect("open a");
        let entrant_b = Store::open(&path, StoreOptions::default()).expect("open b");
        let goal_a = open_goal(&path).expect("read a").expect("present");
        let goal_b = open_goal(&path).expect("read b").expect("present");
        assert_eq!(goal_a.revision, 0);
        assert_eq!(goal_b.revision, 0);

        let first = append_continued_if_current(&entrant_a, "s", &goal_a, 1).expect("cas a");
        assert_eq!(first, CasOutcome::Written(1), "the first entrant wins");

        let second = append_continued_if_current(&entrant_b, "s", &goal_b, 1).expect("cas b");
        assert_eq!(
            second,
            CasOutcome::Stale {
                expected: 0,
                found: 1
            },
            "the second sees the advance and refuses"
        );

        // Exactly one continuation is in the log: the stale entrant wrote none.
        let events = read_events(&path).expect("events");
        let continued = events
            .iter()
            .filter(|e| e.typ == hearth_core::EventType::GOAL_CONTINUED.as_str())
            .count();
        assert_eq!(continued, 1, "a stale entrant must not append");
        let folded = open_goal(&path).expect("read").expect("present");
        assert_eq!(folded.revision, 1);
        assert_eq!(folded.rounds, 1);
    }

    /// A pause arriving while a goal is running stops it: the next round must not
    /// begin, and the log says why so a resumed session does not keep going.
    #[test]
    fn a_pause_mid_run_ends_the_goal_as_cancelled() {
        let path = temp_dir("pause").join("s.jsonl");
        let goal = start_goal(&path, "s", "keep going", 4).expect("start");
        continue_goal(&path, "s", &goal, 1).expect("round 1 is running");

        assert!(
            cancel_open_goal(&path, "s", Some("paused by operator")).expect("cancel"),
            "an open goal must be ended by a pause"
        );

        let folded = open_goal(&path).expect("read").expect("present");
        assert_eq!(folded.status, Status::Cancelled);
        assert!(!folded.is_open(), "a cancelled goal asks for no round");

        let events = read_events(&path).expect("events");
        let ended: Vec<_> = events
            .iter()
            .filter(|e| e.typ == hearth_core::EventType::GOAL_ENDED.as_str())
            .collect();
        assert_eq!(ended.len(), 1, "pause writes exactly one goal/ended");
        assert_eq!(
            ended[0]
                .payload
                .as_ref()
                .and_then(|p| p.get("reason"))
                .and_then(|v| v.as_str()),
            Some("cancelled"),
            "the reason a human reads must be cancelled"
        );

        // Idempotent: a second pause arriving after the first writes nothing.
        assert!(
            !cancel_open_goal(&path, "s", None).expect("second pause"),
            "an already-ended goal must not be ended twice"
        );
    }
}
