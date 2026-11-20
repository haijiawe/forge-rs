//! Conformance tests for the M4 session index surface. Each test checks a rule
//! that the Go oracle pins; none edits the oracle's fixtures.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hearth_store::{is_bad_cursor, is_closed, Options, Query, Session, Store};
use serde_json::{json, Value};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

const BASE_TS: i64 = 1_700_000_000_000;

fn fixed_now() -> i64 {
    BASE_TS
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("hearth-store-{tag}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn data(&self) -> PathBuf {
        self.path.join("data")
    }

    fn sessions(&self) -> PathBuf {
        self.data().join("sessions")
    }

    fn index(&self) -> PathBuf {
        self.path.join("index.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn open(temp: &TempDir) -> Store {
    Store::open(Options {
        path: temp.index(),
        now: Some(fixed_now),
    })
    .expect("open store")
}

fn session(id: &str, updated_at: i64, status: &str, workspace: &str) -> Session {
    Session {
        id: id.to_owned(),
        workspace: workspace.to_owned(),
        model: "test-model".to_owned(),
        title: format!("title {id}"),
        status: status.to_owned(),
        created_at: updated_at - 10,
        updated_at,
        turns: 1,
        last_seq: 8,
    }
}

fn event_line(seq: u64, typ: &str, ts: i64, turn: u32, payload: Value) -> String {
    json!({
        "v": 1,
        "seq": seq,
        "id": format!("e{seq}"),
        "ts": ts,
        "session": "s1",
        "type": typ,
        "turn": turn,
        "payload": payload,
    })
    .to_string()
}

fn write_log(dir: &Path, id: &str, marker: &str) {
    let ts = BASE_TS;
    let lines = [
        event_line(
            1,
            "session/start",
            ts,
            0,
            json!({"workspace": format!("/w/{id}"), "cwd": "/tmp", "model": "test-model"}),
        ),
        event_line(2, "turn/start", ts + 10, 1, json!({"number": 1})),
        event_line(
            3,
            "user/message",
            ts + 20,
            1,
            json!({"text": format!("{marker} please fix the build")}),
        ),
        event_line(
            4,
            "assistant/message",
            ts + 30,
            1,
            json!({"content": format!("looking at the {marker} failure now")}),
        ),
        event_line(
            5,
            "tool/call",
            ts + 40,
            1,
            json!({"callId": "c1", "name": "run"}),
        ),
        event_line(
            6,
            "tool/result",
            ts + 50,
            1,
            json!({"callId": "c1", "output": "ZEBRAPAYLOAD only lives in this tool result"}),
        ),
        event_line(7, "turn/end", ts + 60, 1, json!({"reason": "complete"})),
        event_line(8, "session/end", ts + 70, 0, json!({"reason": "done"})),
    ];
    fs::create_dir_all(dir).expect("create sessions dir");
    fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n") + "\n").expect("write log");
}

fn remove_index_files(temp: &TempDir) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(temp.path.join(format!("index.db{suffix}")));
    }
}

