//! Board state for a `hearth team run`, read off the disk for the browser.
//!
//! A run leaves a directory `<dataDir>/teams/<slug>-<YYYYMMDD-HHMMSS>/` holding
//! `board.json` and `workers/<name>.jsonl`. `hearth-team` owns that format and
//! this crate may not depend on it (see `crates/server/README.md`), so the
//! reader here parses the versioned JSON with `serde_json::Value` instead of
//! sharing the structs. Two rules follow from that and are the whole point of
//! this module:
//!
//! * It only reads. Nothing here creates a directory, starts a worker, repairs
//!   a file or rewrites a board -- a route that opened `Board::open` would
//!   create `<dataDir>/teams` on a machine that has never run a team.
//! * One junk board is a diagnostic on that board, never a failure of the
//!   route: an operator who has ten boards and one truncated `board.json` must
//!   still see the other nine.

use std::fs;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::Value;

/// Directory under the data dir that `hearth team run` writes into.
const TEAMS_DIR: &str = "teams";
/// The board inside a run directory (`crates/team/src/board.rs`).
const BOARD_FILE: &str = "board.json";
/// Worker transcripts inside a run directory (`crates/team/src/run.rs`).
const WORKERS_DIR: &str = "workers";
/// The only board format this build understands. A newer version is reported,
/// not guessed at: unknown versions are errors, unknown data is not.
const BOARD_VERSION: u64 = 1;
/// A board name is one path segment, so it may not be longer than a session id.
const MAX_NAME: usize = 128;
/// A board that is bigger than this is not read; the size is reported instead.
const MAX_BOARD_BYTES: u64 = 4 << 20;
/// Same for one worker transcript.
const MAX_TRANSCRIPT_BYTES: u64 = 8 << 20;
/// How much of an error message a diagnostic keeps.
const MAX_DIAGNOSTIC: usize = 240;

/// The teams directory could not be read at all. A board that is merely junk is
/// a diagnostic on that board, never this.
#[derive(Debug)]
pub struct Unreadable(pub String);

/// `GET /api/teams`.
#[derive(Debug, Default, Serialize)]
pub struct Boards {
    boards: Vec<Board>,
}

/// One run directory, as the browser sees it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    /// The directory name: unique, and the key a client uses to ask again.
    board: String,
    /// The slug without the timestamp. Both live in the directory name because
    /// nothing writes them into `board.json`.
    team: String,
    /// How many workers may run at once. `hearth team run` chooses this from
    /// `--max-parallel`, `team.max_parallel` or the default, and persists it
    /// nowhere, so this is `null` until something records it.
    parallel: Option<usize>,
    /// When the run started: the stamp in the directory name, rendered as RFC
    /// 3339 in UTC. A directory without a stamp reports its mtime instead.
    created_at: String,
    tasks: Vec<Task>,
    workers: Vec<Worker>,
    /// Present only when something was wrong. A healthy board has no key here,
    /// so a client can treat its absence as "nothing to warn about".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<String>,
}

/// A row of `board.json`'s `tasks`, reduced to what a board view shows.
#[derive(Debug, Serialize)]
pub struct Task {
    id: String,
    subject: String,
    /// The exact string `board.json` carries (`pending`, `in_progress`,
    /// `completed`); an unknown value is passed through with a diagnostic
    /// rather than dropped, because unknown data is still data.
    status: String,
    owner: Option<String>,
}

/// One `workers/<name>.jsonl` transcript.
#[derive(Debug, Serialize)]
pub struct Worker {
    name: String,
    /// From the transcript's `worker/end` line.
    ///
    /// `null` means the transcript has no end line: the worker was still
    /// running, or was killed, when the board was read. That is *not* the same
    /// claim as `false` — a worker that has not finished has not failed, and
    /// reporting it as failed would make every board read mid-run look worse
    /// than it is. The diagnostic beside it says which case it was.
    ok: Option<bool>,
    /// The end line's `note`, already clipped by the team crate to 400 bytes.
    note: String,
    /// How many JSON lines the transcript holds: one `worker/start`, plus the
    /// `worker/end` if the worker finished.
    events: usize,
}

