//! Cold resume: a continuable child stays resumable after it settles, driven
//! entirely by its durable log, so a *different* `Runner` (a stand-in for a new
//! process) can append another turn. One-shot children refuse; a hostile id is
//! rejected before any path is built.

mod common;

use std::sync::Arc;

use common::*;
use hearth_agent::Cancel;
use hearth_subagent::{CallContext, Delivery, Mode, Options, Runner};
use hearth_tool::Call;

const FIXED_NOW_MS: i64 = 1_777_863_721_000;

fn runner_with(dir: &TempDir, provider: Arc<ScriptedProvider>) -> Arc<Runner> {
    let mut opt = Options::new(
        provider,
        dir.path(),
        dir.path().join("workspace").to_string_lossy().into_owned(),
        "test-model",
    );
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.clock = Arc::new(|| FIXED_NOW_MS);
    Arc::new(Runner::new(opt).expect("runner"))
}

fn ctx(dir: &TempDir) -> CallContext {
    CallContext {
        cancel: Cancel::default(),
        parent_session_id: String::new(),
        workspace: dir.path().join("workspace").to_string_lossy().into_owned(),
    }
}

fn child_events(dir: &TempDir, id: &str) -> Vec<hearth_core::Event> {
    read_events_at(&dir.sessions().join(format!("{id}.jsonl")))
}

#[test]
fn a_continuable_child_can_be_resumed_by_a_fresh_runner() {
    let dir = TempDir::new("resume-ok");
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("first answer"),
        text_response("second answer"),
    ]));

    let first_runner = runner_with(&dir, provider.clone());
    let first_run = first_runner
        .run_child(
            &ctx(&dir),
            "do the first task",
            "",
            "",
            None,
            Mode::Continuable,
            Some("worker"),
        )
        .expect("first run");
    assert_eq!(first_run.text, "first answer");
    assert_eq!(first_run.mode, Mode::Continuable);

    // A continuable child is never terminal: no session/end, and it carries a
    // continuable descriptor.
    let events = child_events(&dir, &first_run.id);
    assert!(
        last(&events, "session/end").is_none(),
        "a continuable child must stay open"
    );
    let descriptor = first(&events, "subagent/descriptor").expect("descriptor");
    let payload = payload(descriptor);
    assert_eq!(payload["mode"], serde_json::json!("continuable"));

    // A brand-new Runner stands in for a new process; it resumes the same log.
    let second_runner = runner_with(&dir, provider);
    let second = second_runner
        .resume(&first_run.id, "do the second task", "", "")
        .expect("resume");
    assert_eq!(second.text, "second answer");
    assert_eq!(second.turns, 2, "the second turn appended to the same log");

    let after = child_events(&dir, &first_run.id);
    assert_eq!(count(&after, "turn/end"), 2, "two turns in one log");
    assert!(
        last(&after, "session/end").is_none(),
        "still open after a resume"
    );
}

#[test]
fn a_one_shot_child_refuses_to_resume() {
    let dir = TempDir::new("resume-oneshot");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("done")]));
    let runner = runner_with(&dir, provider.clone());
    let out = runner
        .run_child(&ctx(&dir), "task", "", "", None, Mode::OneShot, None)
        .expect("one-shot run");
    // A one-shot child wrote its terminal record.
    let events = child_events(&dir, &out.id);
    assert!(last(&events, "session/end").is_some());

    let err = runner_with(&dir, provider)
        .resume(&out.id, "more work", "", "")
        .expect_err("one-shot cannot resume");
    assert!(
        err.message().contains("one-shot") || err.message().contains("session/end"),
        "unexpected error: {}",
        err.message()
    );
}

#[test]
fn resume_rejects_an_id_that_is_not_a_child_id() {
    let dir = TempDir::new("resume-bad");
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let err = runner_with(&dir, provider)
        .resume("../../etc/passwd", "x", "", "")
        .expect_err("hostile id is refused before any path is built");
    assert!(
        err.message().contains("not a child id"),
        "unexpected error: {}",
        err.message()
    );
}

#[test]
fn the_subagent_tool_resumes_a_continuable_child_by_id() {
    let dir = TempDir::new("resume-tool");
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("first answer"),
        text_response("second answer"),
    ]));
    let runner = runner_with(&dir, provider);
    let tool = runner.tool();

    let first = tool.call(Call::new(
        "c1",
        "subagent",
        serde_json::json!({"prompt": "start the job", "continuable": true}),
        "",
    ));
    assert!(first.is_ok(), "{first:?}");
    assert_eq!(first.output, "first answer");
    let id = first.meta["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    let second = tool.call(Call::new(
        "c2",
        "subagent",
        serde_json::json!({"prompt": "now the second half", "resume": id}),
        "",
    ));
    assert!(second.is_ok(), "{second:?}");
    assert_eq!(second.output, "second answer");
    assert_eq!(second.meta["turns"], serde_json::json!(2));
}

