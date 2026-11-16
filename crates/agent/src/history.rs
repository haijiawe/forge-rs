//! Rebuilding the model's history from the event log.
//!
//! Invariant I1: there is exactly one copy of the truth — the append-only log.
//! Every turn re-reads it and re-projects it through `forge_session`; the agent
//! never keeps its own history between turns.

use forge_llm::{Message, ToolCall};
use forge_session::projection::{MESSAGE_ASSISTANT, MESSAGE_SUMMARY, MESSAGE_TOOL, MESSAGE_USER};
use forge_session::Snapshot;

/// Turn the projected snapshot into the model message list, with the (possibly
/// truncated) system prompt leading. Tool calls attached to an assistant
/// message by the projection are carried through verbatim.
pub fn to_llm_messages(snap: &Snapshot, system: &str) -> Vec<Message> {
    let mut out = Vec::with_capacity(snap.messages.len() + 1);
    if !system.is_empty() {
        out.push(Message::system(system));
    }
    for m in &snap.messages {
        match m.kind.as_str() {
            MESSAGE_USER => out.push(Message::user(&m.text)),
            MESSAGE_ASSISTANT => {
                let mut msg = Message::assistant(&m.text);
                msg.tool_calls = m
                    .tool_calls
                    .iter()
                    .map(|c| ToolCall {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        args: c.args.clone().unwrap_or_else(|| serde_json::json!({})),
                    })
                    .collect();
                out.push(msg);
            }
            MESSAGE_TOOL => out.push(Message::tool_result(&m.call_id, &m.text)),
            MESSAGE_SUMMARY => out.push(Message::system(&m.text)),
            _ => {} // errors and unknown kinds never reach the model
        }
    }
    out
}

/// The number of the next turn: one more than the last recorded turn, or 1.
pub fn next_turn(snap: &Snapshot) -> u32 {
    snap.turns.last().map(|t| t.number + 1).unwrap_or(1)
}

/// The number of the newest turn that has no end record, if any. This is how a
/// turn already opened by the HTTP layer (PromptRecorded) is adopted.
pub fn open_turn(snap: &Snapshot) -> u32 {
    match snap.turns.last() {
        Some(t) if t.end_seq == 0 => t.number,
        _ => 0,
    }
}