/// Every board under `<data_dir>/teams/`, newest first.
///
/// A machine that has never run a team has no such directory, and that is an
/// empty list rather than an error -- and rather than a directory this module
/// would have to create.
pub fn list(data_dir: &Path) -> Result<Boards, Unreadable> {
    let root = data_dir.join(TEAMS_DIR);
    // `is_dir` follows symlinks; `read_dir` on a file or a missing path would
    // report a misleading error, and "no boards" is the honest answer to both.
    if !root.is_dir() {
        return Ok(Boards::default());
    }
    let entries =
        fs::read_dir(&root).map_err(|error| Unreadable(format!("{}: {error}", root.display())))?;
    let mut found: Vec<(i64, Board)> = Vec::new();
    for entry in entries {
        // One unreadable entry must not cost the caller the other nine.
        let Ok(entry) = entry else { continue };
        // `file_type` does not follow symlinks, so a link to somewhere else is
        // not a board -- the same reason a board name may not be a path.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_board_name(&name) {
            continue;
        }
        let path = entry.path();
        let mut diagnostics = Vec::new();
        let seconds = created_seconds(&name, &path, &mut diagnostics);
        found.push((seconds, read_board(name, &path, seconds, diagnostics)));
    }
    // Newest first; the name breaks ties so two runs in the same second keep a
    // stable order instead of whatever the filesystem handed us.
    found.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.board.cmp(&left.1.board))
    });
    Ok(Boards {
        boards: found.into_iter().map(|(_, board)| board).collect(),
    })
}

/// One board, or `None` when there is no such directory.
///
/// The name is checked as a single path segment before any path is built, so a
/// caller cannot turn `{board}` into a path to something else.
pub fn one(data_dir: &Path, name: &str) -> Result<Option<Board>, Unreadable> {
    if !valid_board_name(name) {
        return Ok(None);
    }
    let path = data_dir.join(TEAMS_DIR).join(name);
    // Not following symlinks, for the same reason `list` does not.
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(None);
    };
    if !metadata.is_dir() {
        return Ok(None);
    }
    let mut diagnostics = Vec::new();
    let seconds = created_seconds(name, &path, &mut diagnostics);
    Ok(Some(read_board(
        name.to_string(),
        &path,
        seconds,
        diagnostics,
    )))
}

/// A board name is one plain directory name: not `.`, not `..`, no separator,
/// and no leading dot. This is the rule `valid_session_id` applies to session
/// ids and `Blobs::get` to attachment ids, read for a name that is a directory.
pub fn valid_board_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME {
        return false;
    }
    // `.` and `..` are the two names that are not a segment of anything; a
    // leading dot also hides the directory from a plain listing.
    if name.starts_with('.') {
        return false;
    }
    // Both separators are refused on every platform, not just the current one:
    // a board name travels in a URL, which has no idea which OS will read it.
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    // Windows drive-relative names (`C:x`) and alternate data streams (`x:y`)
    // are paths, not names.
    if name.contains(':') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
}

fn read_board(name: String, path: &Path, seconds: i64, mut diagnostics: Vec<String>) -> Board {
    let team = match split_stamp(&name) {
        Some((team, _)) => team.to_string(),
        None => name.clone(),
    };
    let tasks = read_tasks(path, &mut diagnostics);
    let workers = read_workers(path, &mut diagnostics);
    Board {
        board: name,
        team,
        // Nothing persists this today; see the field's doc comment.
        parallel: None,
        created_at: format_stamp(seconds),
        tasks,
        workers,
        diagnostics,
    }
}

