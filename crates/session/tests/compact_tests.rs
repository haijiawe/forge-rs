//! Integration tests for context compaction (`crates/session/src/compact.rs`).
//!
//! The cases mirror `forge-v2/internal/session/compact_test.go` one-for-one where
//! the behaviour is contractual: which range a plan may name, what a writer
//! refuses, and what a reader sees afterwards. Every test builds its log through
//! the real store, so the seqs under test are the ones a session would actually
//! have.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use forge_core::event::{Event, EventType};
use serde_json::{json, Value};

use forge_session::compact::{
    compact, compact_log, is_already_compacted, is_empty_summary, is_invalid_log, is_invalid_plan,
    last_compacted_span, plan_compaction, Summarizer, TruncatingSummarizer, DEFAULT_KEEP_TURNS,
    DEFAULT_SUMMARY_BYTES,
};
use forge_session::projection::{Projection, MESSAGE_SUMMARY};
use forge_session::store::{Options, Store};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "forge-compact-{}-{}-{}",
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

fn read_all(store: &Store) -> Vec<Event> {
    let mut events = Vec::new();
    store
        .read_from(1, |event| {
            events.push(event.clone());
            Ok(())
        })
        .expect("read log");
    events
}

fn append(store: &Store, typ: &'static str, turn: u32, step: u32, payload: Value) -> u64 {
    let mut event = Event::new("s1", EventType(typ), Some(payload));
    event.turn = turn;
    event.step = step;
    store.append(&event).expect("append")
}

/// One closed turn: turn/start, user/message, assistant/message, turn/end.
fn append_turn(store: &Store, turn: u32) {
    append(store, "turn/start", turn, 0, json!({"number": turn}));
    append(
        store,
        "user/message",
        turn,
        0,
        json!({"text": format!("question {turn}")}),
    );
    append(
        store,
        "assistant/message",
        turn,
        1,
        json!({"text": format!("answer {turn}")}),
    );
    append(
        store,
        "turn/end",
        turn,
        1,
        json!({"reason": "completed", "steps": 1}),
    );
}

/// A session with `turns` closed turns, and a store positioned on it.
fn seeded(dir: &TempDir, turns: u32) -> Store {
    let store = Store::open(dir.join("events.jsonl"), Options::default()).expect("open store");
    append(
        &store,
        "session/start",
        0,
        0,
        json!({"cwd": "/work", "model": "m", "profile": "default"}),
    );
    for turn in 1..=turns {
        append_turn(&store, turn);
    }
    store
}

#[test]
fn plan_selects_the_oldest_turns_and_keeps_the_window() {
    let dir = TempDir::new("plan-window");
    let store = seeded(&dir, 5);
    let events = read_all(&store);

    let plan = plan_compaction(&events, 0).expect("plan");
    assert!(!plan.is_empty(), "five closed turns leave room to compact");
    // Two turns are kept (the default), so the plan must end at the turn/end of
    // turn 3. It starts at the first seq of the log, not at the first turn/start:
    // the range names a prefix, and session/start is part of that prefix.
    let turn_ends: Vec<u64> = events
        .iter()
        .filter(|e| e.typ == "turn/end")
        .map(|e| e.seq)
        .collect();
    assert_eq!(plan.from, 1);
    assert_eq!(plan.to, turn_ends[2]);
    assert!(
        plan.reason.contains("keep the last 2 of 5 turns"),
        "{}",
        plan.reason
    );
    assert_eq!(DEFAULT_KEEP_TURNS, 2);
}

#[test]
fn plan_never_cuts_inside_a_turn() {
    let dir = TempDir::new("plan-atomic");
    let store = seeded(&dir, 4);
    let events = read_all(&store);

    // Asking to keep no turns at all would put the cut on the newest turn, which is
    // the live tail; 0 therefore means the default window. Requesting one turn
    // moves the cut earlier, and it must still land on a turn/end.
    let plan = plan_compaction(&events, 1).expect("plan");
    let ends: Vec<u64> = events
        .iter()
        .filter(|e| e.typ == "turn/end")
        .map(|e| e.seq)
        .collect();
    assert!(
        ends.contains(&plan.to),
        "cut {} is not a turn boundary",
        plan.to
    );
}

