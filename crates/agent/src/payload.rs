//! Payload builders for the frozen events-v1 vocabulary.
//!
//! Payload member sets are normative: a silently added or renamed member is a
//! compatibility break. Every builder here returns a `serde_json::Value` whose
//! object has exactly the members the contract lists.

use forge_core::EventType;
use forge_llm::Usage;
use serde_json::{json, Map, Value};

pub const STOP_END_TURN: &str = "end_turn";
pub const STOP_TOOL_USE: &str = "tool_use";
pub const STOP_CANCELLED: &str = "cancelled";
pub const STOP_MAX_TOKENS: &str = "max_tokens";
pub const STOP_ERROR: &str = "error";

pub const TURN_COMPLETED: &str = "completed";
pub const TURN_CANCELLED: &str = "cancelled";
pub const TURN_ERROR: &str = "error";

pub const CODE_CANCELLED: &str = "cancelled";
pub const MAX_SYSTEM_BYTES: usize = 8 << 10;

/// Map the provider's own stop vocabulary onto the harness vocabulary.
pub fn normalize_stop(stop: Option<&str>) -> &'static str {
    match stop.unwrap_or("") {
        "tool_calls" | "function_call" | "tool_use" => STOP_TOOL_USE,
        "length" | "max_tokens" => STOP_MAX_TOKENS,
        "cancelled" | "canceled" => STOP_CANCELLED,
        _ => STOP_END_TURN,
    }
}

/// A model that asks for tools while stopping normally is a tool-use step.
pub fn stop_with_calls(stop: &'static str, call_count: usize) -> &'static str {
    if call_count > 0 && stop == STOP_END_TURN {
        STOP_TOOL_USE
    } else {
        stop
    }
}

pub fn turn_reason(stop: &str) -> &'static str {
    match stop {
        STOP_END_TURN | STOP_MAX_TOKENS => TURN_COMPLETED,
        STOP_CANCELLED => TURN_CANCELLED,
        _ => TURN_ERROR,
    }
}

/// Usage as the events-v1 object: only members that carry a value, camelCase.
/// An all-zero usage is not recorded at all (the caller omits the member).
pub fn usage_payload(u: &Usage) -> Value {
    let mut m = Map::new();
    if u.input_tokens != 0 {
        m.insert("inputTokens".into(), json!(u.input_tokens));
    }
    if u.output_tokens != 0 {
        m.insert("outputTokens".into(), json!(u.output_tokens));
    }
    if u.cache_read_tokens != 0 {
        m.insert("cacheReadTokens".into(), json!(u.cache_read_tokens));
    }
    if u.cache_write_tokens != 0 {
        m.insert("cacheWriteTokens".into(), json!(u.cache_write_tokens));
    }
    Value::Object(m)
}

