//! Delegation behaviour mirrored from `forge-go/internal/subagent`.
//!
//! Every test uses the scripted provider and a fresh on-disk log; assertions
//! read the log back, because in this crate the log is the only truth.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use common::*;
use hearth_agent::provider_error;
use hearth_llm::Role;
use hearth_subagent::{CallContext, Options, Runner, CODE_DEPTH_EXCEEDED, CODE_FAILED};
use hearth_tool::{Call, CODE_CANCELLED, ERR_BAD_ARGS};
use serde_json::{json, Value};

const FIXED_NOW_MS: i64 = 1_777_863_721_000;

fn make_runner(dir: &TempDir, provider: Arc<ScriptedProvider>) -> Arc<Runner> {
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

fn call_tool(runner: &Arc<Runner>, id: &str, args: Value) -> hearth_tool::Result {
    runner.tool().call(Call::new(id, "subagent", args, ""))
}

fn only_child(dir: &TempDir) -> String {
    let ids = child_ids(dir.path());
    assert_eq!(ids.len(), 1, "exactly one child log: {ids:?}");
    ids[0].clone()
}

fn child_events(dir: &TempDir, id: &str) -> Vec<hearth_core::Event> {
    read_events_at(&dir.sessions().join(format!("{id}.jsonl")))
}

// ---------------------------------------------------------------------------

#[test]
fn successful_delegation_writes_a_fresh_child_log_and_returns_its_text() {
    let dir = TempDir::new("success");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "child says hello",
    )]));
    let runner = make_runner(&dir, provider.clone());

    let res = call_tool(&runner, "call-1", json!({"prompt": "do the task"}));

    assert!(res.is_ok(), "{res:?}");
    assert_eq!(res.output, "child says hello");
    assert_eq!(res.code, "");
    assert_eq!(res.meta["turns"], json!(1));
    assert!(res.meta["steps"].as_u64().unwrap_or(0) > 0);
    assert_eq!(res.meta["model"], json!("test-model"));

    let id = only_child(&dir);
    assert!(id.starts_with("20260504-030201-"), "id: {id}");
    assert_eq!(id.len(), "20260504-030201-".len() + 8);

    let events = child_events(&dir, &id);
    assert_dense(&events, &id);
    assert_eq!(count(&events, "session/start"), 1);
    assert_eq!(count(&events, "session/end"), 1);
    let head = payload(first(&events, "session/start").unwrap());
    assert_eq!(head["startedBy"], json!("subagent"));
    assert_eq!(head["depth"], json!(1));
    assert_eq!(head["model"], json!("test-model"));
    assert_eq!(
        head["workspace"],
        json!(dir.path().join("workspace").to_string_lossy())
    );
    assert!(head.get("parent").is_none(), "no parent header when unset");
    assert_eq!(
        payload(first(&events, "user/message").unwrap())["text"],
        json!("do the task")
    );
    assert_eq!(
        payload(first(&events, "assistant/message").unwrap())["text"],
        json!("child says hello")
    );
    let end = last(&events, "session/end").unwrap();
    assert_eq!(payload(end)["reason"], json!("completed"));
    assert_eq!(payload(end)["turns"], json!(1));

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "test-model");
    assert!(
        requests[0].tools.is_empty(),
        "no parent tools means no tools"
    );
    let last_msg = requests[0].messages.last().unwrap();
    assert_eq!(last_msg.role, Role::User);
    assert_eq!(last_msg.content, "do the task");
}

#[test]
fn parent_from_options_is_recorded_in_the_child_header() {
    let dir = TempDir::new("opt-parent");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("ok")]));
    let mut opt = Options::new(provider, dir.path(), "ws", "test-model");
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.parent_session_id = "parent-123".into();
    let runner = Arc::new(Runner::new(opt).expect("runner"));

    let res = call_tool(&runner, "call-1", json!({"prompt": "task"}));
    assert!(res.is_ok(), "{res:?}");
    let id = only_child(&dir);
    let events = child_events(&dir, &id);
    assert_eq!(
        payload(first(&events, "session/start").unwrap())["parent"],
        json!("parent-123")
    );
}

