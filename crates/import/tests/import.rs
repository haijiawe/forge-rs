//! Port of forge-go internal/import/{detect,mapping,write}_test.go.
//!
//! Every fixture lives under `tests/fixtures`: no test reads the Go tree at
//! runtime. The tests go through the public API and reparse the bytes `write`
//! commits, so they assert what a reader actually sees.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use forge_core::event::parse_log;
use forge_core::json::canonical;
use forge_core::{Event, EventType};
use forge_import::error::{
    is_bad_header, is_bad_log, is_compressed, is_exists, is_no_dir, is_no_session, is_plan_stale,
    is_unsupported_version, ErrCompressed, ErrExists, ErrNoDir, ERR_COMPRESSED, ERR_EXISTS,
    ERR_NO_DIR,
};
use forge_import::{detect, make_plan, write, Source};
use forge_session::projection::{
    CALL_ERROR, CALL_OK, MESSAGE_ASSISTANT, MESSAGE_USER, TURN_COMPLETE,
};
use forge_session::Projection;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch directory removed on drop. The workspace does not carry
/// `tempfile`, so tests make their own with a process-unique name.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "forge-import-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create scratch dir");
        Scratch(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture(name: &str) -> PathBuf {
    fixtures().join(name)
}

fn copy_file(src: &Path, dst: &Path) {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::copy(src, dst).expect("copy fixture");
}

fn put(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, body).expect("write test file");
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read test file")
}

fn has_warning(warnings: &[String], needle: &str) -> bool {
    warnings.iter().any(|warning| warning.contains(needle))
}

/// Write a minimal old log into a scratch dir and return it. Keeps the tests
/// honest about the physical envelope: header first, then {"type","data"}.
fn synthetic_log(version: i64, rows: &[&str]) -> Scratch {
    let dir = Scratch::new("synthetic");
    let name = if version == 0 {
        "session.jsonl".to_string()
    } else {
        format!("session.v{version}.jsonl")
    };
    let header = format!(
        r#"{{"type":"session","version":{version},"id":"synthetic","createdAt":1000,"isSeeded":false,"delegationDepth":0}}"#
    );
    let mut body = header;
    for row in rows {
        body.push('\n');
        body.push_str(row);
    }
    body.push('\n');
    put(&dir.path().join(name), &body);
    dir
}

fn compile_via_write(dir: &Path, session_id: &str) -> Vec<Event> {
    let src = detect(dir).expect("detect");
    let mut plan = make_plan(&src).expect("make plan");
    plan.session_id = session_id.to_owned();
    let data = Scratch::new("compile");
    let written = write(data.path(), &src, &plan).expect("write");
    let log = data
        .path()
        .join("sessions")
        .join(format!("{session_id}.jsonl"));
    assert_eq!(
        written,
        log.display().to_string(),
        "write must answer with the path it wrote"
    );
    parse_log(&read(&log)).expect("reparse imported log")
}

fn compile_fixture(name: &str) -> Vec<Event> {
    compile_via_write(&fixture(name), "session-under-test")
}

fn event_of_type(events: &[Event], typ: EventType) -> &Event {
    events
        .iter()
        .find(|event| event.typ == typ.as_str())
        .unwrap_or_else(|| panic!("no {typ} event was mapped"))
}

fn payload_of(event: &Event) -> BTreeMap<String, serde_json::Value> {
    match &event.payload {
        Some(serde_json::Value::Object(map)) => {
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        }
        other => panic!("event has no object payload: {other:?}"),
    }
}

fn names_any(reason: &str, types: &[&str]) -> bool {
    types.iter().any(|typ| reason.contains(typ))
}

fn render_events(events: &[Event]) -> String {
    let mut out = String::new();
    for event in events {
        out.push_str(&canonical(&event).expect("canonical event"));
        out.push('\n');
    }
    out
}

fn import_fixture(name: &str) -> (Scratch, PathBuf, Vec<Event>) {
    let src = detect(&fixture(name)).expect("detect fixture");
    let plan = make_plan(&src).expect("make plan");
    let data = Scratch::new("import");
    let written = write(data.path(), &src, &plan).expect("write fixture");
    let log = data
        .path()
        .join("sessions")
        .join(format!("{}.jsonl", plan.session_id));
    assert_eq!(written, log.display().to_string());
    let events = parse_log(&read(&log)).expect("parse imported log");
    (data, log, events)
}

