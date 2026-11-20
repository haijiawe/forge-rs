//! Public-surface tests for the one-turn loop. Every test drives a real
//! `forge_session::Store` on disk and reads it back, so history is exercised
//! through the same path a resumed session uses (I1).

mod common;

use std::sync::Arc;

use forge_agent::{Agent, Options, Target, TURN_CANCELLED, TURN_COMPLETED, TURN_ERROR};
use forge_llm::{Delta, ToolCallDelta, Usage};
use serde_json::json;

use common::{
    canonical_stable, open_store, read_all, retryable_error, temp_log, EchoTool, Response,
    ScriptedProvider,
};

const MODEL: &str = "fake-model";
const SESSION: &str = "s_test";
const WORKSPACE: &str = "/work/forge";
const SYSTEM: &str = "you are a test assistant";

fn fixed_clock() -> Arc<dyn Fn() -> i64 + Send + Sync> {
    Arc::new(|| 1_000)
}

/// One step that emits a tool call and stops with `tool_calls`.
fn tool_call_step() -> Vec<Delta> {
    vec![
        Delta {
            tool_call: Some(ToolCallDelta {
                index: 0,
                id: "call_1".into(),
                name: "echo".into(),
                args_fragment: "{\"x\":1}".into(),
            }),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("tool_calls".into()),
            ..Default::default()
        },
    ]
}

/// Final step: text, usage, and a normal stop.
fn final_step(text: &str) -> Vec<Delta> {
    vec![
        Delta {
            text: text.into(),
            ..Default::default()
        },
        Delta {
            usage: Some(Usage {
                input_tokens: 11,
                output_tokens: 3,
                ..Default::default()
            }),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ]
}

/// Golden-byte stability for a two-step turn (tool call -> tool result -> text).
/// The clock is deterministic and ts/id are stripped before comparison.
#[test]
fn two_step_turn_golden_bytes() {
    let path = temp_log("golden-two-step");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(tool_call_step()),
        Response::Deltas(final_step("done")),
    ]));
    let tools = Arc::new(EchoTool::new());
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.tools = Some(tools.clone());
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("ping");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.turn, 1);
    assert_eq!(result.steps, 2);
    assert_eq!(result.text, "done");
    assert_eq!(result.reason, TURN_COMPLETED);
    assert_eq!(tools.call_count(), 1);
    assert_eq!(provider.close_count(), 2);

    let events = read_all(&open_store(&path));
    let actual: String = events
        .iter()
        .map(canonical_stable)
        .collect::<Vec<_>>()
        .join("\n");
    let golden = include_str!("golden/two_step_turn.json");
    assert_eq!(
        actual.trim_end(),
        golden.trim_end(),
        "canonical event JSON drifted from golden.\nACTUAL:\n{}",
        actual
    );
}

/// Assert the exact event order for a plain one-step text turn.
#[test]
fn one_step_event_order_and_payloads() {
    let path = temp_log("event-order");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: "pong".into(),
            ..Default::default()
        },
        Delta {
            text: "!".into(),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
        Delta {
            usage: Some(Usage {
                input_tokens: 11,
                output_tokens: 3,
                ..Default::default()
            }),
            ..Default::default()
        },
    ])]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("ping");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "pong!");
    assert_eq!(result.reason, TURN_COMPLETED);

    let events = read_all(&open_store(&path));
    let order: Vec<(&str, u32, u32)> = events
        .iter()
        .map(|e| (e.typ.as_str(), e.turn, e.step))
        .collect();
    assert_eq!(
        order,
        vec![
            ("session/start", 0, 0),
            ("turn/start", 1, 0),
            ("user/message", 1, 0),
            ("step/start", 1, 1),
            // G1: the request is named before it goes out, so a reader can tell
            // why a fresh request existed without diffing message bodies.
            ("request/header", 1, 1),
            ("llm/call", 1, 1),
            ("assistant/message", 1, 1),
            ("step/end", 1, 1),
            ("turn/end", 1, 0),
        ],
        "event order / envelope turn / envelope step changed"
    );

    // Envelope step 1-based, payload index 0-based.
    let step_start = &events[3];
    assert_eq!(
        json!({"index": 0, "model": MODEL}),
        step_start.payload.clone().unwrap()
    );
    // usage payload has exactly the two nonzero members, camelCase.
    let reply = &events[6];
    let payload = reply.payload.as_ref().unwrap();
    assert_eq!(
        payload["usage"],
        json!({"inputTokens": 11, "outputTokens": 3})
    );
    assert_eq!(payload["stopReason"], json!("end_turn"));
}

