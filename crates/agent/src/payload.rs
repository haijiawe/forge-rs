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

pub fn turn_end(reason: &str, steps: usize) -> (EventType, Value) {
    (
        EventType::TURN_END,
        json!({"reason": reason, "steps": steps}),
    )
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

pub fn error_event(message: &str, code: &str, retryable: bool) -> (EventType, Value) {
    let mut m = Map::new();
    m.insert("message".into(), json!(message));
    if !code.is_empty() {
        m.insert("code".into(), json!(code));
    }
    m.insert("retryable".into(), json!(retryable));
    (EventType::ERROR, Value::Object(m))
}