/// Hash names, sizes, mtimes, modes and content of every path under `dir`, so
/// any write — including touching an existing file — changes the digest.
fn tree_fingerprint(dir: &Path) -> BTreeMap<PathBuf, (u64, u128, bool, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::symlink_metadata(&path).expect("stat source path");
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let is_dir = meta.is_dir();
        let content = if is_dir {
            for entry in fs::read_dir(&path).expect("read source dir") {
                stack.push(entry.expect("dir entry").path());
            }
            Vec::new()
        } else {
            fs::read(&path).unwrap_or_default()
        };
        let rel = path
            .strip_prefix(dir)
            .expect("relative source path")
            .to_path_buf();
        out.insert(rel, (meta.len(), mtime, is_dir, content));
    }
    out
}

// ---- detect ---------------------------------------------------------------

#[test]
fn detect_reports_the_selected_generation() {
    let cases = [
        ("v0-text", "forge-session-v0"),
        ("v0-tool", "forge-session-v0"),
        ("v4-tool", "forge-session-v4"),
        ("v4-approval", "forge-session-v4"),
        ("v4-compaction", "forge-session-v4"),
        ("multi-gen", "forge-session-v4"),
    ];
    for (name, format) in cases {
        let dir = fixture(name);
        let src = detect(&dir).expect("detect fixture");
        assert_eq!(src.format, format, "fixture {name}");
        assert_eq!(src.dir, dir, "detect must echo the caller's path");
        assert_eq!(src.sessions, 1, "fixture {name}");
        assert!(src.events > 0, "fixture {name} has no event rows");
    }
}

#[test]
fn detect_writes_nothing() {
    for name in [
        "v0-text",
        "v0-tool",
        "v4-tool",
        "v4-approval",
        "v4-compaction",
        "multi-gen",
    ] {
        let dir = fixture(name);
        let before = tree_fingerprint(&dir);
        detect(&dir).expect("first detect");
        detect(&dir).expect("second detect");
        assert_eq!(before, tree_fingerprint(&dir), "detect changed {name}");
    }
}

#[test]
fn detect_finds_session_under_the_production_layout() {
    let root = Scratch::new("layout");
    let dir = root
        .path()
        .join("--D--work-demo--/~002E~002Esession~002Eid");
    copy_file(
        &fixture("v4-tool").join("session.v4.jsonl"),
        &dir.join("session.v4.jsonl"),
    );
    let src = detect(root.path()).expect("detect nested session");
    assert_eq!(src.format, "forge-session-v4");
    assert_eq!(src.sessions, 1);
    assert!(src.events > 0);
}

#[test]
fn detect_prefers_the_newest_generation_and_says_so() {
    let src = detect(&fixture("multi-gen")).expect("detect multi-gen");
    assert_eq!(src.format, "forge-session-v4");
    assert!(
        has_warning(&src.warnings, "committed generations"),
        "{:?}",
        src.warnings
    );
    assert!(has_warning(&src.warnings, "v2,v3,v4"), "{:?}", src.warnings);
}

#[test]
fn detect_refuses_a_compressed_newest_generation() {
    let dir = Scratch::new("compressed");
    put(&dir.path().join("session.v4.jsonl.zstd"), "not really zstd");
    let err = detect(dir.path()).expect_err("compressed log must be refused");
    assert!(is_compressed(&err), "{err:?}");
}

#[test]
fn detect_ignores_compressed_siblings_and_warns() {
    let dir = Scratch::new("sibling");
    copy_file(
        &fixture("v4-tool").join("session.v4.jsonl"),
        &dir.path().join("session.v4.jsonl"),
    );
    put(&dir.path().join("session.v3.jsonl.zstd"), "not really zstd");
    let src = detect(dir.path()).expect("detect with compressed sibling");
    assert!(has_warning(&src.warnings, "zstd"), "{:?}", src.warnings);
}