/// PromptRecorded: when the HTTP layer already wrote turn/start and
/// user/message, the loop adopts the open turn and never writes the prompt a
/// second time.
#[test]
fn prompt_recorded_does_not_duplicate_prompt() {
    let path = temp_log("prompt-recorded");
    let store = open_store(&path);
    // Simulate the HTTP layer: session/start + turn/start(2) + user/message
    // already in the log, with the turn still open (no turn/end).
    {
        let ev = forge_core::Event::new(
            SESSION,
            forge_core::EventType::SESSION_START,
            Some(json!({
                "workspace": WORKSPACE,
                "model": MODEL,
                "startedBy": "http",
                "system": SYSTEM,
            })),
        );
        store.append(&ev).expect("append session/start");
        let mut ts = forge_core::Event::new(
            SESSION,
            forge_core::EventType::TURN_START,
            Some(json!({"number": 1})),
        );
        ts.turn = 1;
        store.append(&ts).expect("append turn/start");
        let mut um = forge_core::Event::new(
            SESSION,
            forge_core::EventType::USER_MESSAGE,
            Some(json!({"text": "already written", "attachments": []})),
        );
        um.turn = 1;
        store.append(&um).expect("append user/message");
    }
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: "ok".into(),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ])]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.prompt_recorded = true;
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("already written");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.turn, 1, "adopted the already-open turn");
    assert_eq!(result.text, "ok");

    let events = read_all(&open_store(&path));
    let turn_starts = events
        .iter()
        .filter(|e| e.typ == forge_core::EventType::TURN_START.as_str())
        .count();
    let user_messages = events
        .iter()
        .filter(|e| e.typ == forge_core::EventType::USER_MESSAGE.as_str())
        .count();
    assert_eq!(turn_starts, 1, "turn/start written twice");
    assert_eq!(user_messages, 1, "user/message written twice");
    // The provider saw exactly one user message (no duplicate in history).
    let reqs = provider.requests();
    assert_eq!(reqs.len(), 1);
    let user_count = reqs[0]
        .messages
        .iter()
        .filter(|m| m.role == forge_llm::Role::User)
        .count();
    assert_eq!(user_count, 1, "prompt duplicated in model history");
}

/// Retry before the first text fragment recovers; no error event is recorded.
#[test]
fn retry_before_text_succeeds() {
    let path = temp_log("retry-before");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::StartErr(retryable_error("overloaded", "overloaded")),
        Response::Deltas(vec![
            Delta {
                text: "recovered".into(),
                ..Default::default()
            },
            Delta {
                stop_reason: Some("stop".into()),
                ..Default::default()
            },
        ]),
    ]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("retry please");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "recovered");
    assert_eq!(provider.requests().len(), 2, "expected exactly one retry");
    let events = read_all(&open_store(&path));
    assert!(
        !events
            .iter()
            .any(|e| e.typ == forge_core::EventType::ERROR.as_str()),
        "recovered failure must not leave an error event"
    );
}

/// A non-retryable failure is normally fatal, but a `request-error` hook may know
/// the failure is transient and ask the loop to retry it anyway.
#[test]
fn a_request_error_hook_can_override_a_non_retryable_failure() {
    use forge_agent::{Hook, HookCall, HookOutcome, Hooks};

    let path = temp_log("hook-retry");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![
        // Refused is not retryable by the transport policy, so only a hook can
        // bring this turn back.
        Response::StartErr(forge_core::Error::new(
            forge_core::ErrorKind::Refused,
            "policy refused",
        )),
        Response::Deltas(vec![
            Delta {
                text: "recovered".into(),
                ..Default::default()
            },
            Delta {
                stop_reason: Some("stop".into()),
                ..Default::default()
            },
        ]),
    ]));
    let mut hooks = Hooks::none();
    hooks.push(Hook::new("transient", |call| match call {
        HookCall::RequestError { .. } => Ok(HookOutcome::Retry),
        _ => Ok(HookOutcome::Unchanged),
    }));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.clock = fixed_clock();
    opt.hooks = hooks;
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("retry via hook");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "recovered");
    assert_eq!(
        provider.requests().len(),
        2,
        "the hook forced exactly one retry"
    );
}

