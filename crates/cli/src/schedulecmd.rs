//! `forge schedule` — recurring prompts, kept in one JSON file.
//!
//! The rule engine is `forge-schedule`; this command is the operator's half: a
//! place to add, look at and remove tasks, a way to ask a rule what it means
//! (`forge schedule next`), and a tick (`run`) that opens the turns which are
//! due.
//!
//! Times are UTC everywhere, because the log is UTC and a schedule that silently
//! used local time would fire at a different instant after a timezone change.
//! `next` exists so a rule can be read before it is trusted: it prints the
//! instants the engine would produce, which is also how the occurrence table in
//! the tests can be checked by hand.

use std::io::Write;
use std::path::{Path, PathBuf};

use forge_schedule::{parse_rule, Due, Registry, Task};

use crate::{one_line, write_json, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};

pub const USAGE: &str = "usage: forge schedule <command> [flags]

Commands:
  add      record a recurring prompt
  list     print the tasks and when each one is next due
  remove   delete a task
  next     print the occurrences a rule produces
  run      open the turns that are due now

Flags:
  --rule <rrule>       FREQ, INTERVAL, BYDAY, BYHOUR, BYMINUTE, COUNT, UNTIL
  --prompt <text>      what to say when the task fires (add)
  --id <id>            task id (add; generated if absent)
  --target <session>   session to continue (add; a fresh one if absent)
  --start <time>       DTSTART as 2026-11-17T09:00:00Z or epoch millis
  --count <n>          how many occurrences to print (next, default 5)
  --limit <n>          how many tasks to run in one tick (run, default 5)
  --dry-run            print what run would do (run)
  --json               machine-readable output
";

const USAGE_ADD: &str = "usage: forge schedule add --rule <rrule> --prompt <text> [--id <id>] [--target <session>] [--start <time>] [--json]
";
const USAGE_LIST: &str = "usage: forge schedule list [--json]
";
const USAGE_REMOVE: &str = "usage: forge schedule remove <id> [--json]
";
const USAGE_NEXT: &str =
    "usage: forge schedule next --rule <rrule> [--start <time>] [--count <n>] [--json]
";
const USAGE_RUN: &str = "usage: forge schedule run [--limit <n>] [--dry-run] [--json]
";

#[derive(Debug, Default)]
struct Flags {
    rule: String,
    prompt: String,
    id: String,
    target: String,
    start: String,
    count: String,
    limit: String,
    dry_run: bool,
    json: bool,
}