#[test]
fn plan_keeps_a_tool_call_with_its_result() {
    let dir = TempDir::new("plan-toolpair");
    let store = Store::open(dir.join("events.jsonl"), Options::default()).expect("open store");
    append(&store, "session/start", 0, 0, json!({"cwd": "/work"}));
    // Turn 1 issues a tool call whose result lands in turn 3, so a candidate cut at
    // the end of turn 2 would separate the call from its result.
    append(&store, "turn/start", 1, 0, json!({"number": 1}));
    append(
        &store,
        "user/message",
        1,
        0,
        json!({"text": "count the files"}),
    );
    append(
        &store,
        "tool/call",
        1,
        1,
        json!({"callId": "c1", "tool": "shell"}),
    );
    append(
        &store,
        "turn/end",
        1,
        1,
        json!({"reason": "completed", "steps": 1}),
    );
    append_turn(&store, 2);
    append(&store, "turn/start", 3, 0, json!({"number": 3}));
    append(
        &store,
        "tool/result",
        3,
        1,
        json!({"callId": "c1", "output": "3"}),
    );
    append(&store, "assistant/message", 3, 1, json!({"text": "three"}));
    append(
        &store,
        "turn/end",
        3,
        1,
        json!({"reason": "completed", "steps": 1}),
    );

    let events = read_all(&store);
    let call_seq = events
        .iter()
        .find(|e| e.typ == "tool/call")
        .map(|e| e.seq)
        .expect("call");
    let result_seq = events
        .iter()
        .find(|e| e.typ == "tool/result")
        .map(|e| e.seq)
        .expect("result");

    // Keeping one turn puts the only candidate cut at the end of turn 2 — inside
    // the pair. Retreating lands inside turn 1, so no range holds a whole turn and
    // the plan is empty: a tool pair can glue the entire history to the kept tail.
    let plan = plan_compaction(&events, 1).expect("plan");
    assert!(
        plan.is_empty(),
        "no legal cut exists: pair {}..{result_seq}, plan {}..{}",
        call_seq,
        plan.from,
        plan.to
    );

    // With more turns after the pair, the cut moves past the result and the pair is
    // compacted as one unit — never half of it.
    for turn in 4..=6 {
        append_turn(&store, turn);
    }
    let events = read_all(&store);
    let plan = plan_compaction(&events, 1).expect("plan");
    assert!(!plan.is_empty(), "turns 4-6 make room");
    let call_seq = events
        .iter()
        .find(|e| e.typ == "tool/call")
        .map(|e| e.seq)
        .expect("call");
    let result_seq = events
        .iter()
        .find(|e| e.typ == "tool/result")
        .map(|e| e.seq)
        .expect("result");
    assert!(
        plan.from <= call_seq && result_seq <= plan.to,
        "the pair must be wholly inside the plan: pair {call_seq}..{result_seq}, plan {}..{}",
        plan.from,
        plan.to
    );
}

#[test]
fn plan_is_zero_when_nothing_worth_compacting_is_left() {
    let dir = TempDir::new("plan-empty");
    let store = seeded(&dir, 2);
    // Two turns, both inside the default window.
    assert!(plan_compaction(&read_all(&store), 0)
        .expect("plan")
        .is_empty());

    // An empty log is a zero plan, not an error.
    let empty: Vec<Event> = Vec::new();
    assert!(plan_compaction(&empty, 0).expect("plan").is_empty());
}

#[test]
fn plan_rejects_a_log_whose_seqs_do_not_ascend() {
    let mut first = Event::new("s1", EventType::TURN_START, Some(json!({"number": 1})));
    first.seq = 2;
    let mut second = Event::new(
        "s1",
        EventType::TURN_END,
        Some(json!({"reason": "completed"})),
    );
    second.seq = 1;

    let err = plan_compaction(&[first, second], 0).expect_err("out of order");
    assert!(is_invalid_log(&err), "{}", err.message());

    let mut zero = Event::new("s1", EventType::TURN_START, Some(json!({"number": 1})));
    zero.seq = 0;
    let err = plan_compaction(&[zero], 0).expect_err("zero seq");
    assert!(is_invalid_log(&err), "{}", err.message());
}