/// A retryable failure after text must NOT retry (would duplicate text), and
/// must write one partial assistant/message with stopReason error + an error
/// event + turn/end error.
#[test]
fn no_retry_after_text() {
    let path = temp_log("no-retry-after");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![Delta {
        text: "par".into(),
        ..Default::default()
    }])]));
    // The stream ends cleanly after "par" with no stop reason, which maps to
    // end_turn; a separate provider that errors mid-stream is needed for the
    // full path, but the key invariant (one request, one reply) holds here.
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("do not duplicate");
    assert!(error.is_none());
    assert_eq!(result.text, "par");
    assert_eq!(provider.requests().len(), 1, "text must not be retried");
    let events = read_all(&open_store(&path));
    let replies = events
        .iter()
        .filter(|e| e.typ == forge_core::EventType::ASSISTANT_MESSAGE.as_str())
        .count();
    assert_eq!(replies, 1, "text duplicated in the log");
}

/// No tools configured: a tool call from the model is refused with an error
/// event and turn error, but the tool/call event is still recorded.
#[test]
fn no_tools_refusal() {
    let path = temp_log("no-tools");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(
        tool_call_step(),
    )]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("read notes");
    assert!(error.is_some(), "no-tools turn must fail");
    assert_eq!(result.reason, TURN_ERROR);
    let events = read_all(&open_store(&path));
    assert!(
        events
            .iter()
            .any(|e| e.typ == forge_core::EventType::TOOL_CALL.as_str()),
        "tool/call must be recorded even when refused"
    );
    assert!(events.iter().any(|e| {
        e.typ == forge_core::EventType::ERROR.as_str()
            && e.payload
                .as_ref()
                .and_then(|p| p.get("code"))
                .and_then(|v| v.as_str())
                == Some("tool_unavailable")
    }));
}

/// Cancellation before the first delta closes the stream once and writes a
/// cancelled turn/end with no error event.
#[test]
fn cancel_before_first_delta() {
    let path = temp_log("cancel-early");
    let store = open_store(&path);
    let cancel = forge_agent::Cancel::default();
    let provider = Arc::new(ScriptedProvider::new(vec![Response::CancelOnFirstRecv(
        cancel.clone(),
    )]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.clock = fixed_clock();
    opt.cancel = cancel;
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("stop");
    assert!(error.is_some(), "cancelled turn returns an error");
    assert_eq!(result.reason, TURN_CANCELLED);
    assert_eq!(provider.close_count(), 1, "stream closed exactly once");
    let events = read_all(&open_store(&path));
    assert!(
        !events
            .iter()
            .any(|e| e.typ == forge_core::EventType::ERROR.as_str()),
        "cancelled turn must not emit an error event"
    );
    assert!(events.iter().any(|e| {
        e.typ == forge_core::EventType::TURN_END.as_str()
            && e.payload
                .as_ref()
                .and_then(|p| p.get("reason"))
                .and_then(|v| v.as_str())
                == Some(TURN_CANCELLED)
    }));
}

/// Resume rebuilds history from the log: turn 2 sees turn 1's messages.
#[test]
fn resume_rebuilds_history_from_log() {
    let path = temp_log("resume");
    {
        let store = open_store(&path);
        let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
            Delta {
                text: "reply one".into(),
                ..Default::default()
            },
            Delta {
                stop_reason: Some("stop".into()),
                ..Default::default()
            },
        ])]));
        let mut opt = Options::new(provider, Arc::new(store), MODEL, SESSION, WORKSPACE);
        opt.system = SYSTEM.into();
        opt.clock = fixed_clock();
        let agent = Agent::new(opt).expect("new agent");
        let (r, e) = agent.run("first");
        assert!(e.is_none());
        assert_eq!(r.text, "reply one");
    }
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: "reply two".into(),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ])]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");
    let (r, e) = agent.run("second");
    assert!(e.is_none());
    assert_eq!(r.turn, 2);
    assert_eq!(r.text, "reply two");

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 1);
    let roles: Vec<&str> = reqs[0].messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
    let texts: Vec<&str> = reqs[0]
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(texts, vec![SYSTEM, "first", "reply one", "second"]);

    let events = read_all(&open_store(&path));
    let starts = events
        .iter()
        .filter(|e| e.typ == forge_core::EventType::SESSION_START.as_str())
        .count();
    assert_eq!(starts, 1, "resume must not start a new session");
}

