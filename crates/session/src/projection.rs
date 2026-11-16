//! Folding an event log into the state a view renders.
//!
//! The projection is deliberately forgiving in one direction and strict in the
//! other: an unknown *event type* is counted and skipped (a newer writer may
//! know more than this build), while a malformed payload of a *known* type is
//! an error (the event was supposed to be understandable, so guessing would
//! silently change the state). Duplicate seqs are ignored rather than refused:
//! replaying a log twice is a supported recovery path.

use std::collections::HashMap;

use forge_core::{Error, ErrorKind, Event, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TURN_RUNNING: &str = "running";
pub const TURN_COMPLETE: &str = "complete";
pub const TURN_ABORTED: &str = "aborted";

pub const MESSAGE_USER: &str = "user";
pub const MESSAGE_ASSISTANT: &str = "assistant";
pub const MESSAGE_TOOL: &str = "tool";
pub const MESSAGE_SUMMARY: &str = "summary";
pub const MESSAGE_ERROR: &str = "error";

pub const CALL_PENDING: &str = "pending";
pub const CALL_OK: &str = "ok";
pub const CALL_ERROR: &str = "error";

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One message in the visible transcript. `tool_calls` is only populated for
/// assistant messages: it is what the model asked to run, before the result
/// arrives as a separate tool message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub kind: String,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub turn: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub step: u32,
    pub seq: u64,
    pub ts: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AssistantCall>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssistantCall {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub number: u32,
    pub status: String,
    pub steps: usize,
    pub start_seq: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub end_seq: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub end_reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub turn: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub step: u32,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error_code: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub orphan: bool,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub result_seq: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub turn: u32,
    pub seq: u64,
    pub ts: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Counters {
    pub turns: usize,
    pub steps: usize,
    pub messages: usize,
    pub tool_calls: usize,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub skipped: usize,
    pub duplicates: usize,
}

/// The folded state. Serialization order is the struct order; golden tests
/// compare canonical bytes, so key order in this declaration is not the
/// contract, the member set and values are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_id: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    pub ended: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub end_reason: String,
    pub last_seq: u64,
    pub turns: Vec<Turn>,
    pub messages: Vec<Message>,
    pub tool_calls: Vec<ToolCall>,
    pub pending_approvals: Vec<Approval>,
    pub counters: Counters,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SessionStartPayload {
    workspace: String,
    cwd: String,
    model: String,
    profile: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MessagePayload {
    text: String,
    content: String,
    model: String,
    usage: Option<UsagePayload>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UsagePayload {
    #[serde(rename = "inputTokens")]
    input_tokens_camel: i64,
    #[serde(rename = "outputTokens")]
    output_tokens_camel: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    input_tokens: i64,
    output_tokens: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TurnStartPayload {
    number: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TurnEndPayload {
    reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolCallPayload {
    #[serde(rename = "callId")]
    call_id_camel: String,
    #[serde(rename = "call_id")]
    call_id_snake: String,
    id: String,
    name: String,
    #[serde(rename = "tool")]
    tool_name: String,
    args: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolResultPayload {
    #[serde(rename = "callId")]
    call_id_camel: String,
    #[serde(rename = "call_id")]
    call_id_snake: String,
    id: String,
    ok: Option<bool>,
    output: String,
    text: String,
    content: String,
    error: String,
    #[serde(rename = "error_code")]
    error_code_snake: String,
    code: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ApprovalRequestPayload {
    #[serde(rename = "approvalId")]
    approval_id_camel: String,
    #[serde(rename = "approval_id")]
    approval_id_snake: String,
    id: String,
    #[serde(rename = "callId")]
    call_id_camel: String,
    #[serde(rename = "call_id")]
    call_id_snake: String,
    kind: String,
    title: String,
    detail: String,
    reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ApprovalDecisionPayload {
    #[serde(rename = "approvalId")]
    approval_id_camel: String,
    #[serde(rename = "approval_id")]
    approval_id_snake: String,
    id: String,
    #[serde(rename = "callId")]
    call_id_camel: String,
    #[serde(rename = "call_id")]
    call_id_snake: String,
    decision: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CompactionPayload {
    text: String,
    summary: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ErrorPayload {
    message: String,
    error: String,
}

/// Fold state so far. `apply` is the only mutator; `snapshot` is a deep copy so
/// callers cannot accidentally share the internal vectors.
#[derive(Default)]
pub struct Projection {
    skipped: usize,
    duplicates: usize,
    last_seq: u64,
    session_id: String,
    model: String,
    profile: String,
    cwd: String,
    ended: bool,
    end_reason: String,
    turns: Vec<Turn>,
    turn_index: HashMap<u32, usize>,
    messages: Vec<Message>,
    tool_calls: Vec<ToolCall>,
    call_index: HashMap<String, usize>,
    pending_approvals: Vec<Approval>,
    approval_index: HashMap<String, usize>,
    steps: usize,
    prompt_tokens: i64,
    completion_tokens: i64,
}

impl Projection {
    pub fn new() -> Projection {
        Projection::default()
    }

    /// Fold a whole log. Convenience for callers that already have the events
    /// in memory (fork validation, tests).
    pub fn fold(events: &[Event]) -> Result<Projection> {
        let mut projection = Projection::new();
        for event in events {
            projection.apply(event)?;
        }
        Ok(projection)
    }

    /// Fold a log straight to the renderable snapshot. This is the common
    /// read path; `fold` remains available when the intermediate projection's
    /// counters are needed.
    pub fn project(events: &[Event]) -> Result<Snapshot> {
        Ok(Projection::fold(events)?.snapshot())
    }

    pub fn apply(&mut self, event: &Event) -> Result<()> {
        // A duplicated seq is what a replay of the same log looks like. It is
        // not corruption; applying it twice would double every counter and
        // append every message again.
        if event.seq != 0 && event.seq <= self.last_seq {
            self.duplicates += 1;
            return Ok(());
        }
        if event.seq != 0 {
            self.last_seq = event.seq;
        }

        match event.typ.as_str() {
            "session/start" => {
                let payload: SessionStartPayload = decode(event)?;
                if !event.session.is_empty() {
                    self.session_id = event.session.clone();
                }
                self.model = first_non_empty(&[payload.model.as_str(), self.model.as_str()]);
                self.profile = first_non_empty(&[payload.profile.as_str(), self.profile.as_str()]);
                self.cwd = first_non_empty(&[
                    payload.workspace.as_str(),
                    payload.cwd.as_str(),
                    self.cwd.as_str(),
                ]);
            }
            "session/end" => {
                let payload: TurnEndPayload = decode(event)?;
                self.ended = true;
                self.end_reason = payload.reason;
            }
            "turn/start" => {
                let payload: TurnStartPayload = decode(event)?;
                self.start_turn(event, payload.number);
            }
            "turn/end" => {
                let payload: TurnEndPayload = decode(event)?;
                self.end_turn(event, &payload.reason);
            }
            "user/message" => {
                let payload: MessagePayload = decode(event)?;
                self.push_message(Message {
                    kind: MESSAGE_USER.to_string(),
                    turn: event.turn,
                    step: event.step,
                    seq: event.seq,
                    ts: event.ts,
                    text: first_non_empty(&[payload.text.as_str(), payload.content.as_str()]),
                    ..Message::default()
                });
            }
            "assistant/message" => {
                let payload: MessagePayload = decode(event)?;
                self.steps += 1;
                if let Some(&index) = self.turn_index.get(&event.turn) {
                    self.turns[index].steps += 1;
                }
                if let Some(usage) = &payload.usage {
                    self.prompt_tokens += first_non_zero(&[
                        usage.input_tokens_camel,
                        usage.prompt_tokens,
                        usage.input_tokens,
                    ]);
                    self.completion_tokens += first_non_zero(&[
                        usage.output_tokens_camel,
                        usage.completion_tokens,
                        usage.output_tokens,
                    ]);
                }
                if !payload.model.is_empty() {
                    self.model = payload.model.clone();
                }
                self.push_message(Message {
                    kind: MESSAGE_ASSISTANT.to_string(),
                    turn: event.turn,
                    step: event.step,
                    seq: event.seq,
                    ts: event.ts,
                    text: first_non_empty(&[payload.text.as_str(), payload.content.as_str()]),
                    ..Message::default()
                });
            }
            "tool/call" => {
                let payload: ToolCallPayload = decode(event)?;
                let call_id = first_non_empty(&[
                    payload.call_id_camel.as_str(),
                    payload.call_id_snake.as_str(),
                    payload.id.as_str(),
                ]);
                let call_id = if call_id.is_empty() {
                    format!("seq-{}", event.seq)
                } else {
                    call_id
                };
                if self.call_index.contains_key(&call_id) {
                    return Ok(());
                }
                let name = first_non_empty(&[payload.name.as_str(), payload.tool_name.as_str()]);
                let index = self.tool_calls.len();
                self.tool_calls.push(ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    turn: event.turn,
                    step: event.step,
                    seq: event.seq,
                    args: payload.args.clone(),
                    status: CALL_PENDING.to_string(),
                    ..ToolCall::default()
                });
                self.call_index.insert(call_id.clone(), index);
                self.attach_call(&call_id, &name, payload.args, event.turn, event.step);
            }
            "tool/result" => {
                let payload: ToolResultPayload = decode(event)?;
                let call_id = first_non_empty(&[
                    payload.call_id_camel.as_str(),
                    payload.call_id_snake.as_str(),
                    payload.id.as_str(),
                ]);
                let call_id = if call_id.is_empty() {
                    format!("seq-{}", event.seq)
                } else {
                    call_id
                };
                let error_code =
                    first_non_empty(&[payload.error_code_snake.as_str(), payload.code.as_str()]);
                let status = if !error_code.is_empty()
                    || !payload.error.is_empty()
                    || payload.ok == Some(false)
                {
                    CALL_ERROR
                } else {
                    CALL_OK
                };
                let mut output = first_non_empty(&[
                    payload.output.as_str(),
                    payload.text.as_str(),
                    payload.content.as_str(),
                ]);
                if output.is_empty() && status == CALL_ERROR {
                    output = payload.error.clone();
                }
                let is_error = status == CALL_ERROR;

                if let Some(&index) = self.call_index.get(&call_id) {
                    let call = &mut self.tool_calls[index];
                    call.status = status.to_string();
                    call.output = output.clone();
                    call.error_code = error_code;
                    call.result_seq = event.seq;
                    let name = call.name.clone();
                    self.push_message(Message {
                        kind: MESSAGE_TOOL.to_string(),
                        turn: event.turn,
                        step: event.step,
                        seq: event.seq,
                        ts: event.ts,
                        text: output,
                        call_id,
                        name,
                        is_error,
                        ..Message::default()
                    });
                } else {
                    // A result without its call is still shown. Hiding it
                    // would make "the tool ran but the log lost the request"
                    // indistinguishable from "nothing happened".
                    let index = self.tool_calls.len();
                    self.tool_calls.push(ToolCall {
                        call_id: call_id.clone(),
                        name: String::new(),
                        turn: event.turn,
                        step: event.step,
                        seq: event.seq,
                        status: status.to_string(),
                        output: output.clone(),
                        error_code,
                        orphan: true,
                        result_seq: event.seq,
                        ..ToolCall::default()
                    });
                    self.call_index.insert(call_id.clone(), index);
                    self.push_message(Message {
                        kind: MESSAGE_TOOL.to_string(),
                        turn: event.turn,
                        step: event.step,
                        seq: event.seq,
                        ts: event.ts,
                        text: output,
                        call_id,
                        is_error,
                        ..Message::default()
                    });
                }
            }
            "approval/request" => {
                let payload: ApprovalRequestPayload = decode(event)?;
                let id = first_non_empty(&[
                    payload.approval_id_camel.as_str(),
                    payload.id.as_str(),
                    payload.approval_id_snake.as_str(),
                ]);
                let id = if id.is_empty() {
                    format!("seq-{}", event.seq)
                } else {
                    id
                };
                if self.approval_index.contains_key(&id) {
                    return Ok(());
                }
                let call_id = first_non_empty(&[
                    payload.call_id_camel.as_str(),
                    payload.call_id_snake.as_str(),
                ]);
                let index = self.pending_approvals.len();
                self.pending_approvals.push(Approval {
                    id: id.clone(),
                    call_id,
                    kind: payload.kind,
                    title: payload.title,
                    detail: first_non_empty(&[payload.detail.as_str(), payload.reason.as_str()]),
                    turn: event.turn,
                    seq: event.seq,
                    ts: event.ts,
                });
                self.approval_index.insert(id, index);
            }
            "approval/decision" => {
                let payload: ApprovalDecisionPayload = decode(event)?;
                let id = first_non_empty(&[
                    payload.approval_id_camel.as_str(),
                    payload.id.as_str(),
                    payload.approval_id_snake.as_str(),
                ]);
                if !id.is_empty() {
                    self.remove_approval(&id);
                } else {
                    let call_id = first_non_empty(&[
                        payload.call_id_camel.as_str(),
                        payload.call_id_snake.as_str(),
                    ]);
                    if !call_id.is_empty() {
                        let ids: Vec<String> = self
                            .pending_approvals
                            .iter()
                            .filter(|approval| approval.call_id == call_id)
                            .map(|approval| approval.id.clone())
                            .collect();
                        for id in ids {
                            self.remove_approval(&id);
                        }
                    }
                }
            }
            "compaction/summary" => {
                // A compaction summary is a new visible beginning: messages
                // before it are hidden, but the counters that describe the
                // session's work are not rewritten.
                let payload: CompactionPayload = decode(event)?;
                self.messages.clear();
                self.push_message(Message {
                    kind: MESSAGE_SUMMARY.to_string(),
                    turn: event.turn,
                    seq: event.seq,
                    ts: event.ts,
                    text: first_non_empty(&[payload.text.as_str(), payload.summary.as_str()]),
                    ..Message::default()
                });
            }
            "error" => {
                let payload: ErrorPayload = decode(event)?;
                self.push_message(Message {
                    kind: MESSAGE_ERROR.to_string(),
                    turn: event.turn,
                    step: event.step,
                    seq: event.seq,
                    ts: event.ts,
                    text: first_non_empty(&[payload.message.as_str(), payload.error.as_str()]),
                    is_error: true,
                    ..Message::default()
                });
            }
            _ => {
                // Forward compatibility includes types this build knows in the
                // envelope but has no fold rule for (step/start, step/end,
                // llm/call): they are counted as skipped, exactly like a type
                // from a newer writer, because the projection cannot claim to
                // have understood them.
                self.skipped += 1;
            }
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            session_id: self.session_id.clone(),
            model: self.model.clone(),
            profile: self.profile.clone(),
            cwd: self.cwd.clone(),
            ended: self.ended,
            end_reason: self.end_reason.clone(),
            last_seq: self.last_seq,
            turns: self.turns.clone(),
            messages: self.messages.clone(),
            tool_calls: self.tool_calls.clone(),
            pending_approvals: self.pending_approvals.clone(),
            counters: Counters {
                turns: self.turns.len(),
                steps: self.steps,
                messages: self.messages.len(),
                tool_calls: self.tool_calls.len(),
                prompt_tokens: self.prompt_tokens,
                completion_tokens: self.completion_tokens,
                total_tokens: self.prompt_tokens + self.completion_tokens,
                skipped: self.skipped,
                duplicates: self.duplicates,
            },
        }
    }

    pub fn skipped(&self) -> usize {
        self.skipped
    }

    pub fn duplicates(&self) -> usize {
        self.duplicates
    }

    fn start_turn(&mut self, event: &Event, number: u32) {
        // A missing turn number means "the next one". Raise it above the last
        // explicit number as well: payload numbers may be sparse, and reusing a
        // number would merge two turns into one in `turn_index`.
        let number = if number == 0 {
            let next = self.turns.len() as u32 + 1;
            match self.turns.last() {
                Some(last) => next.max(last.number.saturating_add(1)),
                None => next,
            }
        } else {
            number
        };
        if let Some(&index) = self.turn_index.get(&number) {
            // A repeated turn/start is not a second turn: fill in a missing
            // start seq but keep the first one authoritative.
            if self.turns[index].start_seq == 0 {
                self.turns[index].start_seq = event.seq;
            }
            return;
        }
        let index = self.turns.len();
        self.turns.push(Turn {
            number,
            status: TURN_RUNNING.to_string(),
            steps: 0,
            start_seq: event.seq,
            ..Turn::default()
        });
        self.turn_index.insert(number, index);
    }

    fn end_turn(&mut self, event: &Event, reason: &str) {
        let status = if is_abort_reason(reason) {
            TURN_ABORTED
        } else {
            TURN_COMPLETE
        };
        let index = self.turn_index.get(&event.turn).copied().or_else(|| {
            self.turns
                .iter()
                .rposition(|turn| turn.status == TURN_RUNNING)
        });
        let index = match index {
            Some(index) => index,
            None => {
                // A turn/end without turn/start is repaired rather than
                // dropped: a log that lost only the opening record should not
                // lose the fact that work happened.
                self.start_turn(event, event.turn);
                self.turns.len() - 1
            }
        };
        let turn = &mut self.turns[index];
        turn.status = status.to_string();
        turn.end_seq = event.seq;
        turn.end_reason = reason.to_string();
    }

    fn attach_call(
        &mut self,
        call_id: &str,
        name: &str,
        args: Option<Value>,
        turn: u32,
        step: u32,
    ) {
        // Walk back to the assistant message this call belongs to. The first
        // assistant message found is the boundary: if it is for another
        // turn/step, the call was emitted outside a step and stays unattached.
        for message in self.messages.iter_mut().rev() {
            if message.kind != MESSAGE_ASSISTANT {
                continue;
            }
            if message.turn == turn && message.step == step {
                message.tool_calls.push(AssistantCall {
                    id: call_id.to_string(),
                    name: name.to_string(),
                    args,
                });
            }
            return;
        }
    }

    fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    fn remove_approval(&mut self, id: &str) {
        if let Some(index) = self.approval_index.remove(id) {
            self.pending_approvals.remove(index);
            // Re-index everything after the removed element; approvals are
            // few, so the linear pass is cheaper than a sorted map.
            for (new_index, approval) in self.pending_approvals.iter().enumerate() {
                self.approval_index.insert(approval.id.clone(), new_index);
            }
        }
    }
}

fn decode<T: DeserializeOwned + Default>(event: &Event) -> Result<T> {
    match &event.payload {
        None => Ok(T::default()),
        Some(Value::Null) => Ok(T::default()),
        Some(value) => serde_json::from_value(value.clone()).map_err(|e| {
            Error::new(
                ErrorKind::Invalid,
                format!(
                    "session: project {} event (seq {}): malformed payload: {e}",
                    event.typ, event.seq
                ),
            )
            .with_source(e)
        }),
    }
}

fn first_non_empty(values: &[&str]) -> String {
    values
        .iter()
        .find(|value| !value.is_empty())
        .map(|value| (*value).to_string())
        .unwrap_or_default()
}

fn first_non_zero(values: &[i64]) -> i64 {
    values
        .iter()
        .copied()
        .find(|value| *value != 0)
        .unwrap_or(0)
}

/// Reasons that mean the turn did not finish normally. The list is a deny-list
/// on purpose: an unknown reason read as "complete" would hide a failure,
/// while an unknown reason read as "aborted" is merely cautious.
pub fn is_abort_reason(reason: &str) -> bool {
    matches!(
        reason,
        "abort"
            | "aborted"
            | "cancel"
            | "cancelled"
            | "canceled"
            | "error"
            | "failed"
            | "timeout"
            | "interrupt"
            | "interrupted"
    )
}