#[test]
fn resuming_an_unknown_child_is_a_reported_failure_not_a_panic() {
    let dir = TempDir::new("resume-missing");
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let runner = runner_with(&dir, provider);
    let res = runner.tool().call(Call::new(
        "c1",
        "subagent",
        serde_json::json!({"prompt": "x", "resume": "20260101-000000-deadbeef"}),
        "",
    ));
    assert!(res.is_error, "expected a failure, got ok: {res:?}");
    // The code names the cause, not just "something went wrong": a `resume`
    // that names a child nobody ever created is `not_resumable`, which is what
    // a caller can act on. `subagent_failed` is reserved for a child that was
    // admitted and then failed while running.
    assert_eq!(
        res.code,
        hearth_subagent::error_code::Code::NotResumable.as_str()
    );
    assert!(
        res.output.contains("no such child session"),
        "unexpected message: {}",
        res.output
    );
}

#[test]
fn interrupting_an_unknown_child_reports_false() {
    // No live child registered under that id, so there is nothing to stop; the
    // control surface must say so rather than claim a stop it did not perform.
    let dir = TempDir::new("interrupt-unknown");
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let runner = runner_with(&dir, provider);
    assert!(!runner.interrupt("no-such-child"), "no live child => false");
}

#[test]
fn a_child_remembers_its_depth_across_a_cold_resume() {
    // The depth of a child is a property of where it was created, written into
    // its own `session/start` as `delegationDepth`. A cold resume in a fresh
    // process (a new Runner whose own depth is 0) must read that header, not
    // start counting from zero — otherwise grandchildren could exceed maxDepth.
    let dir = TempDir::new("depth-cold");
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("first answer"),
        text_response("second answer"),
    ]));
    let runner = runner_with(&dir, provider);
    let first = runner
        .run_child(&ctx(&dir), "go", "", "", None, Mode::Continuable, None)
        .expect("first run");
    // The root runner is at depth 0, so its child is at depth 1.
    assert_eq!(first.depth, 1);

    // The header records it.
    let events = child_events(&dir, &first.id);
    let head = events
        .iter()
        .find(|e| e.typ == hearth_core::EventType::SESSION_START.as_str())
        .and_then(|e| e.payload.clone())
        .expect("session/start payload");
    assert_eq!(head["delegationDepth"], serde_json::json!(1));

    // A resume from the same (depth 0) runner keeps depth at 1, read from the log.
    let second = runner
        .deliver(&first.id, "again", Delivery::Queue)
        .expect("resume");
    assert_eq!(
        second.depth, 1,
        "depth must come from the header, not reset"
    );
}

#[test]
fn deliver_appends_a_turn_like_resume() {
    // A delivery to a continuable child is a turn append (the synchronous
    // model); it must behave exactly like a resume and report the new turn count.
    let dir = TempDir::new("deliver");
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("first answer"),
        text_response("second answer"),
    ]));
    let runner = runner_with(&dir, provider);
    let first = runner
        .run_child(&ctx(&dir), "go", "", "", None, Mode::Continuable, None)
        .expect("first run");
    assert_eq!(first.turns, 1);
    let second = runner
        .deliver(&first.id, "again", Delivery::Queue)
        .expect("deliver");
    assert_eq!(second.turns, 2);
    assert_eq!(second.text, "second answer");
}

