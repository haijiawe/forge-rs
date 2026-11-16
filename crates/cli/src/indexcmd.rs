//! `forge index`: build, inspect and query the SQLite session index.
//!
//! The index (`index.db`, sitting beside `sessions/`) is derived data: every row
//! is a projection of `<dataDir>/sessions/*.jsonl`, so deleting it costs one
//! rebuild and loses nothing. Only `--rebuild` writes; every other invocation
//! opens an existing index read-only and reports what it says. Opening is what a
//! missing index must not trigger, so the file is stat'ed first: a command that
//! silently created an empty index would answer "0 sessions" to a question about
//! a directory full of logs.

use std::io::Write;
use std::path::{Path, PathBuf};

use forge_config::Config;
use forge_core::{Error, ErrorKind};
use forge_store::{Hit, Query, Session, Stats, Store};

use crate::{one_line, reorder_flags, write_json};

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

/// An invocation without `--rebuild` reports the existing index and writes
/// nothing.
const MODE_READ_ONLY: &str = "read-only";
/// The mode after `--rebuild` dropped and re-derived the whole index.
const MODE_REBUILT: &str = "rebuilt";

/// How many sessions the default summary lists: one screenful, the same first
/// page the HTTP list API returns by default.
const SESSION_PAGE: i32 = 20;
/// `--search` result bound (the store defaults to 50 and caps at 200).
const SEARCH_LIMIT: i32 = 50;

pub const USAGE: &str = "usage: forge index [flags]