fn parse(args: &[String], usage: &str, stderr: &mut dyn Write) -> std::result::Result<Flags, i32> {
    let mut flags = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') {
            let _ = writeln!(stderr, "forge schedule: unexpected argument {arg:?}");
            let _ = stderr.write_all(usage.as_bytes());
            return Err(EXIT_USAGE);
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name.trim_start_matches('-'), Some(value.to_string())),
            None => (arg.trim_start_matches('-'), None),
        };
        let takes_value = matches!(
            name,
            "rule" | "prompt" | "id" | "target" | "start" | "count" | "limit"
        );
        let value = if let Some(value) = inline {
            Some(value)
        } else if takes_value && i + 1 < args.len() {
            i += 1;
            Some(args[i].clone())
        } else {
            None
        };
        match name {
            "dry-run" => flags.dry_run = true,
            "json" => flags.json = true,
            "rule" | "prompt" | "id" | "target" | "start" | "count" | "limit" => {
                let value = match value {
                    Some(value) => value,
                    None => {
                        let _ = writeln!(stderr, "forge schedule: flag needs an argument: -{name}");
                        let _ = stderr.write_all(usage.as_bytes());
                        return Err(EXIT_USAGE);
                    }
                };
                match name {
                    "rule" => flags.rule = value,
                    "prompt" => flags.prompt = value,
                    "id" => flags.id = value,
                    "target" => flags.target = value,
                    "start" => flags.start = value,
                    "count" => flags.count = value,
                    "limit" => flags.limit = value,
                    _ => unreachable!(),
                }
            }
            other => {
                let _ = writeln!(stderr, "forge schedule: unknown flag -{other}");
                let _ = stderr.write_all(usage.as_bytes());
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(flags)
}

/// Milliseconds since the Unix epoch, the only clock this command knows.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// Render an instant the way the log and the contract write it.
pub fn iso(ms: i64) -> String {
    let (year, month, day, hour, minute, second) = crate::civil_from_epoch(ms.div_euclid(1000));
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Parse `2026-11-17T09:00:00Z`, `20261117T090000Z`, `2026-11-17` or epoch millis.
///
/// An empty string means "now, on the minute". The rule subset has no
/// `BYSECOND`, so the anchor's second is the second every occurrence lands on:
/// taking the current second would make `--rule FREQ=DAILY;BYHOUR=9` fire at
/// 09:00:07 today and 09:00:07 forever. An explicit `--start` keeps the second
/// it was given, because then the operator said it out loud.
fn parse_instant(text: &str) -> std::result::Result<i64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(now_ms().div_euclid(60_000) * 60_000);
    }
    if text.chars().all(|ch| ch.is_ascii_digit()) && text.len() > 8 {
        if let Ok(ms) = text.parse::<i64>() {
            return Ok(ms);
        }
    }
    let digits: String = text
        .chars()
        .filter(|ch| !matches!(ch, '-' | ':' | 'Z' | 'T' | ' '))
        .collect();
    if digits.len() != 8 && digits.len() != 14 {
        return Err(format!(
            "{text:?} is not a time: use 2026-11-17T09:00:00Z, 20261117T090000Z, 2026-11-17 or epoch millis"
        ));
    }
    let number = |from: usize, to: usize| -> std::result::Result<i64, String> {
        digits[from..to]
            .parse::<i64>()
            .map_err(|_| format!("{text:?} has a malformed component"))
    };
    let year = number(0, 4)?;
    let month = number(4, 6)? as u32;
    let day = number(6, 8)? as u32;
    let (hour, minute, second) = if digits.len() == 14 {
        (
            number(8, 10)? as u32,
            number(10, 12)? as u32,
            number(12, 14)? as u32,
        )
    } else {
        (0, 0, 0)
    };
    if !(1..=12).contains(&month) || day < 1 || hour > 23 || minute > 59 || second > 59 {
        return Err(format!("{text:?} is not a real time"));
    }
    Ok(forge_schedule::from_civil(
        year, month, day, hour, minute, second,
    ))
}

fn data_dir(stderr: &mut dyn Write, command: &str) -> std::result::Result<PathBuf, i32> {
    match crate::default_data_dir() {
        Ok(dir) => Ok(dir),
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge schedule {command}: read config: {}",
                one_line(err.message())
            );
            Err(EXIT_FAILURE)
        }
    }
}

fn registry(dir: &Path) -> Registry {
    Registry::open(dir.join("schedule"))
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
        "add" => add(rest, stdout, stderr),
        "list" => list(rest, stdout, stderr),
        "remove" => remove(rest, stdout, stderr),
        "next" => next(rest, stdout, stderr),
        "run" => run_due(rest, stdout, stderr),
        other => {
            let _ = writeln!(stderr, "forge schedule: unknown command {other:?}");
            let _ = stderr.write_all(USAGE.as_bytes());
            EXIT_USAGE
        }
    }
}