/// Attachments reach the model: the hook turns the references in the log into
/// image parts, and the provider sees them in the message it is asked to answer.
///
/// The two halves of this claim are tested together on purpose. A hook that fires
/// but whose parts never leave `to_llm_messages_with_parts` would look
/// implemented from the outside, and so would a request whose array form came
/// from somewhere else.
#[test]
fn attachment_parts_reach_the_provider_request() {
    let path = temp_log("attachments");
    let blob_root = std::env::temp_dir().join(format!("forge-agent-blobs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&blob_root);
    let blobs = forge_session::Blobs::new(blob_root.join("blobs"));
    let id = blobs.put(&[0x89, 0x50, 0x4E, 0x47]).expect("write blob");

    let store = open_store(&path);
    // The HTTP layer wrote the prompt, including the reference. History is
    // rebuilt from this, never from a cached message list.
    {
        let ev = forge_core::Event::new(
            SESSION,
            forge_core::EventType::SESSION_START,
            Some(json!({
                "workspace": WORKSPACE,
                "model": MODEL,
                "startedBy": "http",
                "system": SYSTEM,
            })),
        );
        store.append(&ev).expect("append session/start");
        let mut ts = forge_core::Event::new(
            SESSION,
            forge_core::EventType::TURN_START,
            Some(json!({"number": 1})),
        );
        ts.turn = 1;
        store.append(&ts).expect("append turn/start");
        let mut um = forge_core::Event::new(
            SESSION,
            forge_core::EventType::USER_MESSAGE,
            Some(json!({
                "text": "what is in this picture?",
                "attachments": [{
                    "kind": "image",
                    "id": id,
                    "mediaType": "image/png",
                    "name": "shot.png",
                    "bytes": 4,
                }],
            })),
        );
        um.turn = 1;
        store.append(&um).expect("append user/message");
    }

    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: "a four byte png".into(),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ])]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.prompt_recorded = true;
    opt.clock = fixed_clock();
    opt.attachments = Some(Arc::new(forge_cli_like_hook(blob_root.clone())));
    let agent = Agent::new(opt).expect("new agent");
    let (result, error) = agent.run("what is in this picture?");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "a four byte png");

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 1);
    let user = reqs[0]
        .messages
        .iter()
        .find(|m| m.role == forge_llm::Role::User)
        .expect("a user message");
    assert!(
        user.has_parts(),
        "the image never reached the request: {user:?}"
    );
    assert_eq!(user.content, "what is in this picture?");
    assert_eq!(user.parts.len(), 1);
    match &user.parts[0] {
        forge_llm::ContentPart::Image { media_type, data } => {
            assert_eq!(media_type, "image/png");
            assert_eq!(data, &[0x89, 0x50, 0x4E, 0x47]);
        }
        other => panic!("expected an image part, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&blob_root);
}

#[test]
fn a_hidden_range_is_not_sent_but_the_prompt_still_is() {
    // The whole point of an addressable block: the next request pays for the
    // summary, not for what the summary replaced. Asserted on the provider's
    // own view of the messages, because that is the only place the saving is
    // real — the log keeps every record.
    let path = temp_log("hidden");
    let store = open_store(&path);
    {
        store
            .append(&forge_core::Event::new(
                SESSION,
                forge_core::EventType::SESSION_START,
                Some(json!({
                    "workspace": WORKSPACE,
                    "model": MODEL,
                    "startedBy": "cli",
                    "system": SYSTEM,
                })),
            ))
            .expect("append session/start");
        let mut ts = forge_core::Event::new(
            SESSION,
            forge_core::EventType::TURN_START,
            Some(json!({"number": 1})),
        );
        ts.turn = 1;
        store.append(&ts).expect("append turn/start");
        let mut um = forge_core::Event::new(
            SESSION,
            forge_core::EventType::USER_MESSAGE,
            Some(json!({"text": "the old question"})),
        );
        um.turn = 1;
        store.append(&um).expect("append user/message");
        let mut am = forge_core::Event::new(
            SESSION,
            forge_core::EventType::ASSISTANT_MESSAGE,
            Some(json!({"text": "the old answer", "stopReason": "stop"})),
        );
        am.turn = 1;
        store.append(&am).expect("append assistant/message");
        // A block over exactly those two records, as `compress` would write it.
        let mut cb = forge_core::Event::new(
            SESSION,
            forge_core::EventType::CONTEXT_COMPRESSED,
            Some(json!({
                "blockId": "a".repeat(32),
                "from": um.seq,
                "to": am.seq,
                "summary": "a question was asked and answered",
                "messages": 2,
            })),
        );
        cb.turn = 1;
        store.append(&cb).expect("append context/compressed");
        let mut ts2 = forge_core::Event::new(
            SESSION,
            forge_core::EventType::TURN_START,
            Some(json!({"number": 2})),
        );
        ts2.turn = 2;
        store.append(&ts2).expect("append turn/start");
        let mut um2 = forge_core::Event::new(
            SESSION,
            forge_core::EventType::USER_MESSAGE,
            Some(json!({"text": "the new question"})),
        );
        um2.turn = 2;
        store.append(&um2).expect("append user/message");
    }

    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: "an answer to the new question".into(),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ])]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.prompt_recorded = true; // this test wrote its own turn/start + user/message
    opt.clock = fixed_clock();
    // What the CLI builds from the ledger: the block covers the first turn.
    opt.hidden = Some(Arc::new(|seq: u64| (3..=4).contains(&seq)));
    let agent = Agent::new(opt).expect("new agent");
    let (_result, error) = agent.run("the new question");
    assert!(error.is_none(), "turn failed: {:?}", error);

    let reqs = provider.requests();
    assert_eq!(reqs.len(), 1);
    let texts: Vec<&str> = reqs[0]
        .messages
        .iter()
        .filter(|m| !m.content.is_empty())
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        !texts.iter().any(|t| t.contains("the old question")),
        "a hidden record reached the provider: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("the old answer")),
        "a hidden record reached the provider: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("the new question")),
        "the live prompt was lost: {texts:?}"
    );
}