Read or rebuild the SQLite session index (index.db) that sits next to the session
logs. The index is derived data: deleting it loses nothing, and --rebuild
re-derives it from <dataDir>/sessions/*.jsonl. Nothing is written unless
--rebuild is passed, and an index that does not exist is an error rather than an
empty one silently created.

flags:
  --rebuild         drop the index and rebuild it from the session logs
  --stats           print the index totals and nothing else
  --search <text>   full-text search the indexed messages
  --json            machine-readable output
  --db <path>       index file (default: <dataDir>/index.db; the data directory
                    is the index file's parent, so --db also selects the logs)
  -h, --help        this message

stdout: the summary (default), the totals (--stats), or one line per hit
(--search). An empty --search query is not an error: it matches nothing and
prints nothing. Diagnostics go to stderr.

exit codes: 0 ok, 1 failure, 2 usage
";

/// The flags that consume the next argument, so a flag written after a value is
/// never mistaken for one. `forge index` takes no positionals, but a caller can
/// still write `--db DIR --search x`, and the same reordering applies.
const VALUE_FLAGS: &[&str] = &["search", "db"];

#[derive(Debug, Default)]
struct Options {
    rebuild: bool,
    stats: bool,
    search: String,
    /// Distinguishes an explicit `--search ""` (a valid, empty query) from no
    /// `--search` at all.
    search_set: bool,
    json: bool,
    db: String,
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

    let (db_path, data_dir) = match resolve_paths(&opt.db) {
        Ok(pair) => pair,
        Err(err) => {
            let _ = writeln!(stderr, "forge index: {}", one_line(&err.to_string()));
            return EXIT_FAILURE;
        }
    };

    // Stat before open: opening creates the file, so opening first would turn
    // "there is no index" into "here is an empty index" and silently answer zero
    // sessions for a directory that may hold a thousand logs.
    if !opt.rebuild && !db_path.exists() {
        let _ = writeln!(
            stderr,
            "forge index: no index at {}; run 'forge index --rebuild' to build it",
            db_path.display()
        );
        return EXIT_FAILURE;
    }

    let store = match Store::open(forge_store::Options {
        path: db_path.clone(),
        now: None,
    }) {
        Ok(store) => store,
        Err(err) => {
            let _ = writeln!(stderr, "forge index: {}", one_line(&err.to_string()));
            return EXIT_FAILURE;
        }
    };

    let mode = if opt.rebuild {
        if let Err(err) = store.rebuild(&data_dir) {
            let _ = writeln!(
                stderr,
                "forge index: rebuild {}: {}",
                db_path.display(),
                one_line(&err.to_string())
            );
            return EXIT_FAILURE;
        }
        MODE_REBUILT
    } else {
        MODE_READ_ONLY
    };

    let stats = match store.stats() {
        Ok(stats) => stats,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge index: stats {}: {}",
                db_path.display(),
                one_line(&err.to_string())
            );
            return EXIT_FAILURE;
        }
    };

    // `--search` wins over `--stats` (they are rejected together at parse time),
    // and both are honoured after `--rebuild` so one invocation can rebuild and
    // ask.
    if opt.search_set {
        let query = Query {
            limit: SEARCH_LIMIT,
            ..Query::default()
        };
        let hits = match store.search(&opt.search, query) {
            Ok(hits) => hits,
            Err(err) => {
                let _ = writeln!(
                    stderr,
                    "forge index: search {}: {}",
                    db_path.display(),
                    one_line(&err.to_string())
                );
                return EXIT_FAILURE;
            }
        };
        if opt.json {
            let docs: Vec<HitDoc> = hits.iter().map(HitDoc::of).collect();
            return write_json(stdout, stderr, "index", &docs);
        }
        for hit in &hits {
            let _ = writeln!(
                stdout,
                "{}:{}  [{}]  {}",
                hit.session_id,
                hit.seq,
                hit.event_type,
                one_line(&hit.snippet)
            );
        }
        return EXIT_OK;
    }

    if opt.stats {
        if opt.json {
            return write_json(stdout, stderr, "index", &StatsDoc::of(&stats));
        }
        write_totals(stdout, &stats);
        return EXIT_OK;
    }

    let rows = match store.list(Query {
        limit: SESSION_PAGE,
        ..Query::default()
    }) {
        Ok((rows, _next)) => rows,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge index: list {}: {}",
                db_path.display(),
                one_line(&err.to_string())
            );
            return EXIT_FAILURE;
        }
    };

    if opt.json {
        let doc = SummaryDoc {
            db: db_path.display().to_string(),
            mode: mode.to_string(),
            stats: StatsDoc::of(&stats),
            sessions: rows.iter().map(SessionDoc::of).collect(),
        };
        return write_json(stdout, stderr, "index", &doc);
    }

    write_summary(stdout, &db_path, mode, &stats, &rows);
    EXIT_OK
}

/// Validates every flag before the store is opened, so a usage mistake never
/// creates or touches an index file.
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
            "-h" | "--help" => {
                // Handled by the caller; reaching here means it came after `--`.
                rest.push(arg);
            }
            "--rebuild" => opt.rebuild = true,
            "--stats" => opt.stats = true,
            "--json" => opt.json = true,
            "--search" | "--db" => {
                let value = match inline {
                    Some(value) => value,
                    None => match ordered.get(i) {
                        Some(next) => {
                            i += 1;
                            next.clone()
                        }
                        None => {
                            let _ =
                                writeln!(stderr, "forge index: {name} needs a value\n\n{USAGE}");
                            return Err(EXIT_USAGE);
                        }
                    },
                };
                if name == "--search" {
                    opt.search = value;
                    opt.search_set = true;
                } else {
                    opt.db = value;
                }
            }
            other => {
                let _ = writeln!(stderr, "forge index: unknown flag {other:?}\n\n{USAGE}");
                return Err(EXIT_USAGE);
            }
        }
    }

    if let Some(first) = rest.first() {
        let _ = writeln!(
            stderr,
            "forge index: unexpected positional argument {first:?}\n\n{USAGE}"
        );
        return Err(EXIT_USAGE);
    }
    if opt.stats && opt.search_set {
        let _ = writeln!(
            stderr,
            "forge index: --stats and --search are mutually exclusive\n\n{USAGE}"
        );
        return Err(EXIT_USAGE);
    }
    Ok(opt)
}

/// Resolves the index file and the data directory its logs come from.
/// `index.db` lives beside `sessions/`, so the index file's parent *is* the data
/// directory: `--db` therefore also selects which logs are indexed.
pub fn resolve_paths(opt_db: &str) -> Result<(PathBuf, PathBuf), Error> {
    if !opt_db.trim().is_empty() {
        let db = PathBuf::from(opt_db);
        let parent = db
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok((db, parent));
    }
    let data_dir = crate::default_data_dir()?;
    let db = data_dir.join("index.db");
    Ok((db, data_dir))
}

// --- JSON documents ----------------------------------------------------
//
// Structs, not maps, so key order is stable across runs, and every array is
// allocated (never empty-as-null) so a consumer never has to branch on null.

#[derive(serde::Serialize)]
struct StatsDoc {
    sessions: i64,
    events: i64,
    bytes: i64,
    workspaces: i64,
}

impl StatsDoc {
    fn of(stats: &Stats) -> Self {
        Self {
            sessions: stats.sessions,
            events: stats.events,
            bytes: stats.bytes,
            workspaces: stats.workspaces,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionDoc {
    id: String,
    workspace: String,
    model: String,
    title: String,
    status: String,
    created_at: i64,
    updated_at: i64,
    turns: i64,
    last_seq: u64,
}

impl SessionDoc {
    fn of(row: &Session) -> Self {
        Self {
            id: row.id.clone(),
            workspace: row.workspace.clone(),
            model: row.model.clone(),
            title: row.title.clone(),
            status: row.status.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            turns: row.turns,
            last_seq: row.last_seq,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HitDoc {
    session_id: String,
    seq: u64,
    turn: u32,
    #[serde(rename = "type")]
    event_type: String,
    snippet: String,
}

impl HitDoc {
    fn of(hit: &Hit) -> Self {
        Self {
            session_id: hit.session_id.clone(),
            seq: hit.seq,
            turn: hit.turn,
            event_type: hit.event_type.clone(),
            snippet: hit.snippet.clone(),
        }
    }
}

#[derive(serde::Serialize)]
struct SummaryDoc {
    db: String,
    mode: String,
    stats: StatsDoc,
    sessions: Vec<SessionDoc>,
}

// --- Human reports -----------------------------------------------------

/// The default report: the index header, the totals, and the first page of
/// sessions.
fn write_summary(
    stdout: &mut dyn Write,
    db_path: &Path,
    mode: &str,
    stats: &Stats,
    rows: &[Session],
) {
    let _ = writeln!(stdout, "index\t{}", db_path.display());
    let _ = writeln!(stdout, "mode\t{mode}");
    write_totals(stdout, stats);
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(stdout);
    let _ = writeln!(stdout, "id\tupdated\tstatus\ttitle");
    for row in rows {
        let _ = writeln!(
            stdout,
            "{}\t{}\t{}\t{}",
            row.id,
            format_time(row.updated_at),
            row.status,
            one_line(&row.title)
        );
    }
}

/// The `--stats` report: the four totals and nothing else.
fn write_totals(stdout: &mut dyn Write, stats: &Stats) {
    let _ = writeln!(stdout, "sessions\t{}", stats.sessions);
    let _ = writeln!(stdout, "events\t{}", stats.events);
    let _ = writeln!(stdout, "bytes\t{}", stats.bytes);
    let _ = writeln!(stdout, "workspaces\t{}", stats.workspaces);
}

/// `YYYY-MM-DD HH:MM` in UTC, the shape the Go build prints.
fn format_time(ms: i64) -> String {
    if ms <= 0 {
        return "-".to_string();
    }
    let secs = ms / 1000;
    let (year, month, day, hour, minute, _second) = crate::civil_from_epoch(secs);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// `forge index` never needs a config value other than the data directory, and
/// it must not be stopped by a sandbox typo.
pub fn data_dir(cfg: &Config) -> Result<PathBuf, Error> {
    if cfg.data_dir.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            "no data directory resolved; pass --db",
        ));
    }
    Ok(PathBuf::from(cfg.data_dir.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn rejects_stats_with_search() {
        let mut err = Vec::new();
        let code = parse(&args(&["--stats", "--search", "x"]), &mut err).unwrap_err();
        assert_eq!(code, EXIT_USAGE);
        let text = String::from_utf8_lossy(&err).to_string();
        assert!(text.contains("mutually exclusive"), "{text}");
    }

    #[test]
    fn rejects_a_positional_argument() {
        let mut err = Vec::new();
        let code = parse(&args(&["nope"]), &mut err).unwrap_err();
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("unexpected positional"));
    }

    #[test]
    fn distinguishes_an_empty_search_from_no_search() {
        let mut err = Vec::new();
        let opt = parse(&args(&["--search", ""]), &mut err).expect("parse");
        assert!(opt.search_set);
        assert!(opt.search.is_empty());

        let opt = parse(&args(&[]), &mut err).expect("parse");
        assert!(!opt.search_set);
    }

    #[test]
    fn takes_values_inline_or_separate() {
        let mut err = Vec::new();
        let opt = parse(&args(&["--search=hello", "--db", "x.db"]), &mut err).expect("parse");
        assert_eq!(opt.search, "hello");
        assert_eq!(opt.db, "x.db");
    }

    #[test]
    fn db_flag_selects_the_data_directory_too() {
        let (db, data) = resolve_paths("nested/x.db").expect("paths");
        assert_eq!(db, PathBuf::from("nested/x.db"));
        assert_eq!(data, PathBuf::from("nested"));

        let (db, data) = resolve_paths("x.db").expect("paths");
        assert_eq!(db, PathBuf::from("x.db"));
        assert_eq!(data, PathBuf::from("."));
    }

    #[test]
    fn missing_index_is_a_failure_and_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("forge-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("index.db");

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["--db", db.to_str().expect("utf-8")]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_FAILURE);
        assert!(out.is_empty(), "stdout must stay empty");
        let text = String::from_utf8_lossy(&err).to_string();
        assert!(text.contains("run 'forge index --rebuild'"), "{text}");
        assert!(!db.exists(), "a missing index must not be created");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebuild_then_report_and_search() {
        let dir = std::env::temp_dir().join(format!("forge-index-rb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).expect("temp dir");
        // One tiny log so rebuild has something to index.
        let log = dir.join("sessions").join("s1.jsonl");
        std::fs::write(
            &log,
            concat!(
                r#"{"v":1,"seq":1,"id":"a","ts":1700000000000,"session":"s1","type":"session/start","payload":{"workspace":"/w","model":"m"}}"#,
                "\n",
                r#"{"v":1,"seq":2,"id":"b","ts":1700000000001,"session":"s1","turn":1,"type":"turn/start","payload":{"number":1}}"#,
                "\n",
                r#"{"v":1,"seq":3,"id":"c","ts":1700000000002,"session":"s1","turn":1,"type":"user/message","payload":{"text":"find the zebra"}}"#,
                "\n"
            ),
        )
        .expect("write log");
        let db = dir.join("index.db");
        let db_arg = db.to_str().expect("utf-8").to_string();

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&args(&["--db", &db_arg, "--rebuild"]), &mut out, &mut err);
        assert_eq!(code, EXIT_OK, "stderr: {}", String::from_utf8_lossy(&err));
        let text = String::from_utf8_lossy(&out).to_string();
        assert!(text.contains("mode\trebuilt"), "{text}");
        assert!(text.contains("sessions\t1"), "{text}");

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["--db", &db_arg, "--search", "zebra"]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK, "stderr: {}", String::from_utf8_lossy(&err));
        let text = String::from_utf8_lossy(&out).to_string();
        assert!(text.contains("s1:3"), "{text}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