#[test]
fn run_with_a_context_parent_overrides_options() {
    let dir = TempDir::new("ctx-parent");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("ok")]));
    let mut opt = Options::new(provider, dir.path(), "ws", "test-model");
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.parent_session_id = "options-parent".into();
    let runner = Runner::new(opt).expect("runner");

    let ctx = CallContext {
        cancel: Default::default(),
        parent_session_id: "context-parent".into(),
        workspace: String::new(),
    };
    let out = runner.run(&ctx, "task", "", "").expect("child run");
    let events = child_events(&dir, &out.id);
    assert_eq!(
        payload(first(&events, "session/start").unwrap())["parent"],
        json!("context-parent")
    );
    assert_eq!(out.text, "ok");
}

#[test]
fn final_text_is_the_last_non_empty_assistant_message_not_the_aggregate() {
    let dir = TempDir::new("last-text");
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(vec![
            text("thinking "),
            tool_delta(0, "tc-1", "echo", r#"{"x":1}"#),
            stop("tool_calls"),
        ]),
        text_response("final answer"),
    ]));
    let tools: Arc<dyn hearth_agent::ToolRunner> =
        Arc::new(RecordingTools::new(vec![tool_spec("echo")]));
    let mut opt = Options::new(provider.clone(), dir.path(), "ws", "test-model");
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.tools = Some(tools);
    let runner = Arc::new(Runner::new(opt).expect("runner"));

    let res = call_tool(&runner, "call-1", json!({"prompt": "task"}));

    assert!(res.is_ok(), "{res:?}");
    assert_eq!(res.output, "final answer");
    assert_ne!(res.output, "thinking final answer");
    assert_eq!(res.meta["steps"], json!(2));
    let id = only_child(&dir);
    let events = child_events(&dir, &id);
    assert_eq!(count(&events, "tool/result"), 1);
    assert_eq!(count(&events, "assistant/message"), 2);
    let texts: Vec<_> = events
        .iter()
        .filter(|e| e.typ.as_str() == "assistant/message")
        .map(|e| payload(e)["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(
        texts,
        vec!["thinking ".to_string(), "final answer".to_string()]
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let names: Vec<&str> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"), "{names:?}");
    assert!(names.contains(&"subagent"), "{names:?}");
}

#[test]
fn provider_error_surfaces_as_a_failed_result_with_a_terminal_session_end() {
    let dir = TempDir::new("provider-error");
    let provider = Arc::new(ScriptedProvider::new(vec![Response::StartErr(
        provider_error("boom", "test_error", false, Duration::ZERO),
    )]));
    let runner = make_runner(&dir, provider.clone());

    let res = call_tool(&runner, "call-1", json!({"prompt": "task"}));

    assert!(res.is_error);
    assert_eq!(res.code, CODE_FAILED);
    assert!(res.output.contains("boom"), "{}", res.output);
    let id = only_child(&dir);
    let events = child_events(&dir, &id);
    assert_dense(&events, &id);
    assert_eq!(
        payload(last(&events, "session/end").unwrap())["reason"],
        json!("aborted")
    );
    assert_eq!(
        payload(last(&events, "turn/end").unwrap())["reason"],
        json!("error")
    );
}

#[test]
fn two_delegations_get_independent_logs_and_sequences() {
    let dir = TempDir::new("independent");
    let provider = Arc::new(ScriptedProvider::new(vec![
        text_response("first"),
        text_response("second"),
    ]));
    let runner = make_runner(&dir, provider.clone());

    let a = call_tool(&runner, "call-1", json!({"prompt": "one"}));
    let b = call_tool(&runner, "call-2", json!({"prompt": "two"}));

    assert_eq!(a.output, "first");
    assert_eq!(b.output, "second");
    let ids = child_ids(dir.path());
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_ne!(ids[0], ids[1]);
    for id in &ids {
        let events = child_events(&dir, id);
        assert_eq!(events[0].seq, 1);
        assert_dense(&events, id);
    }
}

#[test]
fn parent_cancel_stops_the_child_and_writes_terminal_events() {
    let dir = TempDir::new("cancel-child");
    let gate = Gate::default();
    let provider = Arc::new(ScriptedProvider::new(vec![Response::BlockThen(
        gate.clone(),
        vec![text("never delivered"), stop("stop")],
    )]));
    let runner = make_runner(&dir, provider.clone());
    let canceller = runner.options().canceller.clone();

    let worker = {
        let runner = Arc::clone(&runner);
        thread::spawn(move || call_tool(&runner, "call-1", json!({"prompt": "task"})))
    };
    wait_until(|| provider.reached() >= 1);
    canceller.cancel_all();
    gate.open();
    let res = worker.join().expect("worker thread");

    assert!(res.is_error);
    assert_eq!(res.code, CODE_CANCELLED);
    assert!(res.output.contains("cancelled"));
    let id = only_child(&dir);
    let events = child_events(&dir, &id);
    assert_eq!(
        payload(last(&events, "turn/end").unwrap())["reason"],
        json!("cancelled")
    );
    assert_eq!(
        payload(last(&events, "session/end").unwrap())["reason"],
        json!("aborted")
    );
}

#[test]
fn cancelled_before_start_never_creates_a_log_or_calls_the_provider() {
    let dir = TempDir::new("cancel-before");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("never")]));
    let runner = make_runner(&dir, provider.clone());
    runner.options().canceller.cancel_all();

    let res = call_tool(&runner, "call-1", json!({"prompt": "task"}));

    assert!(res.is_error);
    assert_eq!(res.code, CODE_CANCELLED);
    assert!(res.output.contains("before the child started"));
    assert_eq!(provider.reached(), 0);
    assert!(child_ids(dir.path()).is_empty());
}