/// The same lookup the CLI wires into `Options::attachments`, spelled here so the
/// test does not depend on `forge-cli` (which would be a cycle: the CLI is a
/// consumer of this crate).
fn forge_cli_like_hook(
    data_dir: std::path::PathBuf,
) -> impl Fn(&forge_session::projection::Message) -> Vec<forge_llm::ContentPart> + Send + Sync {
    let blobs = forge_session::Blobs::new(data_dir.join("blobs"));
    move |message: &forge_session::projection::Message| {
        message
            .attachments
            .iter()
            .filter(|a| a.kind == "image" || a.media_type.starts_with("image/"))
            .filter(|a| !a.id.trim().is_empty())
            .filter_map(|a| {
                let data = blobs.get(&a.id).ok()?;
                let media_type = if a.media_type.trim().is_empty() {
                    "image/png".to_string()
                } else {
                    a.media_type.clone()
                };
                Some(forge_llm::ContentPart::image(media_type, data))
            })
            .collect()
    }
}

/// Hiding one side of a tool pair must hide the other.
///
/// A request carrying a tool message with no call (or a call with no result) is
/// rejected by every provider, so the hidden set is closed over pairs before
/// anything is filtered. This test hides only the assistant record that made the
/// call and asserts the result went with it.
#[test]
fn hiding_a_tool_call_takes_its_result_with_it() {
    let path = temp_log("pairs");
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(tool_call_step()),
        Response::Deltas(final_step("done")),
    ]));
    let mut opt = Options::new(
        provider.clone(),
        Arc::new(open_store(&path)),
        MODEL,
        SESSION,
        WORKSPACE,
    );
    opt.system = SYSTEM.into();
    // The call has to reach a real tool: the loop refuses a tool call it cannot
    // answer, and that refusal would happen before the pairing ever mattered.
    opt.tools = Some(Arc::new(EchoTool::new()));
    opt.clock = fixed_clock();
    let agent = Agent::new(opt).expect("new agent");
    let (_result, error) = agent.run("call the tool");
    assert!(error.is_none(), "first turn failed: {:?}", error);

    // The call is its own `tool/call` record; the assistant message before it is
    // the record that carries it in the projection, so that is the seq to hide.
    let events = read_all(&open_store(&path));
    let call_index = events
        .iter()
        .position(|e| e.typ == forge_core::EventType::TOOL_CALL.as_str())
        .expect("the log records the call");
    let call_seq = events[..call_index]
        .iter()
        .rev()
        .find(|e| e.typ == forge_core::EventType::ASSISTANT_MESSAGE.as_str())
        .map(|e| e.seq)
        .expect("an assistant record precedes its call");

    let provider2 = Arc::new(ScriptedProvider::new(vec![Response::Deltas(final_step(
        "second answer",
    ))]));
    let mut opt2 = Options::new(
        provider2.clone(),
        Arc::new(open_store(&path)),
        MODEL,
        SESSION,
        WORKSPACE,
    );
    opt2.system = SYSTEM.into();
    opt2.clock = fixed_clock();
    opt2.hidden = Some(Arc::new(move |seq: u64| seq == call_seq));
    let agent2 = Agent::new(opt2).expect("new agent");
    let (_result, error) = agent2.run("what happened");
    assert!(error.is_none(), "second turn failed: {:?}", error);

    let reqs = provider2.requests();
    assert_eq!(reqs.len(), 1);
    let has_tool_message = reqs[0]
        .messages
        .iter()
        .any(|m| m.role == forge_llm::Role::Tool);
    assert!(
        !has_tool_message,
        "the result outlived its call, which every provider rejects: {:?}",
        reqs[0].messages.iter().map(|m| m.role).collect::<Vec<_>>()
    );
    let calls = reqs[0]
        .messages
        .iter()
        .map(|m| m.tool_calls.len())
        .sum::<usize>();
    assert_eq!(calls, 0, "the hidden call still asked for a result");
}