#[test]
fn detect_recovers_a_torn_tail() {
    let dir = Scratch::new("torn");
    let body = read(&fixture("v0-text").join("session.jsonl"));
    put(
        &dir.path().join("session.jsonl"),
        body.trim_end_matches('\n'),
    );
    let src = detect(dir.path()).expect("detect torn tail");
    assert!(has_warning(&src.warnings, "torn"), "{:?}", src.warnings);
    assert!(src.events > 0, "torn tail lost every row");
}

#[test]
fn detect_errors() {
    // missing directory -> ErrNoDir with a user-facing, package-free message
    let missing = Scratch::new("missing").path().join("does-not-exist");
    let err = detect(&missing).expect_err("missing dir must fail");
    assert!(
        is_no_dir(&err),
        "missing dir did not produce ErrNoDir: {err:?}"
    );
    let message = err.message();
    assert_eq!(message, format!("{}: {}", ERR_NO_DIR, missing.display()));
    assert!(
        !message.contains("forge_v2") && !message.contains("internal::import"),
        "error leaked internal package names: {message}"
    );
    // sentinel aliases required by the contract
    assert_eq!(ErrNoDir, ERR_NO_DIR);
    assert_eq!(ErrExists, ERR_EXISTS);
    assert_eq!(ErrCompressed, ERR_COMPRESSED);
    // no session
    let empty = Scratch::new("empty");
    let err = detect(empty.path()).expect_err("empty dir must fail");
    assert!(
        is_no_session(&err) || err.message().contains("no old-format"),
        "{err:?}"
    );
    // not a directory
    let dir = Scratch::new("notdir");
    let file = dir.path().join("session.jsonl");
    put(&file, "{}\n");
    assert!(
        detect(&file).is_err(),
        "detect accepted a file as a directory"
    );
    // bad header
    let dir = Scratch::new("badheader");
    put(
        &dir.path().join("session.jsonl"),
        "{\"type\":\"other\",\"version\":4}\n",
    );
    assert!(is_bad_header(&detect(dir.path()).expect_err("bad header")));
    // header without version
    let dir = Scratch::new("noversion");
    put(
        &dir.path().join("session.jsonl"),
        "{\"type\":\"session\",\"id\":\"s\",\"createdAt\":1}\n",
    );
    assert!(is_bad_header(&detect(dir.path()).expect_err("no version")));
    // newer generation than this build speaks
    let dir = Scratch::new("v9");
    put(
        &dir.path().join("session.v9.jsonl"),
        "{\"type\":\"session\",\"version\":9,\"id\":\"s\",\"createdAt\":1}\n",
    );
    assert!(is_unsupported_version(
        &detect(dir.path()).expect_err("v9 header")
    ));
    // corrupt event row
    let dir = Scratch::new("corrupt");
    put(
        &dir.path().join("session.v4.jsonl"),
        "{\"type\":\"session\",\"version\":4,\"id\":\"s\",\"createdAt\":1}\n{\"type\":\"user/message\",\"data\":\n",
    );
    assert!(is_bad_log(&detect(dir.path()).expect_err("corrupt row")));
}

// ---- mapping --------------------------------------------------------------

#[test]
fn make_plan_counts_what_an_import_would_write() {
    let cases = [
        ("v0-text", 9),
        ("v0-tool", 14),
        ("v4-tool", 14),
        ("v4-approval", 16),
        ("v4-compaction", 18),
    ];
    for (name, count) in cases {
        let src = detect(&fixture(name)).expect("detect");
        let plan = make_plan(&src).expect("plan");
        assert_eq!(plan.events, count, "fixture {name}");
        assert!(
            !plan.dropped.is_empty(),
            "fixture {name} has no dropped reasons"
        );
        assert!(!plan.session_id.is_empty(), "plan has no session id");
    }
}

#[test]
fn every_dropped_reason_names_the_old_record() {
    let old_types = [
        "permission/preset",
        "sandbox/mode",
        "approval/policy",
        "agent/inbox/spliced",
        "session/title",
        "request/header",
        "request/context",
        "assistant/chunk",
        "reasoning-chunks",
        "text-chunks",
        "tool-call-chunks",
        "system/message",
        "job/registered",
        "job/settled",
        "compaction/start",
        "compaction/end",
        "image/offload",
        "hook/invoked",
        "hook/result",
        "approval/decided",
        "compaction/summary",
        "user/message",
    ];
    for name in [
        "v0-text",
        "v0-tool",
        "v4-tool",
        "v4-approval",
        "v4-compaction",
    ] {
        let src = detect(&fixture(name)).expect("detect");
        let plan = make_plan(&src).expect("plan");
        assert!(
            !plan.dropped.is_empty(),
            "fixture {name} has no dropped reasons"
        );
        for reason in &plan.dropped {
            assert!(
                names_any(reason, &old_types),
                "fixture {name}: dropped reason {reason:?} names no old record type"
            );
        }
    }
}