#[test]
fn concurrent_calls_queue_on_one_slot_and_all_finish() {
    let dir = TempDir::new("queue");
    let gate = Gate::default();
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::BlockThen(gate.clone(), vec![text("first"), stop("stop")]),
        text_response("second"),
    ]));
    let runner = make_runner(&dir, provider.clone());

    let first = {
        let r = Arc::clone(&runner);
        thread::spawn(move || call_tool(&r, "call-1", json!({"prompt": "one"})))
    };
    wait_until(|| provider.reached() >= 1);
    let second = {
        let r = Arc::clone(&runner);
        thread::spawn(move || call_tool(&r, "call-2", json!({"prompt": "two"})))
    };

    thread::sleep(Duration::from_millis(30));
    assert_eq!(provider.reached(), 1, "second call must still be queued");
    gate.open();

    let a = first.join().expect("first thread");
    let b = second.join().expect("second thread");
    assert_eq!(a.output, "first");
    assert_eq!(b.output, "second");
    let ids = child_ids(dir.path());
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert_eq!(provider.reached(), 2);
}

#[test]
fn nested_delegation_chains_parents_and_increments_depth() {
    let dir = TempDir::new("nested");
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(vec![
            tool_delta(0, "tc-1", "subagent", r#"{"prompt":"inner"}"#),
            stop("tool_calls"),
        ]),
        text_response("grandchild answer"),
        text_response("child answer"),
    ]));
    let tools: Arc<dyn hearth_agent::ToolRunner> =
        Arc::new(RecordingTools::new(vec![tool_spec("echo")]));
    let mut opt = Options::new(provider.clone(), dir.path(), "ws", "test-model");
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.tools = Some(tools);
    opt.max_depth = 2;
    let runner = Arc::new(Runner::new(opt).expect("runner"));

    let res = call_tool(&runner, "call-1", json!({"prompt": "outer"}));

    assert!(res.is_ok(), "{res:?}");
    assert_eq!(res.output, "child answer");
    let ids = child_ids(dir.path());
    assert_eq!(ids.len(), 2, "{ids:?}");

    let mut child = String::new();
    let mut grandchild = String::new();
    for id in &ids {
        let events = child_events(&dir, id);
        let head = payload(first(&events, "session/start").unwrap());
        match head["depth"].as_u64().unwrap_or(0) {
            1 => child = id.clone(),
            2 => grandchild = id.clone(),
            other => panic!("unexpected depth {other}"),
        }
    }
    assert!(!child.is_empty() && !grandchild.is_empty());
    let head = payload(first(&child_events(&dir, &grandchild), "session/start").unwrap());
    assert_eq!(head["parent"], json!(child));
    assert_eq!(head["startedBy"], json!("subagent"));
    assert_eq!(provider.requests().len(), 3);
}

