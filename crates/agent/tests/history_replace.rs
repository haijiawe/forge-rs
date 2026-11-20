//! The replace mode of the history builder: a compressed block stands in for a
//! run of records with one summary message, and the tool-call pairing is still
//! closed so the request a provider sees is always answerable.

use std::collections::BTreeSet;

use forge_agent::history::{to_llm_messages_hiding, to_llm_messages_replacing, SurfaceMode};
use forge_llm::Role;
use forge_session::projection::{AssistantCall, Message as ProjectedMessage};
use forge_session::Snapshot;

fn msg(kind: &str, seq: u64, text: &str) -> ProjectedMessage {
    ProjectedMessage {
        kind: kind.to_string(),
        seq,
        text: text.to_string(),
        ..Default::default()
    }
}

fn call(id: &str) -> AssistantCall {
    AssistantCall {
        id: id.to_string(),
        name: "echo".to_string(),
        args: Some(serde_json::json!({})),
    }
}

fn snapshot(messages: Vec<ProjectedMessage>) -> Snapshot {
    Snapshot {
        messages,
        ..Default::default()
    }
}

fn no_parts(_: &ProjectedMessage) -> Vec<forge_llm::ContentPart> {
    Vec::new()
}

/// Assert the two directions providers enforce: no tool result without a
/// matching call, and no call without a matching result.
fn assert_pairs_closed(out: &[forge_llm::Message]) {
    let mut calls: BTreeSet<String> = BTreeSet::new();
    let mut results: BTreeSet<String> = BTreeSet::new();
    for message in out {
        for c in &message.tool_calls {
            calls.insert(c.id.clone());
        }
        if let Some(id) = &message.tool_call_id {
            results.insert(id.clone());
        }
    }
    assert_eq!(
        calls, results,
        "every tool call must have exactly one result and vice versa"
    );
}

fn summary_user_count(out: &[forge_llm::Message]) -> usize {
    out.iter()
        .filter(|m| m.role == Role::User && m.content.contains("SUMMARY"))
        .count()
}

#[test]
fn replace_hides_the_selected_range_and_puts_one_summary_message_in_its_place() {
    let mut assistant_call = msg("assistant", 2, "");
    assistant_call.tool_calls = vec![call("c1")];
    let mut tool_result = msg("tool", 3, "result one");
    tool_result.call_id = "c1".to_string();

    let snap = snapshot(vec![
        msg("user", 1, "hello"),
        assistant_call,
        tool_result,
        msg("assistant", 4, "done"),
    ]);
    let hidden = |seq: u64| (2..=3).contains(&seq);

    let out = to_llm_messages_replacing(&snap, "SYS", &no_parts, &hidden, "SUMMARY");

    // The replaced records are gone: nothing in the window survives.
    assert!(out.iter().all(|m| m.content != "result one"), "{out:?}");
    assert!(out.iter().all(|m| m.tool_calls.is_empty()), "{out:?}");
    assert!(out.iter().all(|m| m.tool_call_id.is_none()), "{out:?}");

    // Exactly one summary message stands in, in the position the run occupied.
    assert_eq!(summary_user_count(&out), 1, "{out:?}");
    let hello = out
        .iter()
        .position(|m| m.content == "hello")
        .expect("hello");
    let summary = out
        .iter()
        .position(|m| m.content == "SUMMARY")
        .expect("summary");
    let done = out.iter().position(|m| m.content == "done").expect("done");
    assert!(hello < summary && summary < done, "order: {out:?}");

    assert_pairs_closed(&out);
}

#[test]
fn replacing_only_a_call_takes_its_result_with_it() {
    let mut assistant_call = msg("assistant", 2, "");
    assistant_call.tool_calls = vec![call("c1")];
    let mut tool_result = msg("tool", 3, "result one");
    tool_result.call_id = "c1".to_string();

    let snap = snapshot(vec![
        msg("user", 1, "hello"),
        assistant_call,
        tool_result,
        msg("assistant", 4, "done"),
    ]);
    // Only the call is selected; the result must ride along or the request is a
    // result with no call, which every provider rejects.
    let hidden = |seq: u64| seq == 2;

    let out = to_llm_messages_replacing(&snap, "", &no_parts, &hidden, "SUMMARY");

    assert!(out.iter().all(|m| m.content != "result one"), "{out:?}");
    assert_pairs_closed(&out);
    assert_eq!(summary_user_count(&out), 1, "{out:?}");
}

#[test]
fn an_empty_summary_does_not_replace_anything() {
    let mut assistant_call = msg("assistant", 2, "");
    assistant_call.tool_calls = vec![call("c1")];
    let mut tool_result = msg("tool", 3, "result one");
    tool_result.call_id = "c1".to_string();

    let snap = snapshot(vec![
        msg("user", 1, "hello"),
        assistant_call,
        tool_result,
        msg("assistant", 4, "done"),
    ]);
    let hidden = |seq: u64| (2..=3).contains(&seq);

    let out = to_llm_messages_replacing(&snap, "", &no_parts, &hidden, "   ");

    // The conservative direction: a block we cannot summarize is ignored and the
    // model sees every record, rather than a request that hides a range behind
    // nothing.
    assert_eq!(out.len(), 4, "all four records survive: {out:?}");
    assert!(out.iter().any(|m| m.content == "result one"), "{out:?}");
    assert_eq!(summary_user_count(&out), 0, "{out:?}");
    assert_pairs_closed(&out);
}

#[test]
fn an_empty_selection_adds_no_summary_message() {
    let snap = snapshot(vec![msg("user", 1, "hello"), msg("assistant", 2, "hi")]);
    let hidden = |_: u64| false;

    let out = to_llm_messages_replacing(&snap, "", &no_parts, &hidden, "SUMMARY");
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(summary_user_count(&out), 0, "{out:?}");
}

#[test]
fn hide_mode_is_unchanged_and_leaves_no_trace() {
    let mut assistant_call = msg("assistant", 2, "");
    assistant_call.tool_calls = vec![call("c1")];
    let mut tool_result = msg("tool", 3, "result one");
    tool_result.call_id = "c1".to_string();

    let snap = snapshot(vec![
        msg("user", 1, "hello"),
        assistant_call,
        tool_result,
        msg("assistant", 4, "done"),
    ]);
    let hidden = |seq: u64| (2..=3).contains(&seq);

    let out = to_llm_messages_hiding(&snap, "", &no_parts, &hidden);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(summary_user_count(&out), 0, "{out:?}");

    // The same selection through the explicit mode enum agrees with the shim.
    let explicit = forge_agent::history::to_llm_messages_with_mode(
        &snap,
        "",
        &no_parts,
        &hidden,
        SurfaceMode::Hide,
    );
    assert_eq!(out, explicit);
}
