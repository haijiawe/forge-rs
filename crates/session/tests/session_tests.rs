//! Integration tests for `forge-session`.
//!
//! These tests use the shared oracle under `tests/`: the five event fixtures
//! and the golden projection. Fixtures are copied to a temporary directory
//! before a `Store` opens them, because opening repairs torn tails and the
//! oracle must stay untouched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use forge_core::event::{parse_log, Event, EventType};
use forge_core::json::canonical;
use forge_core::ErrorKind;
use serde_json::{json, Value};

use forge_session::decision::DecisionWatcher;
use forge_session::fork::{fork, ForkOptions, ERR_FORK_POINT_MID_TURN};
use forge_session::projection::{Projection, TURN_ABORTED};
use forge_session::store::{Options, Store};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-session-{}-{}-{}",
            name,
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn event(seq: u64, typ: &'static str, turn: u32, step: u32, payload: Value) -> Event {
    let mut event = Event::new("s1", EventType(typ), Some(payload));
    event.seq = seq;
    event.id = format!("evt-{seq:02}");
    event.ts = 1_763_000_000_000 + seq as i64;
    event.turn = turn;
    event.step = step;
    event
}

fn synthetic_events() -> Vec<Event> {
    vec![
        event(
            1,
            "session/start",
            0,
            0,
            json!({"cwd": "/work", "model": "claude-sonnet-4", "profile": "default"}),
        ),
        event(2, "turn/start", 1, 0, json!({})),
        event(3, "user/message", 1, 0, json!({"text": "read notes.txt"})),
        event(
            4,
            "assistant/message",
            1,
            1,
            json!({"text": "Reading notes.txt.", "usage": {"prompt_tokens": 12, "completion_tokens": 5}}),
        ),
        event(
            5,
            "tool/call",
            1,
            1,
            json!({"call_id": "c1", "name": "fs/read", "args": {"path": "notes.txt"}}),
        ),
        event(
            6,
            "tool/result",
            1,
            1,
            json!({"call_id": "c1", "ok": true, "output": "alpha"}),
        ),
        event(
            7,
            "approval/request",
            1,
            0,
            json!({"id": "a1", "call_id": "c2", "kind": "exec", "title": "run shell"}),
        ),
        event(
            8,
            "approval/decision",
            1,
            0,
            json!({"id": "a1", "decision": "approved"}),
        ),
        event(
            9,
            "tool/call",
            1,
            1,
            json!({"call_id": "c2", "name": "shell", "args": {"cmd": "ls"}}),
        ),
        event(
            10,
            "tool/result",
            1,
            1,
            json!({"call_id": "c2", "ok": false, "error": "permission denied", "error_code": "E_EXIT"}),
        ),
        event(11, "turn/end", 1, 0, json!({"reason": "complete"})),
        event(12, "turn/start", 2, 0, json!({})),
        event(13, "user/message", 2, 0, json!({"text": "try again"})),
        event(
            14,
            "tool/call",
            2,
            1,
            json!({"call_id": "c3", "name": "shell", "args": {"cmd": "sleep 30"}}),
        ),
        event(
            15,
            "approval/request",
            2,
            0,
            json!({"id": "a2", "call_id": "c3", "kind": "exec", "title": "run shell", "detail": "long running"}),
        ),
        event(16, "turn/end", 2, 0, json!({"reason": "cancelled"})),
        event(17, "turn/start", 3, 0, json!({})),
        event(
            18,
            "assistant/message",
            3,
            1,
            json!({"text": "partial", "usage": {"prompt_tokens": 3, "completion_tokens": 2}}),
        ),
        event(19, "future/telemetry", 0, 0, json!({"noise": true})),
        event(20, "session/end", 0, 0, json!({"reason": "completed"})),
    ]
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<workspace>/crates/session`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn copy_fixture(name: &str, dir: &TempDir) -> PathBuf {
    let source = workspace_root().join("tests/fixtures/events/v1").join(name);
    let target = dir.join(name);
    fs::copy(&source, &target).expect("copy fixture");
    target
}

#[test]
fn golden_projection_matches_basic_json_canonically() {
    let projection = Projection::fold(&synthetic_events()).expect("fold synthetic log");
    let produced = canonical(&projection.snapshot()).expect("canonical snapshot");

    let golden_path = workspace_root().join("tests/golden/projection/basic.json");
    let golden_text = fs::read_to_string(&golden_path).expect("read golden");
    let golden: Value = serde_json::from_str(&golden_text).expect("parse golden");
    let expected = canonical(&golden).expect("canonical golden");

    assert_eq!(
        produced, expected,
        "projection differs from {golden_path:?}"
    );
}

#[test]
fn golden_projection_reports_skipped_unknown_type() {
    let projection = Projection::fold(&synthetic_events()).unwrap();
    let snapshot = projection.snapshot();
    assert_eq!(snapshot.counters.skipped, 1);
    assert_eq!(snapshot.counters.duplicates, 0);
}

#[test]
fn fixture_logs_replay_to_expected_shapes() {
    let dir = TempDir::new("fixtures");

    let cases: &[(&str, usize, usize)] = &[
        ("happy-path.jsonl", 2, 7),
        ("approval-flow.jsonl", 1, 3),
        ("aborted-turn.jsonl", 1, 2),
        ("unknown-future-event.jsonl", 1, 2),
        ("torn-final-line.jsonl", 0, 0),
    ];

    for (name, turns, messages) in cases {
        let path = copy_fixture(name, &dir);
        let store = Store::open(
            &path,
            Options {
                sync_every: 0,
                no_fsync: true,
            },
        )
        .unwrap_or_else(|e| panic!("open {name}: {e}"));
        let mut events = Vec::new();
        store
            .read_from(0, |event| {
                events.push(event.clone());
                Ok(())
            })
            .unwrap_or_else(|e| panic!("read {name}: {e}"));
        let snapshot = Projection::fold(&events)
            .unwrap_or_else(|e| panic!("fold {name}: {e}"))
            .snapshot();
        if !name.starts_with("torn") {
            assert_eq!(snapshot.counters.turns, *turns, "{name} turns");
            assert_eq!(snapshot.counters.messages, *messages, "{name} messages");
        }
        match *name {
            "happy-path.jsonl" => {
                assert!(snapshot.ended);
                assert_eq!(snapshot.end_reason, "completed");
                assert_eq!(snapshot.counters.tool_calls, 2);
            }
            "approval-flow.jsonl" => {
                assert!(snapshot.pending_approvals.is_empty());
                assert_eq!(snapshot.tool_calls[0].status, "ok");
            }
            "aborted-turn.jsonl" => {
                assert_eq!(snapshot.turns[0].status, TURN_ABORTED);
                assert_eq!(snapshot.end_reason, "aborted");
            }
            "unknown-future-event.jsonl" => {
                assert_eq!(snapshot.counters.skipped, 2);
            }
            "torn-final-line.jsonl" => {
                assert_eq!(events.len(), 6);
                assert_eq!(store.last_seq(), 6);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn torn_tail_is_truncated_and_next_append_continues_the_log() {
    let dir = TempDir::new("torn");
    let path = dir.join("torn.jsonl");
    let complete = [
        event(1, "session/start", 0, 0, json!({"model": "m"})),
        event(2, "turn/start", 1, 0, json!({})),
        event(3, "user/message", 1, 0, json!({"text": "hi"})),
    ];
    let mut text = String::new();
    for event in &complete {
        text.push_str(&event.canonical().unwrap());
        text.push('\n');
    }
    let torn_len = text.len();
    text.push_str(
        r#"{"v":1,"seq":4,"id":"evt-04","ts":1763000000004,"session":"s1","type":"tool/res"#,
    );
    fs::write(&path, text.as_bytes()).unwrap();

    let store = Store::open(
        &path,
        Options {
            sync_every: 0,
            no_fsync: true,
        },
    )
    .unwrap();
    assert_eq!(store.last_seq(), 3);
    assert_eq!(fs::metadata(&path).unwrap().len(), torn_len as u64);

    let next = event(4, "turn/end", 1, 0, json!({"reason": "complete"}));
    assert_eq!(store.append(&next).unwrap(), 4);
    store.close().unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        torn_len as u64 + 1 + next.canonical().unwrap().len() as u64
    );

    let reopened = Store::open(
        &path,
        Options {
            sync_every: 0,
            no_fsync: true,
        },
    )
    .unwrap();
    assert_eq!(reopened.last_seq(), 4);
}

#[test]
fn unsupported_version_refuses_without_touching_the_file() {
    let dir = TempDir::new("version");
    let path = dir.join("v2.jsonl");
    let text = "{\"v\":2,\"seq\":1,\"id\":\"e1\",\"ts\":1,\"session\":\"s\",\"type\":\"session/start\",\"payload\":{}}\n";
    fs::write(&path, text.as_bytes()).unwrap();

    let error = Store::open(
        &path,
        Options {
            sync_every: 0,
            no_fsync: true,
        },
    )
    .err()
    .expect("v2 must be refused");
    assert_eq!(error.kind(), ErrorKind::UnsupportedVersion);
    assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
}

#[test]
fn unsupported_version_in_the_middle_also_refuses() {
    let dir = TempDir::new("version-mid");
    let path = dir.join("mixed.jsonl");
    let first = event(1, "session/start", 0, 0, json!({"model": "m"}));
    let mut text = first.canonical().unwrap() + "\n";
    text.push_str("{\"v\":2,\"seq\":2,\"id\":\"e2\",\"ts\":2,\"session\":\"s1\",\"type\":\"turn/start\",\"payload\":{}}\n");
    fs::write(&path, text.as_bytes()).unwrap();

    let error = Store::open(
        &path,
        Options {
            sync_every: 0,
            no_fsync: true,
        },
    )
    .err()
    .expect("mixed versions must be refused");
    assert_eq!(error.kind(), ErrorKind::UnsupportedVersion);
    assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
}

#[test]
fn append_assigns_dense_sequences_and_read_from_filters() {
    let dir = TempDir::new("dense");
    let path = dir.join("dense.jsonl");
    let store = Store::open(
        &path,
        Options {
            sync_every: 0,
            no_fsync: true,
        },
    )
    .unwrap();
    for i in 0..5 {
        let event = event(0, "user/message", 1, 0, json!({"text": format!("m{i}")}));
        assert_eq!(store.append(&event).unwrap(), i + 1);
    }
    // `Event::new` leaves identity to the store; append must mint it.
    let mut raw = Event::new("s1", EventType::USER_MESSAGE, Some(json!({"text": "raw"})));
    raw.ts = 0;
    assert!(raw.id.is_empty());
    assert_eq!(store.append(&raw).unwrap(), 6);
    let mut seen = Vec::new();
    store
        .read_from(3, |event| {
            assert!(!event.id.is_empty());
            seen.push(event.seq);
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, vec![3, 4, 5, 6]);
    store.close().unwrap();
}

#[test]
fn concurrent_appends_are_dense_and_unique() {
    let dir = TempDir::new("concurrent");
    let path = dir.join("concurrent.jsonl");
    let store = Arc::new(
        Store::open(
            &path,
            Options {
                sync_every: 0,
                no_fsync: true,
            },
        )
        .unwrap(),
    );
    let mut threads = Vec::new();
    for thread_index in 0..8 {
        let store = Arc::clone(&store);
        threads.push(thread::spawn(move || {
            for i in 0..50 {
                let event = event(
                    0,
                    "user/message",
                    1,
                    0,
                    json!({"text": format!("{thread_index}-{i}")}),
                );
                store.append(&event).unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(store.last_seq(), 400);

    let mut seen = Vec::new();
    store
        .read_from(1, |event| {
            seen.push(event.seq);
            Ok(())
        })
        .unwrap();
    assert_eq!(seen.len(), 400);
    assert_eq!(seen, (1..=400).collect::<Vec<_>>());
}

#[test]
fn replaying_the_same_log_twice_is_idempotent_for_the_view() {
    let events = synthetic_events();
    let mut projection = Projection::new();
    for event in &events {
        projection.apply(event).unwrap();
    }
    let first = projection.snapshot();
    for event in &events {
        projection.apply(event).unwrap();
    }
    let second = projection.snapshot();

    assert_eq!(first.turns, second.turns);
    assert_eq!(first.messages, second.messages);
    assert_eq!(first.tool_calls, second.tool_calls);
    assert_eq!(first.pending_approvals, second.pending_approvals);
    assert_eq!(second.counters.duplicates, events.len());
}

#[test]
fn malformed_payload_of_known_type_is_an_error() {
    let error = Projection::fold(&[event(1, "turn/start", 1, 0, json!("not-an-object"))])
        .err()
        .expect("known type with bad payload must fail");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.message().contains("malformed payload"), "{error}");
}

#[test]
fn tool_result_without_call_becomes_an_orphan() {
    let projection = Projection::fold(&[
        event(1, "session/start", 0, 0, json!({"model": "m"})),
        event(
            2,
            "tool/result",
            1,
            1,
            json!({"call_id": "missing", "ok": true, "output": "x"}),
        ),
    ])
    .unwrap()
    .snapshot();
    assert_eq!(projection.tool_calls.len(), 1);
    assert!(projection.tool_calls[0].orphan);
    assert_eq!(projection.messages[0].kind, "tool");
}

#[test]
fn approval_decision_without_id_closes_by_call_id() {
    let projection = Projection::fold(&[
        event(
            1,
            "approval/request",
            1,
            0,
            json!({"id": "a1", "callId": "c1"}),
        ),
        event(
            2,
            "approval/decision",
            1,
            0,
            json!({"callId": "c1", "decision": "allow"}),
        ),
    ])
    .unwrap()
    .snapshot();
    assert!(projection.pending_approvals.is_empty());
}

#[test]
fn turn_end_without_turn_start_synthesises_a_turn() {
    let snapshot = Projection::fold(&[event(1, "turn/end", 7, 0, json!({"reason": "cancelled"}))])
        .unwrap()
        .snapshot();
    assert_eq!(snapshot.turns.len(), 1);
    assert_eq!(snapshot.turns[0].number, 7);
    assert_eq!(snapshot.turns[0].status, TURN_ABORTED);
}

#[test]
fn compaction_hides_messages_without_rewriting_counters() {
    let mut events = synthetic_events();
    events.push(event(
        21,
        "compaction/summary",
        3,
        0,
        json!({"summary": "history compacted"}),
    ));
    let snapshot = Projection::fold(&events).unwrap().snapshot();
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].kind, "summary");
    assert_eq!(snapshot.counters.messages, 1);
    assert_eq!(snapshot.counters.turns, 3);
    assert_eq!(snapshot.counters.steps, 2);
}

#[test]
fn decision_watcher_retries_a_torn_tail_without_duplicating() {
    let dir = TempDir::new("decision");
    let path = dir.join("log.jsonl");
    let request = event(
        1,
        "approval/request",
        1,
        0,
        json!({"id": "a1", "callId": "c1"}),
    );
    fs::write(&path, request.canonical().unwrap() + "\n").unwrap();

    let mut watcher = DecisionWatcher::new(&path);
    assert_eq!(watcher.poll("c1").unwrap(), None);

    // The writer starts the answer but dies before the newline.
    let decision = event(
        2,
        "approval/decision",
        1,
        0,
        json!({"callId": "c1", "decision": "allow"}),
    );
    let line = decision.canonical().unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    use std::io::Write;
    file.write_all(line.as_bytes()).unwrap();
    drop(file);
    assert_eq!(watcher.poll("c1").unwrap(), None);

    // The rest of the line arrives: the watcher must see it exactly once.
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);
    let found = watcher.poll("c1").unwrap().expect("decision after newline");
    assert_eq!(found, "allow");
    assert!(watcher.poll("c1").unwrap().is_none());
}

#[test]
fn decision_watcher_skips_other_calls_and_missing_file() {
    let dir = TempDir::new("decision-other");
    let mut watcher = DecisionWatcher::new(dir.join("absent.jsonl"));
    assert_eq!(watcher.poll("c1").unwrap(), None);

    let path = dir.join("log.jsonl");
    let other = event(
        1,
        "approval/decision",
        1,
        0,
        json!({"callId": "other", "decision": "deny"}),
    );
    let mine = event(
        2,
        "approval/decision",
        1,
        0,
        json!({"callId": "c1", "decision": "allow"}),
    );
    fs::write(
        &path,
        format!(
            "{}\n{}\n",
            other.canonical().unwrap(),
            mine.canonical().unwrap()
        ),
    )
    .unwrap();
    let mut watcher = DecisionWatcher::new(&path);
    assert_eq!(watcher.poll("c1").unwrap().unwrap(), "allow");
}

fn write_parent_log(path: &Path, turns: usize) {
    let store = Store::open(
        path,
        Options {
            sync_every: 0,
            no_fsync: true,
        },
    )
    .unwrap();
    let mut seq = 0u64;
    let push = |typ: &'static str, turn: u32, step: u32, payload: Value, seq: &mut u64| {
        let mut event = Event::new("parent", EventType(typ), Some(payload));
        event.id = format!("parent-{}", *seq + 1);
        event.ts = 1_763_000_000_000 + *seq as i64 + 1;
        event.turn = turn;
        event.step = step;
        *seq = store.append(&event).unwrap();
    };
    push(
        "session/start",
        0,
        0,
        json!({"workspace": "D:/work", "model": "m", "profile": "default"}),
        &mut seq,
    );
    for turn in 1..=turns {
        push("turn/start", turn as u32, 0, json!({}), &mut seq);
        push(
            "user/message",
            turn as u32,
            0,
            json!({"text": format!("turn {turn}")}),
            &mut seq,
        );
        push(
            "assistant/message",
            turn as u32,
            1,
            json!({"text": "ok"}),
            &mut seq,
        );
        push(
            "turn/end",
            turn as u32,
            0,
            json!({"reason": "complete"}),
            &mut seq,
        );
    }
    store.close().unwrap();
}

#[test]
fn fork_copies_completed_prefix_with_dense_seq_and_provenance() {
    let dir = TempDir::new("fork");
    let parent_path = dir.join("parent.jsonl");
    write_parent_log(&parent_path, 2);
    let parent_bytes = fs::read(&parent_path).unwrap();

    let target_path = dir.join("child.jsonl");
    let result = fork(&ForkOptions {
        parent_path: parent_path.clone(),
        target_path: target_path.clone(),
        target_id: "child".to_string(),
        at: 0,
    })
    .unwrap();

    assert_eq!(result.parent_id, "parent");
    assert_eq!(result.session_id, "child");
    assert_eq!(result.forked_at_seq, 9);
    assert_eq!(result.events, 9);

    let child_text = fs::read_to_string(&target_path).unwrap();
    let child: Vec<Event> = parse_log(&child_text).unwrap();
    assert_eq!(child.len(), 9);
    assert_eq!(child[0].typ, "session/start");
    assert_eq!(child[0].session, "child");
    assert_eq!(
        child[0].payload.as_ref().unwrap()["forkedFrom"],
        json!("parent")
    );
    assert_eq!(child[0].payload.as_ref().unwrap()["forkedAtSeq"], json!(9));
    assert_eq!(
        child[0].payload.as_ref().unwrap()["workspace"],
        json!("D:/work")
    );
    assert!(child[0].payload.as_ref().unwrap()["forkedAt"].is_number());
    assert!(child.iter().enumerate().all(|(i, e)| e.seq == i as u64 + 1));
    assert!(child.iter().all(|e| e.session == "child"));
    // events-v1 §4.1 allows exactly two envelope changes per copied record:
    // `session` and `seq`. Everything else — id, ts, turn, step, type, payload
    // — is the parent's bytes, so a copied record stays attributable to the
    // record it came from. The head's payload is the one exception: it gains
    // the three provenance members.
    let parent: Vec<Event> = parse_log(&fs::read_to_string(&parent_path).unwrap()).unwrap();
    assert_eq!(child.len(), parent.len());
    for (i, (copied, original)) in child.iter().zip(parent.iter()).enumerate() {
        assert_eq!(copied.id, original.id, "record {i}: id must be copied");
        assert_eq!(copied.ts, original.ts, "record {i}: ts must be copied");
        assert_eq!(copied.typ, original.typ, "record {i}: type must be copied");
        assert_eq!(copied.turn, original.turn, "record {i}: turn must match");
        assert_eq!(copied.step, original.step, "record {i}: step must match");
        if i > 0 {
            assert_eq!(
                copied.payload, original.payload,
                "record {i}: payload must be copied verbatim"
            );
        }
    }
    let mut ids: Vec<&str> = child.iter().map(|e| e.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        child.len(),
        "an id shared with the parent is allowed (§9 is per-log); a duplicate \
         inside the child is not"
    );
    assert_eq!(fs::read(&parent_path).unwrap(), parent_bytes);
}

#[test]
fn fork_from_torn_parent_copies_only_complete_records() {
    let dir = TempDir::new("fork-torn");
    let parent_path = dir.join("parent.jsonl");
    write_parent_log(&parent_path, 1);
    let mut bytes = fs::read(&parent_path).unwrap();
    let before = bytes.len();
    bytes.extend_from_slice(br#"{"v":1,"seq":6,"id":"torn""#);
    fs::write(&parent_path, &bytes).unwrap();

    let target_path = dir.join("child.jsonl");
    let result = fork(&ForkOptions {
        parent_path: parent_path.clone(),
        target_path,
        target_id: "child".to_string(),
        at: 0,
    })
    .unwrap();
    assert_eq!(result.events, 5);
    assert_eq!(
        fs::read(&parent_path).unwrap().len(),
        before + br#"{"v":1,"seq":6,"id":"torn""#.len()
    );
}

#[test]
fn fork_inside_a_turn_is_refused_and_leaves_no_target() {
    let dir = TempDir::new("fork-mid");
    let parent_path = dir.join("parent.jsonl");
    write_parent_log(&parent_path, 1);
    let target_path = dir.join("child.jsonl");

    let error = fork(&ForkOptions {
        parent_path,
        target_path: target_path.clone(),
        target_id: "child".to_string(),
        at: 2, // turn/start: prefix ends mid-turn
    })
    .expect_err("mid-turn fork must be refused");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert_eq!(error.message(), ERR_FORK_POINT_MID_TURN);
    assert!(!target_path.exists());
}

#[test]
fn fork_synthesises_a_head_when_parent_has_none() {
    let dir = TempDir::new("fork-no-head");
    let parent_path = dir.join("parent.jsonl");
    let mut parent_events = vec![
        event(1, "turn/start", 1, 0, json!({})),
        event(2, "user/message", 1, 0, json!({"text": "hi"})),
        event(3, "assistant/message", 1, 1, json!({"text": "ok"})),
        event(4, "turn/end", 1, 0, json!({"reason": "complete"})),
    ];
    for event in &mut parent_events {
        event.session = "parent".to_string();
    }
    let mut text = String::new();
    for event in &parent_events {
        text.push_str(&event.canonical().unwrap());
        text.push('\n');
    }
    fs::write(&parent_path, text).unwrap();

    let target_path = dir.join("child.jsonl");
    let result = fork(&ForkOptions {
        parent_path,
        target_path: target_path.clone(),
        target_id: "child".to_string(),
        at: 0,
    })
    .unwrap();
    assert_eq!(result.events, 5);
    let child = parse_log(&fs::read_to_string(&target_path).unwrap()).unwrap();
    assert_eq!(child[0].typ, "session/start");
    assert_eq!(
        child[0].payload.as_ref().unwrap()["startedBy"],
        json!("fork")
    );
    assert_eq!(child[1].typ, "turn/start");
    assert_eq!(child[1].seq, 2);
}

#[test]
fn fork_target_must_match_target_id() {
    let dir = TempDir::new("fork-name");
    let parent_path = dir.join("parent.jsonl");
    write_parent_log(&parent_path, 1);
    let error = fork(&ForkOptions {
        parent_path,
        target_path: dir.join("wrong.jsonl"),
        target_id: "child".to_string(),
        at: 0,
    })
    .expect_err("name mismatch must be refused");
    assert_eq!(error.kind(), ErrorKind::Invalid);
}

/// An attachment is projected as a reference, and the bytes are never in the
/// projection: the log holds metadata and the blob store holds the body
/// (`docs/contracts/events-v1.md` §4.0.1).
#[test]
fn attachment_references_survive_projection_without_their_bodies() {
    let events = vec![
        event(
            1,
            "session/start",
            0,
            0,
            json!({"workspace": "/w", "startedBy": "http"}),
        ),
        event(2, "turn/start", 1, 0, json!({"number": 1})),
        event(
            3,
            "user/message",
            1,
            0,
            json!({
                "text": "look at this",
                "attachments": [{
                    "kind": "image",
                    "id": "0123456789abcdef0123456789abcdef",
                    "mediaType": "image/png",
                    "name": "shot.png",
                    "bytes": 8,
                }],
            }),
        ),
        event(
            4,
            "turn/end",
            1,
            0,
            json!({"reason": "end_turn", "steps": 1}),
        ),
    ];
    let projection = Projection::fold(&events).expect("fold");
    let snapshot = projection.snapshot();
    let message = snapshot
        .messages
        .iter()
        .find(|m| m.text == "look at this")
        .expect("the user message");
    assert_eq!(message.attachments.len(), 1);
    let reference = &message.attachments[0];
    assert_eq!(reference.kind, "image");
    assert_eq!(reference.id, "0123456789abcdef0123456789abcdef");
    assert_eq!(reference.media_type, "image/png");
    assert_eq!(reference.name, "shot.png");
    assert_eq!(reference.bytes, 8);

    // The claim that matters for storage: nothing in the projected snapshot is
    // a body. A base64 blob of eight bytes would show up as a long field.
    let canonical = canonical(&snapshot).expect("canonical");
    assert!(!canonical.contains("data"), "{canonical}");
}

/// A log written before attachments existed still projects, with no references.
#[test]
fn a_message_without_attachments_projects_with_an_empty_list() {
    let events = vec![
        event(
            1,
            "session/start",
            0,
            0,
            json!({"workspace": "/w", "startedBy": "http"}),
        ),
        event(2, "turn/start", 1, 0, json!({"number": 1})),
        event(3, "user/message", 1, 0, json!({"text": "hello"})),
        event(
            4,
            "turn/end",
            1,
            0,
            json!({"reason": "end_turn", "steps": 1}),
        ),
    ];
    let projection = Projection::fold(&events).expect("fold");
    let message = projection
        .snapshot()
        .messages
        .into_iter()
        .find(|m| m.text == "hello")
        .expect("the user message");
    assert!(message.attachments.is_empty());
}