fn add(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_ADD.as_bytes());
        return EXIT_OK;
    }
    let flags = match parse(args, USAGE_ADD, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    if flags.rule.trim().is_empty() || flags.prompt.trim().is_empty() {
        let _ = writeln!(
            stderr,
            "forge schedule add: --rule and --prompt are both required"
        );
        let _ = stderr.write_all(USAGE_ADD.as_bytes());
        return EXIT_USAGE;
    }
    // Parse the rule before touching the file: a stored rule that cannot fire is
    // a task that will be quietly skipped on every tick.
    if let Err(err) = parse_rule(&flags.rule) {
        let _ = writeln!(stderr, "forge schedule add: {}", one_line(err.message()));
        return EXIT_USAGE;
    }
    let start_ms = match parse_instant(&flags.start) {
        Ok(ms) => ms,
        Err(message) => {
            let _ = writeln!(stderr, "forge schedule add: {message}");
            return EXIT_USAGE;
        }
    };
    let dir = match data_dir(stderr, "add") {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let registry = registry(&dir);

    let mut task = Task::new(&flags.rule, flags.prompt.trim(), start_ms, now_ms());
    if !flags.id.trim().is_empty() {
        task.id = flags.id.trim().to_string();
    }
    if !flags.target.trim().is_empty() {
        task.target = Some(flags.target.trim().to_string());
    }
    if let Err(err) = registry.add(task.clone()) {
        let _ = writeln!(stderr, "forge schedule add: {}", one_line(err.message()));
        return EXIT_FAILURE;
    }

    let upcoming = occurrences_hint(&task);
    if flags.json {
        return write_json(
            stdout,
            stderr,
            "schedule add",
            &serde_json::json!({
                "id": task.id,
                "rule": task.rule,
                "prompt": task.prompt,
                "target": task.target,
                "next": upcoming,
                "file": registry.path().display().to_string(),
            }),
        );
    }
    let _ = writeln!(stdout, "{}", task.id);
    for at in &upcoming {
        let _ = writeln!(stderr, "forge schedule: next {at}");
    }
    EXIT_OK
}

/// The next few instants a task would fire at, as printable strings.
fn occurrences_hint(task: &Task) -> Vec<String> {
    let Ok(rule) = parse_rule(&task.rule) else {
        return Vec::new();
    };
    rule.occurrences(task.start_ms, now_ms(), 3)
        .into_iter()
        .map(iso)
        .collect()
}

fn list(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_LIST.as_bytes());
        return EXIT_OK;
    }
    let flags = match parse(args, USAGE_LIST, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    let dir = match data_dir(stderr, "list") {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let registry = registry(&dir);
    let tasks = match registry.load() {
        Ok(tasks) => tasks,
        Err(err) => {
            let _ = writeln!(stderr, "forge schedule list: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };
    let now = now_ms();

    if flags.json {
        let rows: Vec<_> = tasks
            .iter()
            .map(|task| {
                let next = task.next_after_ms().map(iso);
                serde_json::json!({
                    "id": task.id,
                    "rule": task.rule,
                    "prompt": task.prompt,
                    "target": task.target,
                    "runs": task.runs,
                    "lastRun": task.last_run_ms.map(iso),
                    "next": next,
                    "due": task.due_at(now).is_some(),
                })
            })
            .collect();
        return write_json(
            stdout,
            stderr,
            "schedule list",
            &serde_json::json!({
                "file": registry.path().display().to_string(),
                "tasks": rows,
            }),
        );
    }

    if tasks.is_empty() {
        let _ = writeln!(
            stdout,
            "no tasks in {} (add one with `forge schedule add`)",
            registry.path().display()
        );
        return EXIT_OK;
    }
    // A fixed-width table keeps the operator's eye on the times, which is the
    // only column that decides whether the schedule does what they meant.
    let _ = writeln!(
        stdout,
        "{:<24} {:<20} {:>5}  rule",
        "id", "next (UTC)", "runs"
    );
    for task in &tasks {
        let next = task
            .next_after_ms()
            .map(iso)
            .unwrap_or_else(|| "never".to_string());
        let marker = if task.due_at(now).is_some() { "*" } else { " " };
        let _ = writeln!(
            stdout,
            "{marker}{:<23} {:<20} {:>5}  {}",
            task.id, next, task.runs, task.rule
        );
    }
    EXIT_OK
}

fn remove(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_REMOVE.as_bytes());
        return EXIT_OK;
    }
    let Some(id) = args.iter().find(|arg| !arg.starts_with('-')).cloned() else {
        let _ = writeln!(stderr, "forge schedule remove: which task? pass an id");
        let _ = stderr.write_all(USAGE_REMOVE.as_bytes());
        return EXIT_USAGE;
    };
    let flags = match parse(args, USAGE_REMOVE, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    let dir = match data_dir(stderr, "remove") {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let registry = registry(&dir);
    if let Err(err) = registry.remove(&id) {
        let _ = writeln!(stderr, "forge schedule remove: {}", one_line(err.message()));
        return EXIT_FAILURE;
    }
    if flags.json {
        return write_json(
            stdout,
            stderr,
            "schedule remove",
            &serde_json::json!({"removed": id}),
        );
    }
    let _ = writeln!(stdout, "removed {id}");
    EXIT_OK
}

fn next(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_NEXT.as_bytes());
        return EXIT_OK;
    }
    let flags = match parse(args, USAGE_NEXT, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    if flags.rule.trim().is_empty() {
        let _ = writeln!(stderr, "forge schedule next: --rule is required");
        let _ = stderr.write_all(USAGE_NEXT.as_bytes());
        return EXIT_USAGE;
    }
    let rule = match parse_rule(&flags.rule) {
        Ok(rule) => rule,
        Err(err) => {
            let _ = writeln!(stderr, "forge schedule next: {}", one_line(err.message()));
            return EXIT_USAGE;
        }
    };
    let start_ms = match parse_instant(&flags.start) {
        Ok(ms) => ms,
        Err(message) => {
            let _ = writeln!(stderr, "forge schedule next: {message}");
            return EXIT_USAGE;
        }
    };
    let count = match flags.count.trim().parse::<usize>() {
        Ok(0) => {
            let _ = writeln!(stderr, "forge schedule next: --count must be at least 1");
            return EXIT_USAGE;
        }
        Ok(count) => count.min(50),
        Err(_) if flags.count.trim().is_empty() => 5,
        Err(_) => {
            let _ = writeln!(
                stderr,
                "forge schedule next: --count {:?} is not a number",
                flags.count
            );
            return EXIT_USAGE;
        }
    };

    let after = now_ms().max(start_ms);
    let instants: Vec<String> = rule
        .occurrences(start_ms, after, count)
        .into_iter()
        .map(iso)
        .collect();

    if flags.json {
        return write_json(
            stdout,
            stderr,
            "schedule next",
            &serde_json::json!({
                "rule": flags.rule.trim(),
                "start": iso(start_ms),
                "after": iso(after),
                "occurrences": instants,
            }),
        );
    }
    if instants.is_empty() {
        let _ = writeln!(
            stdout,
            "this rule produces no occurrence after {after}",
            after = iso(after)
        );
        return EXIT_OK;
    }
    for instant in &instants {
        let _ = writeln!(stdout, "{instant}");
    }
    EXIT_OK
}

fn run_due(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE_RUN.as_bytes());
        return EXIT_OK;
    }
    let flags = match parse(args, USAGE_RUN, stderr) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    let limit = match flags.limit.trim().parse::<usize>() {
        Ok(0) => {
            let _ = writeln!(stderr, "forge schedule run: --limit must be at least 1");
            return EXIT_USAGE;
        }
        Ok(limit) => limit,
        Err(_) if flags.limit.trim().is_empty() => 5,
        Err(_) => {
            let _ = writeln!(
                stderr,
                "forge schedule run: --limit {:?} is not a number",
                flags.limit
            );
            return EXIT_USAGE;
        }
    };
    let dir = match data_dir(stderr, "run") {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let registry = registry(&dir);
    let due: Vec<Due> = match registry.due(now_ms()) {
        Ok(due) => due,
        Err(err) => {
            let _ = writeln!(stderr, "forge schedule run: {}", one_line(err.message()));
            return EXIT_FAILURE;
        }
    };

    if flags.dry_run {
        if flags.json {
            let rows: Vec<_> = due
                .iter()
                .take(limit)
                .map(|due| {
                    serde_json::json!({
                        "id": due.task.id,
                        "at": iso(due.at_ms),
                        "prompt": due.task.prompt,
                        "target": due.task.target,
                    })
                })
                .collect();
            return write_json(
                stdout,
                stderr,
                "schedule run",
                &serde_json::json!({"dryRun": true, "due": rows}),
            );
        }
        if due.is_empty() {
            let _ = writeln!(stdout, "nothing is due");
            return EXIT_OK;
        }
        for due in due.iter().take(limit) {
            let _ = writeln!(
                stdout,
                "{} at {} would run: {}",
                due.task.id,
                iso(due.at_ms),
                one_line(&due.task.prompt)
            );
        }
        return EXIT_OK;
    }

    if due.is_empty() {
        if flags.json {
            return write_json(
                stdout,
                stderr,
                "schedule run",
                &serde_json::json!({"ran": []}),
            );
        }
        let _ = writeln!(stdout, "nothing is due");
        return EXIT_OK;
    }

    let mut ran = Vec::new();
    let mut failed = false;
    for due in due.iter().take(limit) {
        let session = due
            .task
            .target
            .clone()
            .unwrap_or_else(|| crate::new_session_id().unwrap_or_default());
        if session.is_empty() {
            let _ = writeln!(
                stderr,
                "forge schedule run: could not allocate a session id"
            );
            return EXIT_FAILURE;
        }
        let _ = writeln!(
            stderr,
            "forge schedule: {} at {} into session {}",
            due.task.id,
            iso(due.at_ms),
            session
        );
        // A tick has nobody to ask, so it approves nothing: `--approve never` is
        // the same stance the web UI takes without a human attached.
        let mut turn_stdout: Vec<u8> = Vec::new();
        let code = crate::chat::run(
            &[
                "--session".to_string(),
                session.clone(),
                "--approve".to_string(),
                "never".to_string(),
                due.task.prompt.clone(),
            ],
            &mut turn_stdout,
            stderr,
        );
        // The turn's answer belongs to the session log; the tick's job is to
        // report which task ran and whether it worked.
        let blocked = code != EXIT_OK;
        failed |= blocked;
        let note = if blocked {
            Some(format!("forge schedule: {} exited {code}", due.task.id))
        } else {
            None
        };
        // The run is recorded either way: a task that failed at 09:00 must not be
        // retried on every tick for the rest of the day. The note says it failed.
        if let Err(err) = registry.mark_run(&due.task.id, due.at_ms) {
            let _ = writeln!(
                stderr,
                "forge schedule run: could not record {}: {}",
                due.task.id,
                one_line(err.message())
            );
        }
        ran.push(serde_json::json!({
            "id": due.task.id,
            "at": iso(due.at_ms),
            "session": session,
            "exit": code,
            "note": note,
        }));
    }

    if flags.json {
        return write_json(
            stdout,
            stderr,
            "schedule run",
            &serde_json::json!({"ran": ran}),
        );
    }
    let _ = writeln!(stdout, "ran {} task(s)", ran.len());
    if failed {
        return EXIT_FAILURE;
    }
    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instants_round_trip_through_the_text_form() {
        let text = "2026-11-17T09:00:00Z";
        let ms = parse_instant(text).expect("parses");
        assert_eq!(ms, 1_794_906_000_000);
        assert_eq!(iso(ms), text);
        // The compact form the RRULE UNTIL uses must parse to the same instant.
        assert_eq!(parse_instant("20261117T090000Z").expect("parses"), ms);
        assert_eq!(parse_instant("1794906000000").expect("parses"), ms);
    }

    #[test]
    fn a_date_only_start_is_midnight_utc() {
        assert_eq!(
            parse_instant("2026-11-17").expect("parses"),
            1_794_873_600_000
        );
    }

    #[test]
    fn a_time_that_is_not_one_is_refused_with_the_forms_that_work() {
        // A bare number is epoch millis by design, so it is not in this list: the
        // cases here are texts that cannot be read as any accepted form.
        for text in [
            "soon",
            "2026-13-01",
            "2026-11-17T25:00:00Z",
            "2026-11-17T09:00:00+08:00",
        ] {
            let error = parse_instant(text).expect_err(text);
            assert!(
                error.contains("2026-11-17T09:00:00Z") || error.contains("not a real time"),
                "{text}: {error}"
            );
        }
    }

    #[test]
    fn an_empty_start_means_now_on_the_minute() {
        let ms = parse_instant("").expect("parses");
        assert!((ms - now_ms()).abs() < 65_000, "{ms}");
        assert_eq!(ms % 60_000, 0, "{ms} is not on a minute");
    }

    #[test]
    fn the_usage_lists_every_command() {
        for command in ["add", "list", "remove", "next", "run"] {
            assert!(USAGE.contains(command), "{command} missing from usage");
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir()
            .join("forge-schedule-cmd-test")
            .join(format!("{}-{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create dir");
        path
    }

    #[test]
    fn a_task_survives_a_fresh_registry_and_reports_its_next_instant() {
        let dir = temp_dir("storage");
        let registry = self::registry(&dir);
        registry
            .add(Task::new("FREQ=DAILY;BYHOUR=9", "say hi", 0, 0))
            .expect("add");
        let reopened = self::registry(&dir);
        let tasks = reopened.load().expect("load");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].rule, "FREQ=DAILY;BYHOUR=9");
        assert_eq!(tasks[0].prompt, "say hi");
        let shown = occurrences_hint(&tasks[0]);
        assert_eq!(shown.len(), 3);
        assert!(shown[0].ends_with("T09:00:00Z"), "{}", shown[0]);
    }
}
