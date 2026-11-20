//! Structured delegation: the child finishes by CALLING `structured_output`,
//! and the parent reads the submitted object back out of the child's own log.
//!
//! These tests use the same scripted provider and on-disk log as the rest of
//! the crate, because in this crate the log is the only truth: a "successful"
//! structured result must be provable from `tool/call` + `tool/result` records.

mod common;

use std::sync::Arc;

use common::*;
use forge_subagent::{Options, Runner, CODE_BAD_OUTPUT, STRUCTURED_OUTPUT_TOOL};
use forge_tool::Call;
use serde_json::{json, Value};

const FIXED_NOW_MS: i64 = 1_777_863_721_000;

fn result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" },
            "tags": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["name", "tags"]
    })
}

fn make_runner(
    dir: &TempDir,
    provider: Arc<ScriptedProvider>,
    schema: Option<Value>,
) -> Arc<Runner> {
    let mut opt = Options::new(
        provider,
        dir.path(),
        dir.path().join("workspace").to_string_lossy().into_owned(),
        "test-model",
    );
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.clock = Arc::new(|| FIXED_NOW_MS);
    opt.structured_schema = schema;
    Arc::new(Runner::new(opt).expect("runner"))
}

fn call_tool(runner: &Arc<Runner>, id: &str, args: Value) -> forge_tool::Result {
    runner.tool().call(Call::new(id, "subagent", args, ""))
}

/// The child calls `structured_output` with a valid object; the parent must
/// surface that object both in the tool result meta and, parsed, as the output.
#[test]
fn a_structured_submission_reaches_the_parent_as_an_object() {
    let dir = TempDir::new("structured-ok");
    let submitted = json!({"name": "ada", "age": 36, "tags": ["math", "code"]});
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(vec![
            tool_delta(0, "tc-1", STRUCTURED_OUTPUT_TOOL, &submitted.to_string()),
            stop("tool_calls"),
        ]),
        // The loop always asks the model once more after a tool call.
        text_response("done"),
    ]));
    let runner = make_runner(&dir, provider.clone(), Some(result_schema()));

    let res = call_tool(&runner, "call-1", json!({"prompt": "produce a record"}));

    assert!(res.is_ok(), "structured result must be a success: {res:?}");
    assert_eq!(res.code, "");
    assert_eq!(res.meta["structured"], submitted);
    let parsed: Value = serde_json::from_str(&res.output).expect("output is JSON");
    assert_eq!(parsed, submitted);

    // The tool was advertised to the child, and the log records both the call
    // (with its args) and a successful result.
    let first_request = &provider.requests()[0];
    assert!(
        first_request
            .tools
            .iter()
            .any(|t| t.name == STRUCTURED_OUTPUT_TOOL),
        "the child must be offered structured_output: {:?}",
        first_request
            .tools
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );

    let id = child_ids(dir.path())
        .into_iter()
        .next()
        .expect("one child log");
    let events = read_events_at(&dir.sessions().join(format!("{id}.jsonl")));
    let call = first(&events, "tool/call").expect("tool/call");
    let call_payload = payload(call);
    assert_eq!(call_payload["tool"], json!(STRUCTURED_OUTPUT_TOOL));
    assert_eq!(call_payload["args"], submitted);
    let result = first(&events, "tool/result").expect("tool/result");
    assert_eq!(payload(result)["ok"], json!(true));
}

/// A schema was required but the child just answered in prose: the delegation
/// is a failure (`bad_output`), never a success carrying the text.
#[test]
fn a_missing_structured_submission_is_bad_output() {
    let dir = TempDir::new("structured-missing");
    let provider = Arc::new(ScriptedProvider::new(vec![text_response(
        "I would rather explain in prose",
    )]));
    let runner = make_runner(&dir, provider, Some(result_schema()));

    let res = call_tool(&runner, "call-1", json!({"prompt": "produce a record"}));

    assert!(!res.is_ok(), "missing submission must fail: {res:?}");
    assert_eq!(res.code, CODE_BAD_OUTPUT);
    assert!(
        !res.output.contains("prose"),
        "the prose must not be passed off as the structured result: {}",
        res.output
    );
}

/// The per-call `schema` argument overrides the runner default and is what the
/// child actually sees: a schema passed on the call must reach the child even
/// when the runner itself has none.
#[test]
fn a_per_call_schema_argument_reaches_the_child() {
    let dir = TempDir::new("structured-arg");
    let submitted = json!({"name": "grace", "tags": []});
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(vec![
            tool_delta(0, "tc-1", STRUCTURED_OUTPUT_TOOL, &submitted.to_string()),
            stop("tool_calls"),
        ]),
        text_response("done"),
    ]));
    let runner = make_runner(&dir, provider.clone(), None);

    let res = call_tool(
        &runner,
        "call-1",
        json!({"prompt": "produce a record", "schema": result_schema()}),
    );

    assert!(res.is_ok(), "{res:?}");
    assert_eq!(res.meta["structured"], submitted);
    assert!(
        provider.requests()[0]
            .tools
            .iter()
            .any(|t| t.name == STRUCTURED_OUTPUT_TOOL),
        "a per-call schema must still register the terminal tool"
    );
}