#[test]
fn put_list_stats_delete_and_closed_errors() {
    let temp = TempDir::new("put");
    let store = open(&temp);
    assert_eq!(store.path(), temp.index().as_path());

    for (id, updated, status, workspace) in [
        ("a", 300, "running", "w1"),
        ("b", 200, "idle", "w1"),
        ("c", 100, "ended", "w2"),
    ] {
        store
            .put(&session(id, updated, status, workspace))
            .expect("put");
    }
    let (rows, _) = store.list(Query::default()).expect("list");
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );

    // An older Put must never move a row backwards; an equal timestamp still
    // applies, matching the oracle's `>=` upsert predicate.
    let mut stale = session("a", 50, "idle", "w1");
    stale.title = "stale".to_owned();
    store.put(&stale).expect("stale put");
    let (rows, _) = store.list(Query::default()).expect("list");
    assert_eq!(rows[0].title, "title a");
    let mut equal = session("a", 300, "running", "w1");
    equal.title = "equal".to_owned();
    store.put(&equal).expect("equal put");
    let (rows, _) = store.list(Query::default()).expect("list");
    assert_eq!(rows[0].title, "equal");

    let mut cursor = String::new();
    let mut paged = Vec::new();
    loop {
        let (rows, next) = store
            .list(Query {
                limit: 1,
                cursor: cursor.clone(),
                ..Query::default()
            })
            .expect("paged list");
        paged.extend(rows.into_iter().map(|row| row.id));
        if next.is_empty() {
            break;
        }
        cursor = next;
    }
    assert_eq!(paged, ["a", "b", "c"]);

    let (filtered, _) = store
        .list(Query {
            status: "running".to_owned(),
            workspace: "w1".to_owned(),
            ..Query::default()
        })
        .expect("filtered list");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, "a");

    let stats = store.stats().expect("stats");
    assert_eq!(stats.sessions, 3);
    assert_eq!(stats.events, 24);
    assert_eq!(stats.workspaces, 2);

    store.delete("a").expect("delete");
    store.delete("missing").expect("delete missing");
    store.delete("").expect("delete empty");
    assert_eq!(store.stats().expect("stats").sessions, 2);

    store.close().expect("close");
    store.close().expect("close twice");
    let empty = Session::default();
    assert!(is_closed(&store.put(&empty).expect_err("closed put")));
    assert!(is_closed(&store.delete("a").expect_err("closed delete")));
    assert!(is_closed(
        &store.list(Query::default()).expect_err("closed list")
    ));
    assert!(is_closed(
        &store
            .search("x", Query::default())
            .expect_err("closed search")
    ));
    assert!(is_closed(&store.stats().expect_err("closed stats")));
    assert!(is_closed(
        &store.rebuild(temp.data()).expect_err("closed rebuild")
    ));
}

#[test]
fn cursor_survives_concurrent_updates_and_rejects_bad_tokens() {
    let temp = TempDir::new("cursor");
    let store = open(&temp);
    for (id, updated) in [("s1", 100), ("s2", 90), ("s3", 80)] {
        store.put(&session(id, updated, "idle", "w")).expect("put");
    }
    let (_, cursor) = store
        .list(Query {
            limit: 1,
            ..Query::default()
        })
        .expect("first page");
    assert!(!cursor.is_empty());

    // s1 moves to the end after the cursor was minted. An OFFSET cursor would
    // now repeat it or skip s2; a keyset cursor must not.
    store
        .put(&session("s1", 10, "idle", "w"))
        .expect("update s1");
    let (rows, _) = store
        .list(Query {
            limit: 10,
            cursor,
            ..Query::default()
        })
        .expect("second page");
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["s2", "s3"]
    );

    for bad in ["!!!", "e30", "eyJ1IjoxfQ"] {
        let error = store
            .list(Query {
                cursor: bad.to_owned(),
                ..Query::default()
            })
            .expect_err("bad cursor");
        assert!(is_bad_cursor(&error), "{bad}: {error:?}");
    }
}

#[test]
fn unknown_schema_version_drops_and_rebuilds() {
    let temp = TempDir::new("schema");
    let store = open(&temp);
    store.put(&session("x", 100, "idle", "w")).expect("put");
    store.close().expect("close");

    {
        let conn = rusqlite::Connection::open(temp.index()).expect("open raw");
        conn.pragma_update(None, "user_version", 99_i64)
            .expect("bump version");
    }
    let store = open(&temp);
    assert!(store.list(Query::default()).expect("list").0.is_empty());
    let version: i64 = {
        let conn = rusqlite::Connection::open(temp.index()).expect("open raw");
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version")
    };
    assert_eq!(version, 1);
    store
        .put(&session("y", 100, "idle", "w"))
        .expect("put after reset");
    assert_eq!(store.list(Query::default()).expect("list").0.len(), 1);
}