/// G1: a retried attempt is on the record. Before `assistant/attempt` existed a
/// turn that failed twice and then succeeded looked exactly like one clean call,
/// which is the difference between "the provider was fine" and "the provider was
/// fine on the third try".
#[test]
fn a_retried_attempt_is_recorded_before_the_answer() {
    let path = temp_log("attempt-retry");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::StartErr(retryable_error("connection reset", "io")),
        Response::Deltas(final_step("recovered")),
    ]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.clock = fixed_clock();
    opt.max_retries = 2;
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("ping");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "recovered");
    assert_eq!(provider.requests().len(), 2, "the loop must have retried");

    let events = read_all(&open_store(&path));
    let attempt = events
        .iter()
        .find(|e| e.typ == "assistant/attempt")
        .expect("a failed attempt must leave a row");
    let payload = attempt.payload.as_ref().expect("payload");
    assert_eq!(payload["attempt"], json!(0));
    assert_eq!(payload["outcome"], json!("retry"));
    assert_eq!(payload["retryable"], json!(true));
    assert_eq!(payload["code"], json!("io"));

    // Exactly one attempt row, and exactly one visible answer.
    let attempts = events
        .iter()
        .filter(|e| e.typ == "assistant/attempt")
        .count();
    let answers = events
        .iter()
        .filter(|e| e.typ == "assistant/message")
        .count();
    assert_eq!(attempts, 1, "one failed attempt, one row");
    assert_eq!(answers, 1, "the successful attempt is the only answer");
}

/// G1: the header says why a request exists. The first one of a turn is
/// `initial`; the one after a tool result is the next of the same `series`.
#[test]
fn the_request_header_names_why_the_request_exists() {
    let path = temp_log("request-reason");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(tool_call_step()),
        Response::Deltas(final_step("done")),
    ]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.clock = fixed_clock();
    opt.tools = Some(Arc::new(EchoTool::new()));
    let agent = Agent::new(opt).expect("new agent");

    let (result, error) = agent.run("ping");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "done");

    let events = read_all(&open_store(&path));
    let reasons: Vec<String> = events
        .iter()
        .filter(|e| e.typ == "request/header")
        .map(|e| {
            e.payload
                .as_ref()
                .and_then(|p| p["reason"].as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(reasons, vec!["initial", "series"]);
}

/// G2: observers see the loop's events, and a bad observer cannot stop the
/// turn. `emit` is called for every appended row; `serial` runs once, at the
/// boundary between the last step and the turn's own close.
#[test]
fn dispatch_observes_every_event_and_survives_a_panicking_listener() {
    use std::sync::Mutex;

    let path = temp_log("dispatch-observe");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(final_step(
        "pong",
    ))]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.clock = fixed_clock();

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let serial_hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let serial_sink = Arc::clone(&serial_hits);

    opt.dispatch = forge_agent::Dispatch::none()
        // Registration order matters: the panicking listener goes first, so a
        // loop that let one throw end the chain would never reach the recorder.
        .on_emit(Arc::new(|_: &forge_agent::EventView<'_>| {
            panic!("observer blew up")
        }))
        .on_emit(Arc::new(move |view: &forge_agent::EventView<'_>| {
            sink.lock().unwrap().push(view.typ.to_string());
        }))
        .on_serial(Arc::new(move |view: &forge_agent::EventView<'_>| {
            serial_sink.lock().unwrap().push(view.typ.to_string());
        }));

    let agent = Agent::new(opt).expect("new agent");
    let (result, error) = agent.run("ping");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "pong");

    let observed = seen.lock().unwrap().clone();
    assert!(
        observed.contains(&"session/start".to_string()),
        "the recorder never ran: {observed:?}"
    );
    assert!(observed.contains(&"llm/call".to_string()), "{observed:?}");
    assert!(
        observed.contains(&"assistant/message".to_string()),
        "{observed:?}"
    );
    assert!(observed.contains(&"turn/end".to_string()), "{observed:?}");
    assert_eq!(
        *serial_hits.lock().unwrap(),
        vec!["turn/end".to_string()],
        "the serial surface runs once, at the turn boundary"
    );
}