/// Clip a system prompt to `cap` bytes without splitting a UTF-8 rune.
/// Returns (kept, truncated).
pub fn clip_system(s: &str, cap: usize) -> (String, bool) {
    if s.len() <= cap {
        return (s.to_string(), false);
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// Convenience constructors that keep turn/step attribution in one place.
pub fn turn_start(number: u32) -> (EventType, Value) {
    (EventType::TURN_START, json!({"number": number}))
}

pub fn user_message(text: &str) -> (EventType, Value) {
    (
        EventType::USER_MESSAGE,
        json!({"text": text, "attachments": []}),
    )
}

/// Why a turn stopped, and how many steps it took.
///
/// `cancelledBy` is written only when a turn was cancelled **and** the token knew
/// why — the operator, a parent going away, a deadline, a hook, a disposed
/// owner. `cancelled` alone is not an answer: several paths stop a turn, and a
/// reader cannot tell a decision from a bug without the reason.
///
/// The member already existed in the contract (the recovery path writes
/// `cli:recovery`, imported logs carry the old implementation's words), so this
/// extends its vocabulary rather than inventing a second field that would say
/// the same thing under a different name.
pub fn turn_end(reason: &str, steps: usize, cancelled_by: Option<&str>) -> (EventType, Value) {
    let mut m = Map::new();
    m.insert("reason".into(), json!(reason));
    m.insert("steps".into(), json!(steps));
    if let Some(who) = cancelled_by {
        m.insert("cancelledBy".into(), json!(who));
    }
    (EventType::TURN_END, Value::Object(m))
}

pub fn step_start(index: u32, model: &str) -> (EventType, Value) {
    (
        EventType::STEP_START,
        json!({"index": index, "model": model}),
    )
}

pub fn llm_call(model: &str, messages: usize, tools: &[String]) -> (EventType, Value) {
    (
        EventType::LLM_CALL,
        json!({"model": model, "messages": messages, "tools": tools}),
    )
}

pub fn step_end(index: u32, reason: &str) -> (EventType, Value) {
    (
        EventType::STEP_END,
        json!({"index": index, "reason": reason}),
    )
}

/// One row per request the loop builds, before it is sent. `reason` says why a
/// fresh request exists: `initial` (the first of a turn), `resume` (adopting a
/// turn the HTTP layer already recorded), `change` (the tool set differs from
/// the previous request), or `series` (a continuation of the same series).
///
/// The message bodies are deliberately absent: they are already on earlier rows,
/// and copying them would make this row as large as the request itself.
pub fn request_header(
    reason: &str,
    model: &str,
    messages: usize,
    tools: &[String],
) -> (EventType, Value) {
    (
        EventType::REQUEST_HEADER,
        json!({"reason": reason, "model": model, "messages": messages, "tools": tools}),
    )
}

/// One row per request, saying what the request actually carried.
///
/// `request/header` answers "why is this request being made". This answers "what
/// is in it", which is the question you ask when two runs against the same log
/// disagree. Everything here is either a scalar or a digest: the system prompt
/// itself already sits on `session/start`, so repeating it per step would make
/// this row as large as the thing it describes.
///
/// `provider` and `provider_name` are the caller's, not ours: the loop does not
/// know what endpoint a `Provider` talks to, and inventing a name here would be
/// a guess written into a log.
// Nine arguments is over clippy's line, and the alternative is worse: a struct
// built at the one call site that reads its fields by position anyway. Every
// argument here is a distinct fact about the request, and folding any two into a
// tuple would hide which is which.
#[allow(clippy::too_many_arguments)]
pub fn request_context(
    provider: &str,
    model: &str,
    messages: usize,
    tools: usize,
    temperature: Option<f64>,
    max_tokens: Option<i64>,
    stop: &[String],
    system_bytes: usize,
    system_fingerprint: &str,
) -> (EventType, Value) {
    let mut m = Map::new();
    if !provider.is_empty() {
        m.insert("provider".into(), json!(provider));
    }
    m.insert("model".into(), json!(model));
    m.insert("messages".into(), json!(messages));
    m.insert("tools".into(), json!(tools));
    if let Some(temperature) = temperature {
        m.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = max_tokens {
        m.insert("maxTokens".into(), json!(max_tokens));
    }
    if !stop.is_empty() {
        m.insert("stop".into(), json!(stop));
    }
    m.insert("systemBytes".into(), json!(system_bytes));
    m.insert("systemFingerprint".into(), json!(system_fingerprint));
    (EventType::REQUEST_CONTEXT, Value::Object(m))
}

/// One row per provider attempt that did **not** become the visible answer.
///
/// The successful attempt still lands as exactly one `assistant/message`; this
/// row exists so a retried or abandoned attempt is on the record instead of
/// leaving a turn that looks like a single call.
pub fn assistant_attempt(
    attempt: u32,
    outcome: &str,
    message: &str,
    code: &str,
    retryable: bool,
) -> (EventType, Value) {
    let mut m = Map::new();
    m.insert("attempt".into(), json!(attempt));
    m.insert("outcome".into(), json!(outcome));
    m.insert("retryable".into(), json!(retryable));
    if !message.is_empty() {
        m.insert("message".into(), json!(message));
    }
    if !code.is_empty() {
        m.insert("code".into(), json!(code));
    }
    (EventType::ASSISTANT_ATTEMPT, Value::Object(m))
}

pub fn error_event(message: &str, code: &str, retryable: bool) -> (EventType, Value) {
    let mut m = Map::new();
    m.insert("message".into(), json!(message));
    if !code.is_empty() {
        m.insert("code".into(), json!(code));
    }
    m.insert("retryable".into(), json!(retryable));
    (EventType::ERROR, Value::Object(m))
}