/// `board.json`, reduced to its tasks.
fn read_tasks(dir: &Path, diagnostics: &mut Vec<String>) -> Vec<Task> {
    let path = dir.join(BOARD_FILE);
    if !path.is_file() {
        // A run that was interrupted before its first write leaves this: the
        // directory exists, the board does not.
        diagnostics.push(format!(
            "{BOARD_FILE} is missing, so this board reports no tasks"
        ));
        return Vec::new();
    }
    let text = match read_capped(&path, MAX_BOARD_BYTES, diagnostics) {
        Some(text) => text,
        None => return Vec::new(),
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(format!(
                "{BOARD_FILE} could not be parsed: {}",
                short(&error.to_string())
            ));
            return Vec::new();
        }
    };
    match value.get("version").and_then(Value::as_u64) {
        Some(BOARD_VERSION) => {}
        Some(other) => {
            diagnostics.push(format!(
                "{BOARD_FILE} is v{other}; this build reads v{BOARD_VERSION}, so its tasks are not shown"
            ));
            return Vec::new();
        }
        None => {
            diagnostics.push(format!(
                "{BOARD_FILE} has no version, so its tasks are not shown"
            ));
            return Vec::new();
        }
    }
    let Some(rows) = value.get("tasks").and_then(Value::as_array) else {
        diagnostics.push(format!("{BOARD_FILE} has no tasks array"));
        return Vec::new();
    };
    let mut tasks = Vec::with_capacity(rows.len());
    for row in rows {
        // `id` is the identity a client addresses a task by: without one a row
        // is not addressable, which is worse than not being listed.
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            diagnostics.push(format!("{BOARD_FILE} has a task with no id, skipped"));
            continue;
        };
        let subject = match row.get("subject").and_then(Value::as_str) {
            Some(subject) => subject.to_string(),
            None => {
                diagnostics.push(format!("task {id} has no subject"));
                String::new()
            }
        };
        let status = match row.get("status").and_then(Value::as_str) {
            Some(status) => {
                if !matches!(status, "pending" | "in_progress" | "completed") {
                    diagnostics.push(format!(
                        "task {id} has status {status:?}, which this build does not know"
                    ));
                }
                status.to_string()
            }
            None => {
                diagnostics.push(format!("task {id} has no status"));
                String::new()
            }
        };
        let owner = match row.get("owner") {
            Some(Value::String(owner)) => Some(owner.clone()),
            Some(Value::Null) | None => None,
            Some(other) => {
                diagnostics.push(format!(
                    "task {id} has a non-string owner: {}",
                    short(&other.to_string())
                ));
                None
            }
        };
        tasks.push(Task {
            id: id.to_string(),
            subject,
            status,
            owner,
        });
    }
    tasks
}

/// `workers/<name>.jsonl`, one line per event.
fn read_workers(dir: &Path, diagnostics: &mut Vec<String>) -> Vec<Worker> {
    let root = dir.join(WORKERS_DIR);
    if !root.is_dir() {
        diagnostics.push(format!(
            "{WORKERS_DIR}/ is missing, so this board reports no workers"
        ));
        return Vec::new();
    }
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(format!("{WORKERS_DIR}/ could not be read: {error}"));
            return Vec::new();
        }
    };
    let mut workers = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".jsonl") else {
            continue;
        };
        if let Some(worker) = read_worker(name, &path, diagnostics) {
            workers.push(worker);
        }
    }
    workers.sort_by(|left, right| left.name.cmp(&right.name));
    if workers.is_empty() {
        diagnostics.push(format!("{WORKERS_DIR}/ holds no *.jsonl transcript yet"));
    }
    workers
}

fn read_worker(name: &str, path: &Path, diagnostics: &mut Vec<String>) -> Option<Worker> {
    let label = format!("{WORKERS_DIR}/{name}.jsonl");
    let text = read_capped(path, MAX_TRANSCRIPT_BYTES, diagnostics)?;
    let mut events = 0usize;
    let mut unreadable = 0usize;
    let mut end: Option<(Option<bool>, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            unreadable += 1;
            continue;
        };
        events += 1;
        if value.get("kind").and_then(Value::as_str) == Some("worker/end") {
            let ok = value.get("ok").and_then(Value::as_bool);
            if ok.is_none() {
                diagnostics.push(format!("{label} has a worker/end line with no ok flag"));
            }
            let note = value
                .get("note")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // Last one wins: a transcript is written once per worker, and a
            // second end line means the last thing written is the truth.
            end = Some((ok, note));
        }
    }
    if unreadable > 0 {
        diagnostics.push(format!(
            "{label} has {unreadable} line(s) that are not JSON, not counted as events"
        ));
    }
    let (ok, note) = match end {
        Some((ok, note)) => (ok, note),
        None => {
            diagnostics.push(format!(
                "{label} has no worker/end line: the worker had not finished when this board was read"
            ));
            (None, String::new())
        }
    };
    Some(Worker {
        name: name.to_string(),
        ok,
        note,
        events,
    })
}

/// Reads a file only if it is small enough to be one, reporting why not.
fn read_capped(path: &Path, cap: u64, diagnostics: &mut Vec<String>) -> Option<String> {
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > cap => {
            diagnostics.push(format!(
                "{label} is {} bytes, larger than the {cap} bytes this route reads",
                metadata.len()
            ));
            None
        }
        Ok(_) => match fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) => {
                diagnostics.push(format!("{label} could not be read: {error}"));
                None
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            diagnostics.push(format!("{label} disappeared while it was being read"));
            None
        }
        Err(error) => {
            diagnostics.push(format!("{label} could not be inspected: {error}"));
            None
        }
    }
}