#[test]
fn rebuild_projects_rows_and_indexes_only_boundary_messages() {
    let temp = TempDir::new("rebuild");
    write_log(&temp.sessions(), "s1", "alpha");
    write_log(&temp.sessions(), "s2", "kappa");
    // Unusable entries are skipped, never repaired.
    fs::write(temp.sessions().join("notes.txt"), "not a log").expect("notes");
    fs::create_dir_all(temp.sessions().join("dir.jsonl")).expect("directory named jsonl");
    fs::write(temp.sessions().join("..bad.jsonl"), "garbage").expect("bad id");

    let store = open(&temp);
    assert_eq!(store.rebuild(temp.data()).expect("rebuild"), 2);
    let rows = store.list(Query::default()).expect("list").0;
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.status, hearth_protocol::status::ENDED);
        assert_eq!(row.model, "test-model");
        assert_eq!(row.turns, 1);
        assert_eq!(row.last_seq, 8);
        assert_eq!(row.created_at, BASE_TS);
        assert_eq!(row.updated_at, BASE_TS + 70);
        assert!(row.title.contains("please fix the build"));
        assert_eq!(row.workspace, format!("/w/{}", row.id));
    }
    let stats = store.stats().expect("stats");
    assert_eq!(stats.sessions, 2);
    assert_eq!(stats.events, 16);
    assert_eq!(stats.workspaces, 2);
    assert!(stats.bytes > 0);

    let hits = store.search("kappa", Query::default()).expect("search");
    assert_eq!(hits.len(), 2);
    let mut hit_types: Vec<_> = hits.iter().map(|hit| hit.event_type.as_str()).collect();
    hit_types.sort_unstable();
    // Only s2 contains "kappa": both its boundary messages match, while the
    // tool result with ZEBRAPAYLOAD is deliberately not indexed.
    assert_eq!(hit_types, ["assistant/message", "user/message"]);
    assert!(store
        .search("ZEBRAPAYLOAD", Query::default())
        .expect("search tool output")
        .is_empty());

    // A torn final line keeps the good prefix and leaves the file untouched.
    let torn_path = temp.sessions().join("s1.jsonl");
    let original = fs::read(&torn_path).expect("read log");
    let mut appended = original.clone();
    appended.extend_from_slice(b"{\"v\":1,\"seq\":9,\"id\":\"e9\",\"ts\":1700000000080,\"sess");
    fs::write(&torn_path, &appended).expect("write torn tail");
    assert_eq!(store.rebuild(temp.data()).expect("rebuild torn"), 2);
    assert_eq!(fs::read(&torn_path).expect("read log again"), appended);
    let rows = store.list(Query::default()).expect("list").0;
    assert!(rows.iter().all(|row| row.last_seq == 8));
    let s1 = rows.iter().find(|row| row.id == "s1").expect("s1");
    assert_eq!(s1.updated_at, BASE_TS + 70);

    // One foreign-generation log is skipped whole, without costing the others.
    fs::write(
        temp.sessions().join("future.jsonl"),
        "{\"v\":2,\"seq\":1,\"id\":\"e1\",\"ts\":1700000000000,\"session\":\"s1\",\"type\":\"session/start\"}\n",
    )
    .expect("write future log");
    assert_eq!(store.rebuild(temp.data()).expect("rebuild future"), 2);
    store.close().expect("close");

    // Delete every index file; the logs alone restore everything.
    remove_index_files(&temp);
    let store = open(&temp);
    assert!(store.list(Query::default()).expect("list").0.is_empty());
    assert_eq!(store.rebuild(temp.data()).expect("rebuild fresh"), 2);
    assert_eq!(
        store
            .search("kappa", Query::default())
            .expect("search")
            .len(),
        2
    );
}