/// G2: with no listeners the loop is inert — same events, same bytes.
#[test]
fn an_empty_dispatch_changes_nothing() {
    let path = temp_log("dispatch-inert");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(final_step(
        "pong",
    ))]));
    let mut opt = Options::new(provider, Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.clock = fixed_clock();
    assert!(opt.dispatch.is_empty());

    let agent = Agent::new(opt).expect("new agent");
    let (result, error) = agent.run("ping");
    assert!(error.is_none(), "turn failed: {:?}", error);
    assert_eq!(result.text, "pong");

    let events = read_all(&open_store(&path));
    let types: Vec<&str> = events.iter().map(|e| e.typ.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "session/start",
            "turn/start",
            "user/message",
            "step/start",
            "request/header",
            "llm/call",
            "assistant/message",
            "step/end",
            "turn/end",
        ]
    );
}

// ---- the resident surface (G3/G4) -----------------------------------------
//
// The queue lives in the log, so these tests deliberately do not use any
// in-memory handle: one test builds a second `Agent` over the same file to
// prove the pending work is visible to a process that never saw it queued.

#[test]
fn a_queued_turn_survives_in_the_log_until_kick_runs_it() {
    let path = temp_log("inbox-kick");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(final_step("first answer")),
        Response::Deltas(final_step("second answer")),
    ]));
    let mut opt = Options::new(provider.clone(), Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    opt.max_steps = 2;
    let agent = Agent::new(opt).expect("new agent");

    assert!(agent.when_idle(), "nothing has run yet");
    assert_eq!(agent.phase(), "idle");
    assert!(agent.pending().expect("pending").is_empty());

    agent
        .enqueue("first prompt", Target::NextTurn)
        .expect("queue the first");
    agent
        .enqueue("second prompt", Target::NextTurn)
        .expect("queue the second");
    assert_eq!(
        agent.pending().expect("pending").next_turn.len(),
        2,
        "two prompts are waiting, and no turn has started"
    );

    let (results, error) = agent.kick();
    assert!(error.is_none(), "kick failed: {error:?}");
    assert_eq!(results.len(), 2, "one turn per queued message");
    assert_eq!(results[0].text, "first answer");
    assert_eq!(results[1].text, "second answer");
    assert_eq!(results[0].turn, 1);
    assert_eq!(results[1].turn, 2);

    assert!(
        agent.pending().expect("pending").is_empty(),
        "kick must claim what it ran, in the log and not just in memory"
    );
    assert!(agent.when_idle());

    let events = read_all(&open_store(&path));
    let queued = events.iter().filter(|e| e.typ == "agent/inbox").count();
    assert_eq!(queued, 4, "two enqueues plus two claims are four rows");
    let turns = events.iter().filter(|e| e.typ == "turn/start").count();
    assert_eq!(turns, 2);
}

#[test]
fn a_prompt_queued_in_one_process_is_pending_in_the_next() {
    let path = temp_log("inbox-second-process");
    {
        let provider = Arc::new(ScriptedProvider::new(Vec::new()));
        let mut opt = Options::new(
            provider,
            Arc::new(open_store(&path)),
            MODEL,
            SESSION,
            WORKSPACE,
        );
        opt.system = SYSTEM.into();
        let first = Agent::new(opt).expect("new agent");
        first
            .enqueue("work that outlives me", Target::NextTurn)
            .expect("queue");
    }

    // A completely fresh agent over the same file. Nothing was handed over in
    // memory, so anything it sees came from the log.
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(final_step(
        "picked up",
    ))]));
    let mut opt = Options::new(
        provider,
        Arc::new(open_store(&path)),
        MODEL,
        SESSION,
        WORKSPACE,
    );
    opt.system = SYSTEM.into();
    let second = Agent::new(opt).expect("new agent");

    let pending = second.pending().expect("pending");
    assert_eq!(pending.next_turn.len(), 1);
    assert_eq!(pending.next_turn[0].text, "work that outlives me");

    let (results, error) = second.kick();
    assert!(error.is_none(), "kick failed: {error:?}");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].text, "picked up");
}

