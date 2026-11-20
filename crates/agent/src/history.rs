//! Rebuilding the model's history from the event log.
//!
//! Invariant I1: there is exactly one copy of the truth — the append-only log.
//! Every turn re-reads it and re-projects it through `forge_session`; the agent
//! never keeps its own history between turns.

use std::collections::BTreeSet;

use forge_llm::{ContentPart, Message, ToolCall};
use forge_session::projection::{
    Message as ProjectedMessage, MESSAGE_ASSISTANT, MESSAGE_SUMMARY, MESSAGE_TOOL, MESSAGE_USER,
};
use forge_session::Snapshot;

/// Turn the projected snapshot into the model message list, with the (possibly
/// truncated) system prompt leading. Tool calls attached to an assistant
/// message by the projection are carried through verbatim.
pub fn to_llm_messages(snap: &Snapshot, system: &str) -> Vec<Message> {
    to_llm_messages_with_parts(snap, system, &|_| Vec::new())
}

/// The same list, but each user message may carry non-text parts — the images
/// named by its attachments.
///
/// `parts_for` is the seam, not the work: this crate knows which messages exist
/// and in what order, while the bytes live in the caller's blob store. Keeping
/// the lookup out of here is what stops the history builder from needing a data
/// directory. A message whose parts come back empty is encoded exactly as it was
/// before this function existed.
pub fn to_llm_messages_with_parts(
    snap: &Snapshot,
    system: &str,
    parts_for: &dyn Fn(&ProjectedMessage) -> Vec<ContentPart>,
) -> Vec<Message> {
    to_llm_messages_hiding(snap, system, parts_for, &|_| false)
}

/// The same list, with the seq ranges a compressed block hides left out.
///
/// `hidden` is the seam: the ledger lives with the surface that owns the log, so
/// this crate is told which seqs are behind a summary rather than shown one.
///
/// Hiding is **not** a plain filter, because a tool call and its result are two
/// records that only mean something together. Hiding one and keeping the other
/// produces a request every provider rejects: a tool message with no call, or a
/// call with no result. So the set is closed first — a hidden call takes its
/// result with it, and a hidden result takes its call — and only then applied.
pub fn to_llm_messages_hiding(
    snap: &Snapshot,
    system: &str,
    parts_for: &dyn Fn(&ProjectedMessage) -> Vec<ContentPart>,
    hidden: &dyn Fn(u64) -> bool,
) -> Vec<Message> {
    let hidden_seq = closed_hidden_set(snap, hidden);
    let mut out = Vec::with_capacity(snap.messages.len() + 1);
    if !system.is_empty() {
        out.push(Message::system(system));
    }
    for m in &snap.messages {
        if hidden_seq.contains(&m.seq) {
            continue;
        }
        match m.kind.as_str() {
            MESSAGE_USER => {
                let mut msg = Message::user(&m.text);
                let parts = parts_for(m);
                if !parts.is_empty() {
                    msg = msg.with_parts(parts);
                }
                out.push(msg);
            }
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
            MESSAGE_TOOL => {
                let mut msg = Message::tool_result(&m.call_id, &m.text);
                // A tool that fetched bytes had to leave them out of its text
                // (a tool message is text-only), so its parts come back the
                // same way a user message's do: through the caller's store.
                let parts = parts_for(m);
                if !parts.is_empty() {
                    msg = msg.with_parts(parts);
                }
                out.push(msg);
            }
            MESSAGE_SUMMARY => out.push(Message::system(&m.text)),
            _ => {} // errors and unknown kinds never reach the model
        }
    }
    out
}

/// The seqs to leave out, closed over tool-call pairs.
///
/// Two passes are enough: the first collects the records a block names, and the
/// second pulls in the partner of any tool call or result that is already out.
/// A third would find nothing new — a partner's partner is the record itself.
fn closed_hidden_set(snap: &Snapshot, hidden: &dyn Fn(u64) -> bool) -> BTreeSet<u64> {
    let mut seqs: BTreeSet<u64> = BTreeSet::new();
    let mut call_ids: BTreeSet<String> = BTreeSet::new();
    for m in &snap.messages {
        if !hidden(m.seq) {
            continue;
        }
        seqs.insert(m.seq);
        if m.kind == MESSAGE_ASSISTANT {
            for call in &m.tool_calls {
                call_ids.insert(call.id.clone());
            }
        }
        if m.kind == MESSAGE_TOOL && !m.call_id.is_empty() {
            call_ids.insert(m.call_id.clone());
        }
    }
    if call_ids.is_empty() {
        return seqs;
    }
    for m in &snap.messages {
        let partner = match m.kind.as_str() {
            MESSAGE_TOOL => call_ids.contains(&m.call_id),
            MESSAGE_ASSISTANT => m.tool_calls.iter().any(|call| call_ids.contains(&call.id)),
            _ => false,
        };
        if partner {
            seqs.insert(m.seq);
        }
    }
    seqs
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