/// The asynchronous shape: `spawn` returns immediately and the settlement
/// arrives later as a `next-turn` message on the parent log. Nothing is handed
/// back through memory, so a parent that was restarted in between still finds
/// the notice -- which is the whole point of putting it in the queue.
#[test]
fn a_spawned_child_leaves_its_settlement_in_the_parent_queue() {
    let dir = TempDir::new("spawn-settle");
    std::fs::create_dir_all(dir.sessions()).expect("sessions dir");
    let parent_log = dir.sessions().join("parent.jsonl");
    {
        // Give the parent log a header, the way a real session has one.
        let mut opt = Options::new(
            std::sync::Arc::new(ScriptedProvider::new(Vec::new())),
            dir.path(),
            dir.path().join("workspace").to_string_lossy().into_owned(),
            "test-model".to_string(),
        );
        opt.parent_session_id = "parent".into();
        opt.parent_log = Some(parent_log.clone());
        let runner = std::sync::Arc::new(Runner::new(opt).expect("runner"));
        let ctx = CallContext {
            workspace: dir.path().join("workspace").to_string_lossy().into_owned(),
            parent_session_id: "parent".into(),
            ..CallContext::default()
        };

        let provider = std::sync::Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
            hearth_llm::Delta {
                text: "the child is done".into(),
                stop_reason: Some("stop".into()),
                ..Default::default()
            },
        ])]));
        let mut child_opt = Options::new(
            provider,
            dir.path(),
            dir.path().join("workspace").to_string_lossy().into_owned(),
            "test-model".to_string(),
        );
        child_opt.parent_session_id = "parent".into();
        child_opt.parent_log = Some(parent_log.clone());
        let child_runner = std::sync::Arc::new(Runner::new(child_opt).expect("runner"));

        child_runner
            .spawn(ctx, "do the thing", Some("agent-1".into()))
            .expect("spawn");
        drop(runner);

        // Wait for the background thread to leave its notice. Bounded, because
        // a test that hangs forever is worse than one that fails.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let store =
                hearth_session::Store::open(&parent_log, hearth_session::Options::default())
                    .expect("parent store");
            let mut events = Vec::new();
            store
                .read_from(0, |ev| {
                    events.push(ev.clone());
                    Ok(())
                })
                .expect("read parent log");
            let inbox = hearth_agent::inbox::fold(&events);
            if let Some(msg) = inbox.next_turn.first() {
                assert!(
                    msg.text.contains("the child is done"),
                    "the notice quotes what the child said: {}",
                    msg.text
                );
                assert!(
                    msg.text.contains("agent-1"),
                    "and it names the child: {}",
                    msg.text
                );
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no settlement arrived within the deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// A runner that cannot deliver must say so instead of spawning a thread whose
/// only outcome is a lost result.
#[test]
fn spawn_without_a_parent_log_is_refused() {
    let dir = TempDir::new("spawn-nolog");
    std::fs::create_dir_all(dir.sessions()).expect("sessions dir");
    let mut opt = Options::new(
        std::sync::Arc::new(ScriptedProvider::new(Vec::new())),
        dir.path(),
        dir.path().join("workspace").to_string_lossy().into_owned(),
        "test-model".to_string(),
    );
    opt.parent_session_id = "parent".into();
    let runner = std::sync::Arc::new(Runner::new(opt).expect("runner"));
    let err = runner
        .spawn(CallContext::default(), "anything", None)
        .expect_err("a runner with no parent log cannot deliver");
    assert!(
        err.message().contains("parent log"),
        "the refusal has to name the reason: {}",
        err.message()
    );
}

/// The tool-level shape of a background delegation: the call returns at once
/// with no answer, and the answer shows up later on the parent log as an
/// inbox message. A schema is refused because a background child has nowhere
/// to put a structured result.
#[test]
fn a_background_delegation_answers_later_through_the_queue() {
    let dir = TempDir::new("spawn-tool");
    std::fs::create_dir_all(dir.sessions()).expect("sessions dir");
    let parent_log = dir.sessions().join("parent.jsonl");
    {
        let store = hearth_session::Store::open(&parent_log, hearth_session::Options::default())
            .expect("parent store");
        store
            .append(&hearth_core::Event::new(
                "parent",
                hearth_core::EventType::SESSION_START,
                Some(serde_json::json!({"workspace": "w", "model": "test-model", "startedBy": "cli"})),
            ))
            .expect("append");
    }

    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        hearth_llm::Delta {
            text: "background answer".into(),
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ])]));
    let mut opt = Options::new(
        provider,
        dir.path(),
        dir.path().join("workspace").to_string_lossy().into_owned(),
        "test-model".to_string(),
    );
    opt.parent_session_id = "parent".into();
    opt.parent_log = Some(parent_log.clone());
    let runner = Arc::new(Runner::new(opt).expect("runner"));
    let tool = runner.tool();

    let call = Call::new(
        "c1",
        "subagent",
        serde_json::json!({"prompt": "do it in the background", "background": true}),
        dir.path().join("workspace").to_string_lossy().into_owned(),
    );
    let out = tool.call(call);
    assert!(
        !out.is_error,
        "starting a background child is not a failure: {}",
        out.output
    );
    assert!(
        out.output.contains("background"),
        "the caller is told it is running in the background: {}",
        out.output
    );
    assert!(
        out.meta.get("background").is_some(),
        "the result is marked so a reader can tell it did not wait"
    );

    // A schema has no path back: the answer arrives as prose, not as a tool
    // result, so promising structured output would be a lie.
    let bad = tool.call(Call::new(
        "c2",
        "subagent",
        serde_json::json!({"prompt": "x", "background": true, "schema": {"type": "object"}}),
        dir.path().join("workspace").to_string_lossy().into_owned(),
    ));
    assert!(bad.is_error);
    assert!(bad
        .output
        .contains("background child cannot report a schema"));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let store = hearth_session::Store::open(&parent_log, hearth_session::Options::default())
            .expect("parent store");
        let mut events = Vec::new();
        store
            .read_from(0, |ev| {
                events.push(ev.clone());
                Ok(())
            })
            .expect("read");
        let inbox = hearth_agent::inbox::fold(&events);
        if let Some(msg) = inbox.next_turn.first() {
            assert!(
                msg.text.contains("background answer"),
                "the notice quotes the child: {}",
                msg.text
            );
            return;
        }
        assert!(std::time::Instant::now() < deadline, "no notice arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