#[test]
fn child_inherits_policy_but_never_an_approver() {
    let dir = TempDir::new("policy");
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(vec![
            tool_delta(0, "tc-1", "danger", "{}"),
            stop("tool_calls"),
        ]),
        text_response("carried on"),
    ]));
    let tools: Arc<RecordingTools> = Arc::new(RecordingTools::new(vec![tool_spec("danger")]));
    let mut opt = Options::new(provider, dir.path(), "ws", "test-model");
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.tools = Some(tools.clone() as Arc<dyn hearth_agent::ToolRunner>);
    opt.policy = Some(Arc::new(AskDanger));
    let runner = Arc::new(Runner::new(opt).expect("runner"));

    let res = call_tool(&runner, "call-1", json!({"prompt": "task"}));
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(res.output, "carried on");
    assert_eq!(tools.call_count(), 0, "denied before the tool ran");

    let id = only_child(&dir);
    let events = child_events(&dir, &id);
    let tr = events
        .iter()
        .find(|e| e.typ.as_str() == "tool/result")
        .expect("tool/result");
    assert_eq!(payload(tr)["code"], json!("no_approver"));
    assert_eq!(payload(tr)["ok"], json!(false));
}

#[test]
fn system_prompt_reaches_the_header_clipped_at_the_byte_cap() {
    let dir = TempDir::new("system");
    let system = "文".repeat(4000); // 12000 UTF-8 bytes
    let expected_bytes = system.len();
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("ok")]));
    let mut opt = Options::new(provider, dir.path(), "ws", "test-model");
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.system = system;
    let runner = Arc::new(Runner::new(opt).expect("runner"));

    let res = call_tool(&runner, "call-1", json!({"prompt": "task"}));
    assert!(res.is_ok(), "{res:?}");
    let id = only_child(&dir);
    let events = child_events(&dir, &id);
    let head = payload(first(&events, "session/start").unwrap());
    assert_eq!(head["systemTruncated"], json!(true));
    assert_eq!(head["systemBytes"], json!(expected_bytes));
    let kept = head["system"].as_str().unwrap();
    assert!(
        kept.len() <= hearth_agent::MAX_SYSTEM_BYTES,
        "{}",
        kept.len()
    );
    assert!(std::str::from_utf8(kept.as_bytes()).is_ok());
}

#[test]
fn per_call_model_and_system_override_the_options() {
    let dir = TempDir::new("overrides");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("ok")]));
    let runner = make_runner(&dir, provider.clone());

    let res = call_tool(
        &runner,
        "call-1",
        json!({"prompt": "task", "model": "cheap-model", "system": "be terse"}),
    );
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(res.meta["model"], json!("cheap-model"));
    let id = only_child(&dir);
    let events = child_events(&dir, &id);
    let head = payload(first(&events, "session/start").unwrap());
    assert_eq!(head["model"], json!("cheap-model"));
    assert_eq!(head["system"], json!("be terse"));
    assert_eq!(provider.requests()[0].model, "cheap-model");
}

#[test]
fn empty_child_text_is_reported_without_an_error_code() {
    let dir = TempDir::new("empty-text");
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![stop(
        "stop",
    )])]));
    let runner = make_runner(&dir, provider.clone());

    let res = call_tool(&runner, "call-1", json!({"prompt": "task"}));
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(res.code, "");
    assert!(res.output.contains("without any text"), "{}", res.output);
    assert_eq!(child_ids(dir.path()).len(), 1);
}

#[test]
fn new_validates_options_and_defaults_the_depth_limit() {
    let provider: Arc<ScriptedProvider> = Arc::new(ScriptedProvider::new(vec![]));
    let empty_dir = Options::new(provider.clone(), Path::new(""), "ws", "model");
    assert!(empty_dir.validate().is_err());
    assert!(Runner::new(empty_dir).is_err());

    let empty_model = Options::new(provider.clone(), "dir", "ws", "");
    assert!(Runner::new(empty_model).is_err());

    let default = Options::new(provider.clone(), "dir", "ws", "model");
    let runner = Runner::new(default).expect("runner");
    assert_eq!(runner.max_depth(), 1);

    let explicit = Options {
        max_depth: 3,
        ..Options::new(provider, "dir", "ws", "model")
    };
    let runner = Runner::new(explicit).expect("runner");
    assert_eq!(runner.max_depth(), 3);
}