#[test]
fn compact_appends_exactly_one_event_naming_the_range() {
    let dir = TempDir::new("compact-append");
    let store = seeded(&dir, 5);
    let before = read_all(&store);
    let plan = plan_compaction(&before, 0).expect("plan");

    compact(&store, &plan, "the story so far", Some(1_763_000_000_000)).expect("compact");

    let after = read_all(&store);
    assert_eq!(
        after.len(),
        before.len() + 1,
        "exactly one record is appended"
    );
    let last = after.last().expect("last record");
    assert_eq!(last.typ, "compaction/summary");
    assert_eq!(last.ts, 1_763_000_000_000, "the caller's clock is used");
    let payload = last.payload.as_ref().expect("payload");
    assert_eq!(payload["summary"], json!("the story so far"));
    assert_eq!(payload["replaces"]["from"], json!(plan.from));
    assert_eq!(payload["replaces"]["to"], json!(plan.to));
    // The marker carries the session the log already knew.
    assert_eq!(last.session, "s1");
}

#[test]
fn compact_uses_the_wall_clock_when_given_none() {
    let dir = TempDir::new("compact-wall-clock");
    let store = seeded(&dir, 5);
    let plan = plan_compaction(&read_all(&store), 0).expect("plan");

    compact(&store, &plan, "summary", None).expect("compact");

    let mut after = read_all(&store);
    let last = after.pop().expect("appended record");
    assert!(last.ts > 0, "the store filled in a real timestamp");
}

#[test]
fn compact_refuses_to_replace_a_range_twice() {
    let dir = TempDir::new("compact-overlap");
    let store = seeded(&dir, 5);
    let events = read_all(&store);
    let plan = plan_compaction(&events, 0).expect("plan");
    compact(&store, &plan, "first summary", None).expect("first compact");

    let before = read_all(&store);
    let err = compact(&store, &plan, "second summary", None).expect_err("overlap refused");
    assert!(is_already_compacted(&err), "{}", err.message());
    assert_eq!(read_all(&store).len(), before.len(), "nothing was appended");
}

#[test]
fn compact_validates_the_plan_before_writing() {
    let dir = TempDir::new("compact-validate");
    let store = seeded(&dir, 5);
    let events = read_all(&store);
    let plan = plan_compaction(&events, 0).expect("plan");
    let before = read_all(&store);

    let zero_from = forge_session::CompactPlan {
        from: 0,
        to: plan.to,
        reason: String::new(),
    };
    assert!(is_invalid_plan(
        &compact(&store, &zero_from, "text", None).expect_err("from=0")
    ));

    let inverted = forge_session::CompactPlan {
        from: plan.to,
        to: plan.from,
        reason: String::new(),
    };
    assert!(is_invalid_plan(
        &compact(&store, &inverted, "text", None).expect_err("to<from")
    ));

    let past_end = forge_session::CompactPlan {
        from: 1,
        to: 99_999,
        reason: String::new(),
    };
    assert!(is_invalid_plan(
        &compact(&store, &past_end, "text", None).expect_err("past last seq")
    ));

    let empty_text = forge_session::CompactPlan {
        from: 1,
        to: plan.to,
        reason: String::new(),
    };
    assert!(is_empty_summary(
        &compact(&store, &empty_text, "   \n\t ", None).expect_err("blank summary")
    ));

    assert_eq!(
        read_all(&store).len(),
        before.len(),
        "refusals write nothing"
    );
}

#[test]
fn compact_is_pure_append_only() {
    let dir = TempDir::new("compact-append-only");
    let store = seeded(&dir, 5);
    let path = store.path();
    let before_bytes = fs::read(&path).expect("read log");
    let plan = plan_compaction(&read_all(&store), 0).expect("plan");

    compact(&store, &plan, "summary", None).expect("compact");

    let after_bytes = fs::read(&path).expect("read log");
    assert!(after_bytes.len() > before_bytes.len());
    assert_eq!(
        &after_bytes[..before_bytes.len()],
        &before_bytes[..],
        "existing bytes are untouched; compaction only appends"
    );
}

#[test]
fn projection_replaces_the_range_with_one_summary_message() {
    let dir = TempDir::new("compact-projection");
    let store = seeded(&dir, 5);
    let before = Projection::project(&read_all(&store)).expect("project");
    let plan = plan_compaction(&read_all(&store), 0).expect("plan");

    compact(&store, &plan, "the story so far", None).expect("compact");

    let after = Projection::project(&read_all(&store)).expect("project");
    assert_eq!(after.messages.len(), 1, "earlier messages are hidden");
    assert_eq!(after.messages[0].kind, MESSAGE_SUMMARY);
    assert_eq!(after.messages[0].text, "the story so far");
    // Compaction hides history; it does not deny the work happened.
    assert_eq!(
        after.counters.turns, before.counters.turns,
        "counters describe the session, not the visible window"
    );
}