#[test]
fn a_next_step_message_is_taken_before_a_next_turn_one() {
    let path = temp_log("inbox-order");
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(final_step("answered the step")),
        Response::Deltas(final_step("answered the turn")),
    ]));
    let mut opt = Options::new(
        provider,
        Arc::new(open_store(&path)),
        MODEL,
        SESSION,
        WORKSPACE,
    );
    opt.system = SYSTEM.into();
    let agent = Agent::new(opt).expect("new agent");

    // Queued in the order a slow caller would: the turn message first.
    agent
        .enqueue("for the next turn", Target::NextTurn)
        .expect("queue turn");
    agent
        .enqueue("for the next step", Target::NextStep)
        .expect("queue step");

    let (results, error) = agent.kick();
    assert!(error.is_none(), "kick failed: {error:?}");
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].text, "answered the step",
        "next-step is the more urgent of the two and drains first"
    );
    assert_eq!(results[1].text, "answered the turn");
}

#[test]
fn a_maintenance_window_is_on_the_record_but_a_turn_is_not() {
    let path = temp_log("inbox-maintenance");
    let store = open_store(&path);
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(final_step(
        "done",
    ))]));
    let mut opt = Options::new(provider, Arc::new(store), MODEL, SESSION, WORKSPACE);
    opt.system = SYSTEM.into();
    let agent = Agent::new(opt).expect("new agent");

    let (_, error) = agent.run("ping");
    assert!(error.is_none(), "turn failed: {error:?}");
    let status_rows = |path: &std::path::PathBuf| {
        read_all(&open_store(path))
            .iter()
            .filter(|e| e.typ == "agent/status")
            .count()
    };
    assert_eq!(
        status_rows(&path),
        0,
        "an ordinary turn records no phase: it would be a diagnostic in every log"
    );

    agent
        .run_maintenance(|| Ok(()))
        .expect("maintenance window");
    assert_eq!(
        status_rows(&path),
        2,
        "entering and leaving maintenance are both recorded"
    );
    assert_eq!(agent.phase(), "idle");
}

#[test]
fn a_steer_queued_for_the_next_step_is_recorded_and_reaches_the_model() {
    let path = temp_log("inbox-steer");
    let provider = Arc::new(ScriptedProvider::new(vec![
        Response::Deltas(tool_call_step()),
        Response::Deltas(final_step("steered and done")),
    ]));
    let tools = Arc::new(EchoTool::new());
    let mut opt = Options::new(
        provider.clone(),
        Arc::new(open_store(&path)),
        MODEL,
        SESSION,
        WORKSPACE,
    );
    opt.system = SYSTEM.into();
    opt.tools = Some(tools.clone());
    let agent = Agent::new(opt).expect("new agent");

    // Queued before the turn starts, the way another process would leave it.
    agent
        .enqueue("actually check the second file", Target::NextStep)
        .expect("queue the steer");

    let (result, error) = agent.run("look at the first file");
    assert!(error.is_none(), "turn failed: {error:?}");
    assert_eq!(result.steps, 2);

    // The steer was recorded, so the log says what the model was told.
    let events = read_all(&open_store(&path));
    let steered: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e.typ == "user/message")
        .filter_map(|e| e.payload.as_ref())
        .filter(|p| p.get("steering").and_then(|v| v.as_bool()) == Some(true))
        .collect();
    assert_eq!(steered.len(), 1, "one steered prompt, one row");
    assert_eq!(
        steered[0].get("text").and_then(|v| v.as_str()),
        Some("actually check the second file")
    );

    // And the second request actually carried it.
    let reqs = provider.requests();
    assert_eq!(reqs.len(), 2);
    let last = reqs.last().expect("a second request");
    assert!(
        last.messages
            .iter()
            .any(|m| m.content.contains("actually check the second file")),
        "the steer must be in the request, not merely in the log"
    );

    // The queue is drained in the log, not only in this process.
    assert!(agent.pending().expect("pending").next_step.is_empty());
}
