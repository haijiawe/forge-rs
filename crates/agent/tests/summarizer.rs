//! Tests for the summarizer: frozen system prompt, no tools, no retries,
//! thinking markers stripped, byte-budget rune clipping, strict seq order.

mod common;

use std::sync::Arc;

use hearth_agent::{
    clip_summary, strip_thinking, ProviderSummarizer, ERR_NO_SUMMARY_TEXT, ERR_SUMMARY_TOOL_CALL,
    ERR_SUMMARY_TRUNCATED,
};
use hearth_core::{Event, EventType};
use hearth_llm::{Delta, Message, Role, ToolCallDelta, Usage};
use serde_json::json;

use common::{Response, ScriptedProvider};

const MODEL: &str = "summarizer-model";

/// Two messages so the provider path is exercised (one message short-circuits).
fn two_messages() -> Vec<Message> {
    vec![Message::user("first"), Message::assistant("second")]
}

fn text_provider(text: &str) -> Arc<ScriptedProvider> {
    Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: text.into(),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ])]))
}

#[test]
fn strip_thinking_removes_markers() {
    assert_eq!(
        strip_thinking("The range covers tool use. Response: none."),
        "The range covers tool use. Response: none."
    );
    assert_eq!(
        strip_thinking("<thinking>maybe</thinking>The range covers setup."),
        "The range covers setup."
    );
    assert_eq!(strip_thinking("  just a summary  "), "just a summary");
    assert_eq!(strip_thinking(""), "");
}

/// A model that opens a scratchpad block and never closes it must not get its
/// private reasoning into the durable record. The Go original stops scanning at
/// the unclosed marker and keeps the text; this port truncates there instead
/// (observed on a real log whose stored summary began with an open marker).
#[test]
fn strip_thinking_drops_an_unterminated_block() {
    let summary = " thinking1. **What the user asked** - question 1 through 30.";
    assert_eq!(strip_thinking(summary), "");
    assert_eq!(
        strip_thinking("<thinking>private plan"),
        "",
        "an unclosed XML-style marker also runs to the end"
    );
    assert_eq!(
        strip_thinking("Answered the range.  thinkinghalf a plan"),
        "Answered the range.",
        "text before the marker survives, the unterminated block does not"
    );
    assert_eq!(strip_thinking("Kept. <thinking>unclosed"), "Kept.");
    // Both spellings appear in real provider output; the short one leaked
    // into a stored summary before it was handled.
    assert_eq!(
        strip_thinking("<think>private plan</think>The range covers setup."),
        "The range covers setup."
    );
    assert_eq!(
        strip_thinking("<think>never closed"),
        "",
        "an unclosed short marker also runs to the end"
    );
}

#[test]
fn clip_summary_cuts_on_rune_boundary() {
    assert_eq!(clip_summary("中文中文中文", 8), "中…");
    assert_eq!(clip_summary("a😀b", 5), "a…");
    assert_eq!(clip_summary("😀", 2), "");
    assert_eq!(clip_summary("你好", 6), "你好");
    assert_eq!(clip_summary("short", 100), "short");
    // Every budget yields valid UTF-8 no longer than the cap.
    let text = "a中😀b文";
    for budget in 1..=text.len() + 3 {
        let got = clip_summary(text, budget);
        assert!(got.len() <= budget, "budget {budget} produced {got:?}");
        assert!(std::str::from_utf8(got.as_bytes()).is_ok());
    }
}

#[test]
fn summarizer_happy_path_and_usage() {
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: "A concise summary.".into(),
            ..Default::default()
        },
        Delta {
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            }),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("stop".into()),
            ..Default::default()
        },
    ])]));
    let s = ProviderSummarizer::new(Some(provider.clone()), MODEL);
    let summary = s.summarize_messages(&two_messages()).expect("summarize");
    assert_eq!(summary.text, "A concise summary.");
    assert!(summary.usage_reported);
    assert_eq!(summary.usage.input_tokens, 10);
    assert_eq!(summary.usage.output_tokens, 5);
    let (last, reported) = s.last_usage();
    assert!(reported);
    assert_eq!(last.input_tokens, 10);
    // One provider call, no retries.
    assert_eq!(provider.requests().len(), 1);
    assert_eq!(provider.close_count(), 1);
    // No tools offered; system prompt leads and the frozen text is intact.
    let req = &provider.requests()[0];
    assert!(req.tools.is_empty());
    assert_eq!(req.messages[0].role, Role::System);
    assert!(req.messages[0]
        .content
        .starts_with("Summarize the transcript below"));
}