#[test]
fn unknown_record_type_is_dropped_not_fatal() {
    let dir = synthetic_log(
        4,
        &[
            r#"{"type":"user/message","data":{"content":[{"type":"text","text":"hi"}],"source":{"kind":"user"},"role":"user","id":"m1"}}"#,
            r#"{"type":"future/thing","data":{"anything":true}}"#,
        ],
    );
    let src = detect(dir.path()).expect("detect");
    let plan = make_plan(&src).expect("plan");
    let events = compile_via_write(dir.path(), "unknown-type");
    assert!(!events.is_empty(), "unknown type aborted the mapping");
    assert!(
        plan.dropped
            .iter()
            .any(|reason| names_any(reason, &["future/thing"])),
        "dropped = {:?}, want a reason naming future/thing",
        plan.dropped
    );
}

#[test]
fn sections_only_user_message_is_dropped_once() {
    let dir = synthetic_log(
        4,
        &[
            r#"{"type":"user/message","data":{"source":{"kind":"plugin","form":"snapshot"},"sections":[{"name":"sandbox:policy","text":"danger-full-access"}],"role":"user","id":"m1"}}"#,
        ],
    );
    let src = detect(dir.path()).expect("detect");
    let plan = make_plan(&src).expect("plan");
    let events = compile_via_write(dir.path(), "sections-only");
    assert!(
        !events
            .iter()
            .any(|event| event.typ == EventType::USER_MESSAGE.as_str()),
        "a dropped record produced a user/message event"
    );
    assert!(
        plan.dropped
            .iter()
            .any(|reason| names_any(reason, &["user/message"])),
        "dropped = {:?}, want a reason naming user/message",
        plan.dropped
    );
}

#[test]
fn unknown_fields_are_ignored() {
    let plain = synthetic_log(
        4,
        &[
            r#"{"type":"user/message","data":{"content":[{"type":"text","text":"hi"}],"source":{"kind":"user"},"role":"user","id":"m1"}}"#,
        ],
    );
    let with_extras = synthetic_log(
        4,
        &[
            r#"{"type":"user/message","data":{"content":[{"type":"text","text":"hi","futureBlockField":1}],"source":{"kind":"user"},"role":"user","id":"m1","futureDataField":"x"},"futureEnvelopeField":true}"#,
        ],
    );
    let events = compile_via_write(plain.path(), "unknown-fields");
    let message = event_of_type(&events, EventType::USER_MESSAGE);
    let payload = payload_of(message);
    assert_eq!(payload.get("text").and_then(|v| v.as_str()), Some("hi"));
    assert!(
        !payload.contains_key("futureDataField"),
        "unknown field leaked"
    );
    assert_eq!(
        render_events(&compile_via_write(with_extras.path(), "unknown-fields")),
        render_events(&events),
        "future members changed the mapping"
    );
}

#[test]
fn compile_is_deterministic() {
    for name in ["v0-text", "v4-tool", "v4-approval"] {
        let dir = fixture(name);
        let first = compile_via_write(&dir, "session-under-test");
        let second = compile_via_write(&dir, "session-under-test");
        assert_eq!(
            render_events(&first),
            render_events(&second),
            "fixture {name}"
        );
        for (index, event) in first.iter().enumerate() {
            assert_eq!(event.seq, index as u64 + 1, "fixture {name} seq {index}");
            assert_eq!(event.id, second[index].id, "fixture {name} id {index}");
        }
    }
}