#[test]
fn compact_log_is_idempotent_and_respects_the_floor() {
    let dir = TempDir::new("compact-log");
    let store = seeded(&dir, 5);
    let summarizer = TruncatingSummarizer::default();

    // Below the floor nothing is planned, so no provider work and no record.
    let wrote = compact_log(&store, &summarizer, 0, 1_000).expect("compact log");
    assert!(!wrote, "a log under min_events is left alone");

    let wrote = compact_log(&store, &summarizer, 0, 1).expect("compact log");
    assert!(wrote, "a long enough log is compacted");

    // The second call finds the range already summarized: a zero plan, not an
    // error, so an idle turn never reports a broken compaction.
    let wrote = compact_log(&store, &summarizer, 0, 1).expect("compact log");
    assert!(!wrote, "the second call has nothing left to fold");

    let span = last_compacted_span(&store).expect("a summary exists");
    assert_eq!(span.0, 1, "the span starts at the first compactable seq");
}

#[test]
fn last_compacted_span_reports_the_newest_range() {
    let dir = TempDir::new("compact-span");
    let store = seeded(&dir, 9);
    assert_eq!(last_compacted_span(&store), None, "no summary yet");

    let summarizer = TruncatingSummarizer::default();
    assert!(compact_log(&store, &summarizer, 0, 1).expect("first"));
    let first = last_compacted_span(&store).expect("first span");
    // A second compaction needs new history to fold: the turns kept verbatim by the
    // first pass are, by construction, the ones the planner refused to touch.
    for turn in 10..=12 {
        append_turn(&store, turn);
    }
    assert!(compact_log(&store, &summarizer, 0, 1).expect("second"));
    let second = last_compacted_span(&store).expect("second span");
    assert!(
        second.1 > first.1,
        "the newest summary covers more history: {second:?} vs {first:?}"
    );
    assert_eq!(
        second.0,
        first.1 + 1 + 9,
        "the second range starts after the first summary marker and the turns it kept: \
         first {first:?}, second {second:?}"
    );
}

#[test]
fn truncating_summarizer_is_deterministic_and_clipped() {
    let dir = TempDir::new("compact-truncating");
    let store = seeded(&dir, 3);
    let events = read_all(&store);
    let summarizer = TruncatingSummarizer::default();

    let first = summarizer.summarize(&events).expect("summarize");
    let second = summarizer.summarize(&events).expect("summarize");
    assert_eq!(
        first, second,
        "no clock, no randomness: same input, same text"
    );
    assert!(
        first.contains("turn/start"),
        "each event contributes its type"
    );
    assert!(first.contains("question 1"), "and a one-line excerpt");

    let tiny = TruncatingSummarizer::new(40);
    let clipped = tiny.summarize(&events).expect("summarize");
    assert!(
        clipped.len() <= 40,
        "clipped to the budget: {}",
        clipped.len()
    );
    assert!(
        clipped.ends_with('…'),
        "a clipped summary says so: {clipped:?}"
    );

    let empty = TruncatingSummarizer::new(1)
        .summarize(&[])
        .expect("summarize");
    assert!(empty.is_empty());
    assert_eq!(DEFAULT_SUMMARY_BYTES, 2048);
}

/// A summarizer that records what it was asked to fold, so a test can prove the
/// caller hands over exactly the planned range.
struct RecordingSummarizer {
    seen: std::sync::Mutex<Vec<(u64, u64)>>,
}

impl Summarizer for RecordingSummarizer {
    fn summarize(&self, events: &[Event]) -> forge_core::Result<String> {
        let from = events.first().map(|e| e.seq).unwrap_or(0);
        let to = events.last().map(|e| e.seq).unwrap_or(0);
        self.seen.lock().expect("lock").push((from, to));
        Ok("recorded".to_string())
    }
}

#[test]
fn compact_log_folds_exactly_the_planned_range() {
    let dir = TempDir::new("compact-range");
    let store = seeded(&dir, 5);
    let events = read_all(&store);
    let plan = plan_compaction(&events, 0).expect("plan");
    let recorder = RecordingSummarizer {
        seen: std::sync::Mutex::new(Vec::new()),
    };

    assert!(compact_log(&store, &recorder, 0, 1).expect("compact log"));
    let seen = recorder.seen.lock().expect("lock").clone();
    assert_eq!(
        seen,
        vec![(plan.from, plan.to)],
        "the summarizer saw [{}, {}]",
        plan.from,
        plan.to
    );
}