#[test]
fn cancelled_while_queued_returns_cancelled_and_leaves_the_first_call_alone() {
    let dir = TempDir::new("queued-cancel");
    let gate = Gate::default();
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::BlockThen(gate.clone(), vec![text("first"), stop("stop")]),
        text_response("second"),
    ]));
    let runner = make_runner(&dir, provider.clone());

    let first = {
        let r = Arc::clone(&runner);
        thread::spawn(move || call_tool(&r, "call-1", json!({"prompt": "one"})))
    };
    wait_until(|| provider.reached() >= 1);

    let second_done = Arc::new(AtomicBool::new(false));
    let second = {
        let r = Arc::clone(&runner);
        let done = Arc::clone(&second_done);
        thread::spawn(move || {
            let out = call_tool(&r, "call-2", json!({"prompt": "two"}));
            done.store(true, Ordering::SeqCst);
            out
        })
    };
    thread::sleep(Duration::from_millis(30));
    assert!(!second_done.load(Ordering::SeqCst), "second must be queued");

    runner.options().canceller.cancel_call("call-2");
    thread::sleep(Duration::from_millis(20));
    assert!(
        second_done.load(Ordering::SeqCst),
        "second must wake on cancel"
    );
    gate.open();

    let a = first.join().expect("first thread");
    let b = second.join().expect("second thread");
    assert_eq!(a.output, "first");
    assert_eq!(b.code, CODE_CANCELLED, "{b:?}");
    assert_eq!(
        provider.reached(),
        1,
        "the queued call never reached a provider"
    );
    assert_eq!(child_ids(dir.path()).len(), 1);
}

#[test]
fn bad_args_are_rejected_before_any_log_is_written() {
    let dir = TempDir::new("bad-args");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("never")]));
    let runner = make_runner(&dir, provider.clone());

    for args in [
        json!({}),
        json!({"prompt": "   "}),
        json!({"prompt": "x", "modle": "typo"}),
        json!({"prompt": 123}),
        json!({"prompt": "x", "extra": 1}),
        Value::Null,
    ] {
        let res = call_tool(&runner, "call-bad", args.clone());
        assert_eq!(res.code, ERR_BAD_ARGS, "args {args}: {res:?}");
    }
    assert_eq!(provider.reached(), 0);
    assert!(
        child_ids(dir.path()).is_empty(),
        "no session may be created"
    );
}

#[test]
fn depth_limited_runner_advertises_a_tool_that_always_errors() {
    for (depth, max_depth) in [(1usize, 1usize), (3, 2), (2, 0)] {
        let dir = TempDir::new("depth");
        let provider = Arc::new(ScriptedProvider::new(vec![text_response("never")]));
        let mut opt = Options::new(provider.clone(), dir.path(), "ws", "test-model");
        opt.depth = depth;
        opt.max_depth = max_depth;
        let runner = Arc::new(Runner::new(opt).expect("runner"));
        let tool = runner.tool();
        assert_eq!(tool.spec().name, "subagent");

        let res = tool.call(Call::new(
            "call-1",
            "subagent",
            json!({"prompt": "task"}),
            "",
        ));
        let expected_max = if max_depth == 0 { 1 } else { max_depth };
        assert_eq!(res.code, CODE_DEPTH_EXCEEDED, "depth {depth}/{max_depth}");
        assert!(res.is_error);
        assert_eq!(res.meta["depth"], json!(depth));
        assert_eq!(res.meta["maxDepth"], json!(expected_max));
        assert!(res.output.contains("depth limit"));
        assert_eq!(provider.reached(), 0);
        assert!(child_ids(dir.path()).is_empty());
    }
}

/// Poll without a blocking sleep in the success path. Test-only helper.
fn wait_until<F: Fn() -> bool>(f: F) {
    for _ in 0..3000 {
        if f() {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!("condition never became true");
}