#[test]
fn single_message_passes_through_without_provider() {
    let provider = text_provider("ignored");
    let s = ProviderSummarizer::new(Some(provider.clone()), MODEL);
    let summary = s
        .summarize_messages(&[Message::user(" only this ")])
        .expect("summarize");
    assert_eq!(summary.text, "only this");
    assert_eq!(
        provider.requests().len(),
        0,
        "no provider call for one message"
    );
}

#[test]
fn empty_history_skips_provider() {
    let provider = text_provider("ignored");
    let s = ProviderSummarizer::new(Some(provider.clone()), MODEL);
    assert_eq!(s.summarize(&[]).expect("summarize"), "");
    assert_eq!(provider.requests().len(), 0);
    let (_, reported) = s.last_usage();
    assert!(!reported);
}

#[test]
fn no_provider_or_model_is_an_error() {
    let no_provider = ProviderSummarizer::new(None, MODEL);
    let err = no_provider.summarize_messages(&two_messages()).unwrap_err();
    assert_eq!(err.message(), "agent: summarizer has no provider");

    let no_model = ProviderSummarizer::new(Some(text_provider("x")), "");
    let err = no_model.summarize_messages(&two_messages()).unwrap_err();
    assert_eq!(err.message(), "agent: summarizer has no model");
}

#[test]
fn tool_call_in_stream_is_an_error() {
    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![Delta {
        tool_call: Some(ToolCallDelta {
            index: 0,
            id: "c".into(),
            name: "whatever".into(),
            args_fragment: "{}".into(),
        }),
        ..Default::default()
    }])]));
    let s = ProviderSummarizer::new(Some(provider.clone()), MODEL);
    let err = s.summarize_messages(&two_messages()).unwrap_err();
    assert_eq!(err.message(), ERR_SUMMARY_TOOL_CALL);
    assert_eq!(provider.close_count(), 1, "stream closed exactly once");
}

#[test]
fn empty_text_or_token_ceiling_is_an_error() {
    let empty = ProviderSummarizer::new(Some(text_provider("")), MODEL);
    assert_eq!(
        empty
            .summarize_messages(&two_messages())
            .unwrap_err()
            .message(),
        ERR_NO_SUMMARY_TEXT
    );

    let provider = Arc::new(ScriptedProvider::new(vec![Response::Deltas(vec![
        Delta {
            text: "partial".into(),
            ..Default::default()
        },
        Delta {
            stop_reason: Some("length".into()),
            ..Default::default()
        },
    ])]));
    let s = ProviderSummarizer::new(Some(provider), MODEL);
    assert_eq!(
        s.summarize_messages(&two_messages()).unwrap_err().message(),
        ERR_SUMMARY_TRUNCATED
    );
}

#[test]
fn out_of_order_history_is_rejected() {
    let s = ProviderSummarizer::new(Some(text_provider("x")), MODEL);
    let events = vec![
        {
            let mut e = Event::new(
                SESSION,
                EventType::USER_MESSAGE,
                Some(json!({"text": "a", "attachments": []})),
            );
            e.seq = 2;
            e
        },
        {
            let mut e = Event::new(
                SESSION,
                EventType::USER_MESSAGE,
                Some(json!({"text": "b", "attachments": []})),
            );
            e.seq = 2;
            e
        },
    ];
    let err = s.summarize(&events).unwrap_err();
    assert!(
        err.message().contains("out of sequence"),
        "unexpected error: {}",
        err.message()
    );
}

const SESSION: &str = "s_summarizer";