/// When the run started: the trailing `-YYYYMMDD-HHMMSS` of the directory name,
/// falling back to the directory's own mtime for a directory named otherwise
/// (the `TeamTool` bridge names one after its call id).
fn created_seconds(name: &str, path: &Path, diagnostics: &mut Vec<String>) -> i64 {
    if let Some((_, seconds)) = split_stamp(name) {
        return seconds;
    }
    match fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
    {
        Some(seconds) => {
            diagnostics.push(
                "the directory name carries no -YYYYMMDD-HHMMSS stamp, so createdAt is its modification time"
                    .to_string(),
            );
            seconds
        }
        None => {
            diagnostics.push(
                "the directory name carries no stamp and its modification time is unreadable, so createdAt is the Unix epoch"
                    .to_string(),
            );
            0
        }
    }
}

/// Splits `<slug>-<YYYYMMDD-HHMMSS>` into the slug and the instant it stamps.
fn split_stamp(name: &str) -> Option<(&str, i64)> {
    // "-YYYYMMDD-HHMMSS" is sixteen bytes.
    const SUFFIX: usize = 16;
    if name.len() <= SUFFIX {
        return None;
    }
    let (slug, stamp) = name.split_at(name.len() - SUFFIX);
    let bytes = stamp.as_bytes();
    if bytes[0] != b'-' || bytes[9] != b'-' {
        return None;
    }
    let digits = |slice: &[u8]| -> Option<u32> {
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let year = digits(&bytes[1..5])? as i64;
    let month = digits(&bytes[5..7])?;
    let day = digits(&bytes[7..9])?;
    let hour = digits(&bytes[10..12])?;
    let minute = digits(&bytes[12..14])?;
    let second = digits(&bytes[14..16])?;
    if slug.is_empty() || slug.ends_with('/') || slug.ends_with('\\') {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400
        + hour as i64 * 3600
        + minute as i64 * 60
        + second as i64;
    // The stamp has to survive a round trip through the calendar: a name like
    // `-20260231-000000` is a typo, not a date, and falls back to the mtime
    // rather than being normalised into a different day.
    if format_stamp(seconds)
        != format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    {
        return None;
    }
    Some((slug, seconds))
}

/// `YYYY-MM-DDTHH:MM:SSZ`, always UTC: the board is stamped in UTC and the
/// browser can localise a `Z` time but not a naive one.
fn format_stamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Howard Hinnant's civil-from-days: the one piece of calendar arithmetic this
/// crate needs, and the reason it does not take a date library.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// The inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// One line of somebody else's error message, because a diagnostic is rendered
/// in a browser row and not in a log file.
fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() > MAX_DIAGNOSTIC {
        let mut clipped: String = line.chars().take(MAX_DIAGNOSTIC).collect();
        clipped.push_str("...");
        clipped
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_board_name_is_one_plain_segment() {
        for name in ["release-squad-20261118-002223", "s_abc-Call1", "a+b.c"] {
            assert!(valid_board_name(name), "{name} should be a board name");
        }
        for name in [
            "",
            ".",
            "..",
            "../token",
            "../../token",
            "a/b",
            "a\\b",
            "C:/token",
            "ads:stream",
            ".hidden",
            &"a".repeat(MAX_NAME + 1),
        ] {
            assert!(!valid_board_name(name), "{name:?} must not be a board name");
        }
    }

    #[test]
    fn a_stamped_directory_name_names_the_team_and_the_instant() {
        let (team, seconds) = split_stamp("release-squad-20261118-002223").expect("stamped");
        assert_eq!(team, "release-squad");
        assert_eq!(format_stamp(seconds), "2026-11-18T00:22:23Z");
        // A name that is a typo is not a stamp; the caller falls back to mtime.
        assert!(split_stamp("release-squad-20260231-000000").is_none());
        assert!(split_stamp("release-squad").is_none());
        assert!(split_stamp("-20261118-002223").is_none());
    }

    #[test]
    fn calendar_conversions_are_inverses() {
        for days in [-719_468_i64, -1, 0, 1, 10_957, 20_762, 100_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days, "day {days}");
        }
        assert_eq!(format_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_stamp(1_000_000_000), "2001-09-09T01:46:40Z");
    }
}