#[test]
fn session_start_carries_the_old_facts() {
    for name in ["v0-text", "v4-tool"] {
        let events = compile_fixture(name);
        let payload = payload_of(event_of_type(&events, EventType::SESSION_START));
        for key in ["workspace", "model", "startedBy", "system", "systemBytes"] {
            assert!(
                payload.contains_key(key),
                "fixture {name} session/start has no {key}"
            );
        }
        assert_eq!(
            payload.get("model").and_then(|v| v.as_str()),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            payload.get("startedBy").and_then(|v| v.as_str()),
            Some("import")
        );
        assert_eq!(
            payload.get("workspace").and_then(|v| v.as_str()),
            Some("{{cwd}}")
        );
    }
}

#[test]
fn tool_result_both_generations() {
    let cases = [
        (
            "v0-tool",
            false,
            "Error: bash is disabled by policy in this session",
        ),
        ("v4-tool", true, "SNAPSHOT_OK\n"),
    ];
    for (name, ok, output) in cases {
        let events = compile_fixture(name);
        let result = payload_of(event_of_type(&events, EventType::TOOL_RESULT));
        assert_eq!(
            result.get("ok").and_then(|v| v.as_bool()),
            Some(ok),
            "fixture {name} ok"
        );
        assert_eq!(
            result.get("output").and_then(|v| v.as_str()),
            Some(output),
            "fixture {name} output"
        );
        assert_eq!(
            result.get("tool").and_then(|v| v.as_str()),
            Some("bash"),
            "fixture {name} tool"
        );
        let call = payload_of(event_of_type(&events, EventType::TOOL_CALL));
        assert_eq!(
            result.get("callId"),
            call.get("callId"),
            "fixture {name} callId mismatch"
        );
    }
}

#[test]
fn tool_call_arguments_are_decoded_from_the_json_string() {
    let events = compile_fixture("v4-tool");
    let payload = payload_of(event_of_type(&events, EventType::TOOL_CALL));
    assert_eq!(payload.get("tool").and_then(|v| v.as_str()), Some("bash"));
    let args = payload
        .get("args")
        .and_then(|v| v.as_object())
        .expect("args object");
    assert_eq!(
        args.get("command").and_then(|v| v.as_str()),
        Some("echo SNAPSHOT_OK")
    );
}

#[test]
fn step_end_reason_is_derived_from_the_assistant_message() {
    let events = compile_fixture("v4-tool");
    let want = BTreeMap::from([(0, "tool_use"), (1, "end_turn")]);
    let mut seen = 0;
    for event in &events {
        if event.typ != EventType::STEP_END.as_str() {
            continue;
        }
        let payload = payload_of(event);
        let index = payload
            .get("index")
            .and_then(|v| v.as_u64())
            .expect("step index") as usize;
        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .expect("step reason");
        assert_eq!(reason, want[&index], "step {index}");
        seen += 1;
    }
    assert_eq!(seen, want.len(), "step/end count");
}

