//! Rebuilding the model's history from the event log.
//!
//! Invariant I1: there is exactly one copy of the truth — the append-only log.
//! Every turn re-reads it and re-projects it through `hearth_session`; the agent
//! never keeps its own history between turns.

use std::collections::BTreeSet;

use hearth_llm::{ContentPart, Message, ToolCall};
use hearth_session::projection::{
    Message as ProjectedMessage, MESSAGE_ASSISTANT, MESSAGE_SUMMARY, MESSAGE_TOOL, MESSAGE_USER,
};
use hearth_session::Snapshot;

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

/// How a compressed block changes the model's view.
///
/// The two modes own the same pairing rule; they differ only in what stands in
/// for the records they take out.
#[derive(Debug, Clone, Copy)]
pub enum SurfaceMode<'a> {
    /// The historic behaviour: the selected records are left out and nothing is
    /// put in their place.
    Hide,
    /// Replace the selected records with one `user` message carrying `summary`.
    ///
    /// An empty or whitespace-only summary means "do not replace": the block is
    /// ignored and every record stays visible. That is the conservative
    /// direction — send the model more rather than a request it cannot use.
    Replace { summary: &'a str },
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
    to_llm_messages_with_mode(snap, system, parts_for, hidden, SurfaceMode::Hide)
}

/// The same list, with the hidden records **replaced** by one summary message.
///
/// The summary is inserted where the first replaced record sat, so the turn
/// order the model reads is the same as the order it actually happened in. The
/// replaced set is closed over tool-call pairs exactly as [`SurfaceMode::Hide`]
/// does it, so a selected call brings its result and a selected result brings
/// its call: the request never carries half a pair.
pub fn to_llm_messages_replacing(
    snap: &Snapshot,
    system: &str,
    parts_for: &dyn Fn(&ProjectedMessage) -> Vec<ContentPart>,
    hidden: &dyn Fn(u64) -> bool,
    summary: &str,
) -> Vec<Message> {
    to_llm_messages_with_mode(
        snap,
        system,
        parts_for,
        hidden,
        SurfaceMode::Replace { summary },
    )
}

/// The one implementation both public shims call. `mode` decides whether the
/// closed set is dropped (`Hide`) or stood in for by a summary (`Replace`).
pub fn to_llm_messages_with_mode(
    snap: &Snapshot,
    system: &str,
    parts_for: &dyn Fn(&ProjectedMessage) -> Vec<ContentPart>,
    hidden: &dyn Fn(u64) -> bool,
    mode: SurfaceMode<'_>,
) -> Vec<Message> {
    let mut skip = closed_hidden_set(snap, hidden);
    let mut summary_message: Option<&str> = None;
    if let SurfaceMode::Replace { summary } = mode {
        if summary.trim().is_empty() || skip.is_empty() {
            // Nothing safe to stand in for: show the originals. A replacement
            // with no summary, or with an empty selection, would otherwise add
            // a message and hide nothing useful.
            skip.clear();
        } else {
            summary_message = Some(summary);
        }
    }
    let insert_at = summary_message.map(|_| insertion_index(snap, &skip));
    let mut out = Vec::with_capacity(snap.messages.len() + 2);
    if !system.is_empty() {
        out.push(Message::system(system));
    }
    for (index, m) in snap.messages.iter().enumerate() {
        if Some(index) == insert_at {
            if let Some(text) = summary_message {
                out.push(Message::user(text));
            }
        }
        if skip.contains(&m.seq) {
            continue;
        }
        push_message(&mut out, m, parts_for);
    }
    if let Some(text) = summary_message {
        if insert_at == Some(snap.messages.len()) {
            out.push(Message::user(text));
        }
    }
    out
}

/// Where the summary message goes: the index of the first record it replaces.
/// `skip` is non-empty whenever a summary is being inserted, so a matching
/// index always exists.
fn insertion_index(snap: &Snapshot, skip: &BTreeSet<u64>) -> usize {
    snap.messages
        .iter()
        .position(|m| skip.contains(&m.seq))
        .unwrap_or(snap.messages.len())
}

/// Renders one projected message into the model list.
fn push_message(
    out: &mut Vec<Message>,
    m: &ProjectedMessage,
    parts_for: &dyn Fn(&ProjectedMessage) -> Vec<ContentPart>,
) {
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