#[test]
fn search_tokens_snippets_and_limits() {
    let temp = TempDir::new("search");
    fs::create_dir_all(temp.sessions()).expect("mkdir");
    let lines = [
        event_line(1, "session/start", BASE_TS, 0, json!({"workspace": "/w/a"})),
        event_line(
            2,
            "user/message",
            BASE_TS + 1,
            1,
            json!({"text": "forgotten"}),
        ),
        event_line(
            3,
            "assistant/message",
            BASE_TS + 2,
            1,
            json!({"content": "forgotten flags"}),
        ),
        event_line(4, "user/message", BASE_TS + 3, 1, json!({"text": "zebra"})),
        event_line(5, "session/end", BASE_TS + 4, 0, json!({"reason": "done"})),
    ];
    fs::write(temp.sessions().join("a.jsonl"), lines.join("\n") + "\n").expect("write log");

    let store = open(&temp);
    assert_eq!(store.rebuild(temp.data()).expect("rebuild"), 1);
    for (query, expected) in [
        ("", 0),
        ("   ", 0),
        ("!!!", 0),
        ("forgotten", 2),
        ("forg*", 2),
        ("forg", 2),
        ("forgotten flags", 1),
        ("zebra", 1),
        ("forgotten\" OR 1=1 --", 0),
    ] {
        let hits = store.search(query, Query::default()).expect("search");
        assert_eq!(hits.len(), expected, "query {query:?}");
    }
    assert_eq!(
        store
            .search(
                "forgotten",
                Query {
                    limit: 1,
                    ..Query::default()
                }
            )
            .expect("limited search")
            .len(),
        1
    );

    let long = format!("{} needleword {}", "界".repeat(100), "x".repeat(200));
    fs::write(
        temp.sessions().join("b.jsonl"),
        [
            event_line(1, "session/start", BASE_TS, 0, json!({"workspace": "/w/b"})),
            event_line(2, "user/message", BASE_TS + 1, 1, json!({"text": long})),
            event_line(3, "session/end", BASE_TS + 2, 0, json!({"reason": "done"})),
        ]
        .join("\n")
            + "\n",
    )
    .expect("write long log");
    assert_eq!(store.rebuild(temp.data()).expect("rebuild long"), 2);
    let hits = store
        .search("needleword", Query::default())
        .expect("search long");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.len() <= 200, "{}", hits[0].snippet.len());
    assert!(hits[0].snippet.contains("needleword"));
    assert!(hits[0].snippet.starts_with('…') && hits[0].snippet.ends_with('…'));

    store.delete("a").expect("delete a");
    assert!(store
        .search("forgotten", Query::default())
        .expect("search")
        .is_empty());
    assert_eq!(
        store
            .search("needleword", Query::default())
            .expect("search")
            .len(),
        1
    );
}

#[test]
fn rebuild_1000_sessions_100k_events_within_budget() {
    let temp = TempDir::new("timing");
    fs::create_dir_all(temp.sessions()).expect("mkdir");
    for index in 0..1000 {
        let id = format!("s{index:04}");
        let mut text = String::with_capacity(24_000);
        let mut seq = 0u64;
        let mut push = |typ: &str, turn: u32, payload: String, text: &mut String| {
            seq += 1;
            let _ = writeln!(
                text,
                "{{\"v\":1,\"seq\":{seq},\"id\":\"e{seq}\",\"ts\":{},\"session\":\"s1\",\"type\":\"{typ}\",\"turn\":{turn},\"payload\":{payload}}}",
                BASE_TS + i64::try_from(seq).unwrap_or(i64::MAX)
            );
        };
        push(
            "session/start",
            0,
            json!({"workspace": "/w/timing", "model": "test-model"}).to_string(),
            &mut text,
        );
        for turn in 1..=24u32 {
            push(
                "turn/start",
                turn,
                json!({"number": turn}).to_string(),
                &mut text,
            );
            push(
                "user/message",
                turn,
                json!({"text": format!("needleword {index}")}).to_string(),
                &mut text,
            );
            push(
                "assistant/message",
                turn,
                json!({"text": format!("answer {index}")}).to_string(),
                &mut text,
            );
            push(
                "turn/end",
                turn,
                json!({"reason": "complete"}).to_string(),
                &mut text,
            );
        }
        push(
            "session/end",
            0,
            json!({"reason": "done"}).to_string(),
            &mut text,
        );
        fs::write(temp.sessions().join(format!("{id}.jsonl")), text).expect("write timing log");
    }

    // The Go oracle's timing test measures the best of three and asserts 10 s;
    // every run is printed so a regression cannot hide behind the minimum.
    let mut timings = Vec::new();
    for run in 1..=3 {
        remove_index_files(&temp);
        let store = open(&temp);
        let started = Instant::now();
        let indexed = store.rebuild(temp.data()).expect("rebuild timing corpus");
        let elapsed = started.elapsed();
        eprintln!("rebuild run {run}: {indexed} sessions, {elapsed:?}");
        assert_eq!(indexed, 1000);
        timings.push(elapsed);
        store.close().expect("close");
    }
    let best = timings.iter().min().copied().expect("three runs");
    eprintln!("best rebuild: {best:?} for 1000 sessions / 100000 events");
    assert!(
        best <= Duration::from_secs(10),
        "best rebuild {best:?} exceeds the 10 s oracle budget"
    );
}