#[test]
fn turn_end_reason_translation() {
    let cases: &[(&str, &str, &str, bool)] = &[
        (r#"{"kind":"completed"}"#, "completed", "", false),
        (r#"{"kind":"aborted"}"#, "cancelled", "aborted", false),
        (
            r#"{"kind":"interrupted"}"#,
            "cancelled",
            "interrupted",
            false,
        ),
        (r#"{"kind":"forked"}"#, "cancelled", "forked", false),
        (r#"{"kind":"blocked"}"#, "cancelled", "blocked", false),
        (
            r#"{"kind":"aborted","reason":{"kind":"user"}}"#,
            "cancelled",
            "user",
            true,
        ),
        (r#"{"kind":"error"}"#, "error", "", false),
        (r#"{"kind":"max-tokens"}"#, "error", "", false),
        (r#"{"kind":"something-new"}"#, "error", "", true),
        ("{}", "error", "", true),
        (r#""completed""#, "completed", "", false),
    ];
    for (reason, want, cancelled, noted) in cases {
        let row = format!(r#"{{"type":"turn/end","data":{{"turn":1,"reason":{reason}}}}}"#);
        let dir = synthetic_log(4, &[&row]);
        let src = detect(dir.path()).expect("detect");
        let plan = make_plan(&src).expect("plan");
        let events = compile_via_write(dir.path(), "turn-end");
        let payload = payload_of(event_of_type(&events, EventType::TURN_END));
        assert_eq!(
            payload.get("reason").and_then(|v| v.as_str()),
            Some(*want),
            "reason {reason}"
        );
        assert_eq!(
            payload
                .get("cancelledBy")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            *cancelled,
            "cancelledBy for {reason}"
        );
        assert_eq!(
            plan.dropped.iter().any(|r| r.contains("turn/end")),
            *noted,
            "degradation note for {reason}: {:?}",
            plan.dropped
        );
    }
    // A missing reason is an error, never a guessed success.
    let dir = synthetic_log(4, &[r#"{"type":"turn/end","data":{"turn":1}}"#]);
    let events = compile_via_write(dir.path(), "turn-end");
    let payload = payload_of(event_of_type(&events, EventType::TURN_END));
    assert_eq!(
        payload.get("reason").and_then(|v| v.as_str()),
        Some("error")
    );
}

#[test]
fn approval_mapping() {
    let events = compile_fixture("v4-approval");
    let request = payload_of(event_of_type(&events, EventType::APPROVAL_REQUEST));
    for key in ["approvalId", "callId", "tool", "reason"] {
        assert!(request.contains_key(key), "approval/request has no {key}");
    }
    let decision = payload_of(event_of_type(&events, EventType::APPROVAL_DECISION));
    assert_eq!(
        decision.get("decision").and_then(|v| v.as_str()),
        Some("allow")
    );
    assert_eq!(decision.get("approvalId"), request.get("approvalId"));
    assert!(
        !decision.contains_key("decidedBy"),
        "decidedBy was invented"
    );
}

#[test]
fn approval_decision_rejected_maps_to_deny() {
    let dir = synthetic_log(
        4,
        &[
            r#"{"type":"approval/asked","data":{"id":"a1","toolName":"bash","callId":"c1","reason":"escalate"}}"#,
            r#"{"type":"approval/decided","data":{"id":"a1","outcome":"rejected"}}"#,
        ],
    );
    let events = compile_via_write(dir.path(), "approval-deny");
    let payload = payload_of(event_of_type(&events, EventType::APPROVAL_DECISION));
    assert_eq!(
        payload.get("decision").and_then(|v| v.as_str()),
        Some("deny")
    );
}

#[test]
fn compaction_summary_omits_the_old_seq_range() {
    let events = compile_fixture("v4-compaction");
    let payload = payload_of(event_of_type(&events, EventType::COMPACTION_SUMMARY));
    assert!(
        !payload
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty(),
        "summary text is empty"
    );
    assert!(
        !payload.contains_key("replaces"),
        "old seq range was emitted"
    );
}

#[test]
fn reasoning_text_is_not_folded_into_assistant_text() {
    let events = compile_fixture("v0-text");
    let text_values: Vec<String> = events
        .iter()
        .filter(|event| event.typ == EventType::ASSISTANT_MESSAGE.as_str())
        .filter_map(|event| {
            payload_of(event)
                .get("text")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .collect();
    let texts: Vec<&str> = text_values.iter().map(String::as_str).collect();
    assert_eq!(
        texts,
        vec!["PONG"],
        "reasoning blocks leaked into answer text"
    );
}

#[test]
fn user_message_keeps_text_and_ambient_turn() {
    let events = compile_fixture("v0-text");
    let message = event_of_type(&events, EventType::USER_MESSAGE);
    let payload = payload_of(message);
    assert!(
        payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .starts_with("Reply with exactly the word: PONG"),
        "user text not preserved: {payload:?}"
    );
    assert_eq!(message.turn, 1, "user message is not in the open turn");
}

#[test]
fn truncated_system_prompt_is_flagged() {
    let long = "x".repeat((8 << 10) + 10);
    let row = format!(
        r#"{{"type":"system/message","data":{{"turn":1,"step":1,"message":{{"role":"system","content":[{{"type":"text","text":"{long}"}}],"source":{{"kind":"system-prompt"}},"id":"s1"}}}}}}"#
    );
    let dir = synthetic_log(4, &[&row]);
    let events = compile_via_write(dir.path(), "long-system");
    let payload = payload_of(event_of_type(&events, EventType::SESSION_START));
    assert_eq!(
        payload.get("systemTruncated").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        payload.get("system").and_then(|v| v.as_str()).map(str::len),
        Some(8 << 10)
    );
    assert_eq!(
        payload.get("systemBytes").and_then(|v| v.as_i64()),
        Some(((8 << 10) + 10) as i64)
    );
}

#[test]
fn tool_output_is_clipped_and_flagged() {
    let huge = "y".repeat((64 << 10) + 5);
    let rows = [
        r#"{"type":"tool/call","data":{"turn":1,"step":1,"callId":"c1","name":"bash","arguments":"{}"}}"#.to_string(),
        format!(
            r#"{{"type":"tool/result","data":{{"turn":1,"step":1,"message":{{"role":"tool","source":{{"kind":"tool","callId":"c1"}},"toolCallId":"c1","content":[{{"type":"text","text":"{huge}"}}],"isError":false,"id":"m1"}}}}}}"#
        ),
    ];
    let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
    let dir = synthetic_log(4, &refs);
    let events = compile_via_write(dir.path(), "huge-output");
    let payload = payload_of(event_of_type(&events, EventType::TOOL_RESULT));
    assert_eq!(
        payload.get("truncated").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        payload.get("output").and_then(|v| v.as_str()).map(str::len),
        Some(64 << 10)
    );
}

#[test]
fn make_plan_refuses_an_empty_source_directory() {
    assert!(make_plan(&Source::default()).is_err());
}

#[test]
fn plan_is_stale_when_the_source_changed() {
    let dir = synthetic_log(
        4,
        &[
            r#"{"type":"user/message","data":{"content":[{"type":"text","text":"hi"}],"source":{"kind":"user"},"role":"user","id":"m1"}}"#,
        ],
    );
    let src = detect(dir.path()).expect("detect");
    let mut plan = make_plan(&src).expect("plan");
    plan.events += 1;
    let data = Scratch::new("stale");
    let err = write(data.path(), &src, &plan).expect_err("stale plan must fail");
    assert!(is_plan_stale(&err), "{err:?}");
    assert!(!data
        .path()
        .join("sessions")
        .join(format!("{}.jsonl", plan.session_id))
        .exists());
}

// ---- write ----------------------------------------------------------------

#[test]
fn write_is_consumable_by_the_real_reader() {
    struct Want {
        turns: usize,
        steps: usize,
        messages: usize,
        tool_calls: usize,
        assistant_texts: Vec<&'static str>,
        tool_output: &'static str,
        tool_ok: bool,
        tool_status: &'static str,
        prompt_tokens: i64,
        end_reason: &'static str,
    }
    let cases: &[(&str, Want)] = &[
        (
            "v4-tool",
            Want {
                turns: 1,
                steps: 2,
                messages: 5,
                tool_calls: 1,
                assistant_texts: vec!["", "DONE"],
                tool_output: "SNAPSHOT_OK\n",
                tool_ok: true,
                tool_status: CALL_OK,
                prompt_tokens: 3049,
                end_reason: "completed",
            },
        ),
        (
            "v0-text",
            Want {
                turns: 1,
                steps: 1,
                messages: 3,
                tool_calls: 0,
                assistant_texts: vec!["PONG"],
                tool_output: "",
                tool_ok: false,
                tool_status: "",
                prompt_tokens: 3091,
                end_reason: "completed",
            },
        ),
        (
            "v0-tool",
            Want {
                turns: 1,
                steps: 2,
                messages: 5,
                tool_calls: 1,
                assistant_texts: vec![
                    "",
                    "The tool returned:\n\n> Error: bash is disabled by policy in this session\n\nI cannot run the command because the bash tool is disabled by policy.",
                ],
                tool_output: "Error: bash is disabled by policy in this session",
                tool_ok: false,
                tool_status: CALL_ERROR,
                prompt_tokens: 3045,
                end_reason: "completed",
            },
        ),
    ];
    for (name, want) in cases {
        let (_data, _log, events) = import_fixture(name);
        // Fold with the reader the rest of the workspace uses.
        let snapshot = Projection::project(&events).expect("fold imported log");
        assert_eq!(snapshot.turns.len(), want.turns, "fixture {name} turns");
        assert_eq!(snapshot.counters.steps, want.steps, "fixture {name} steps");
        assert_eq!(
            snapshot.messages.len(),
            want.messages,
            "fixture {name} messages"
        );
        assert_eq!(
            snapshot.tool_calls.len(),
            want.tool_calls,
            "fixture {name} tool calls"
        );
        assert!(
            !snapshot.ended,
            "fixture {name}: the old log never records session/end"
        );
        let turn = &snapshot.turns[0];
        assert_eq!(turn.status, TURN_COMPLETE, "fixture {name} turn status");
        assert_eq!(
            turn.end_reason, want.end_reason,
            "fixture {name} end reason"
        );
        assert_eq!(
            snapshot.counters.prompt_tokens, want.prompt_tokens,
            "fixture {name} prompt tokens"
        );
        let assistant_texts: Vec<&str> = snapshot
            .messages
            .iter()
            .filter(|message| message.kind == MESSAGE_ASSISTANT)
            .map(|message| message.text.as_str())
            .collect();
        assert_eq!(
            assistant_texts, want.assistant_texts,
            "fixture {name} assistant texts"
        );
        if want.tool_calls == 1 {
            let call = &snapshot.tool_calls[0];
            assert_eq!(call.status, want.tool_status, "fixture {name} tool status");
            assert_eq!(
                call.status == CALL_OK,
                want.tool_ok,
                "fixture {name} tool ok flag"
            );
            assert_eq!(call.output, want.tool_output, "fixture {name} tool output");
            assert!(!call.orphan, "fixture {name} tool result did not link back");
            assert_eq!(call.name, "bash", "fixture {name} tool name");
            assert!(
                snapshot
                    .messages
                    .iter()
                    .any(|message| message.kind == MESSAGE_USER),
                "fixture {name} lost its user message"
            );
        }
    }
}

#[test]
fn imported_turn_keeps_the_old_timestamp() {
    let (_data, _log, events) = import_fixture("v0-text");
    let snapshot = Projection::project(&events).expect("fold imported log");
    assert!(!snapshot.messages.is_empty());
    assert!(
        snapshot.messages[0].ts >= 1783600629539,
        "first message ts = {}, want >= the old createdAt",
        snapshot.messages[0].ts
    );
}

#[test]
fn write_refuses_to_overwrite() {
    let src = detect(&fixture("v4-tool")).expect("detect");
    let plan = make_plan(&src).expect("plan");
    let data = Scratch::new("overwrite");
    let written = write(data.path(), &src, &plan).expect("first write");
    let log = data
        .path()
        .join("sessions")
        .join(format!("{}.jsonl", plan.session_id));
    assert_eq!(written, log.display().to_string());
    let before = read(&log);
    let err = write(data.path(), &src, &plan).expect_err("second write must fail");
    assert!(is_exists(&err), "{err:?}");
    assert_eq!(read(&log), before, "the existing log changed");
}

#[test]
fn write_leaves_no_temporary_file() {
    let (_data, log, _events) = import_fixture("v4-tool");
    let sessions = log.parent().expect("sessions dir");
    for entry in fs::read_dir(sessions).expect("read sessions dir") {
        let name = entry.expect("entry").file_name();
        assert!(
            !name.to_string_lossy().ends_with(".tmp"),
            "leftover temp file {name:?}"
        );
    }
}

#[test]
fn write_is_deterministic() {
    let first = import_fixture("v4-tool");
    let second = import_fixture("v4-tool");
    let id_a = first.1.file_stem().unwrap().to_string_lossy().to_string();
    let id_b = second.1.file_stem().unwrap().to_string_lossy().to_string();
    let a = read(&first.1).replace(&id_a, "<session>");
    let b = read(&second.1).replace(&id_b, "<session>");
    assert_eq!(
        a, b,
        "two imports of the same source differ beyond the session id"
    );
}

#[test]
fn write_validates_the_log_it_produces() {
    let (_data, log, events) = import_fixture("v4-approval");
    let body = read(&log);
    assert!(body.ends_with('\n'), "log does not end with an LF");
    assert_eq!(events.len(), body.lines().count());
    let id = log.file_stem().unwrap().to_string_lossy();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1, "line {} seq", index + 1);
        assert_eq!(event.session, id, "line {} session", index + 1);
        event.validate().expect("event validates");
    }
}
