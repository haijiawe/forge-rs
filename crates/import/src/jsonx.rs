//! Small, total helpers for reading old JSON without guessing.
//!
//! Old writers put strings where objects belong, omit volatile fields, and add
//! members this build has never seen. Every accessor here returns `None`
//! instead of failing: an unreadable value is a dropped record, not a fatal
//! error, and unknown members are simply never looked at.

use serde_json::{Map, Value};

pub(crate) type Object = Map<String, Value>;

pub(crate) fn parse_object(text: &str) -> Option<Object> {
    let value: Value = serde_json::from_str(text).ok()?;
    value.as_object().cloned()
}

pub(crate) fn nested<'a>(obj: &'a Object, key: &str) -> Option<&'a Object> {
    obj.get(key).and_then(Value::as_object)
}

pub(crate) fn raw_string(obj: &Object, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub(crate) fn first_string(obj: &Object, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| raw_string(obj, key).filter(|value| !value.is_empty()))
}

pub(crate) fn raw_i64(obj: &Object, key: &str) -> Option<i64> {
    obj.get(key).and_then(Value::as_i64)
}

pub(crate) fn raw_bool(obj: &Object, key: &str) -> Option<bool> {
    obj.get(key).and_then(Value::as_bool)
}

pub(crate) fn raw_array<'a>(obj: &'a Object, key: &str) -> Option<&'a Vec<Value>> {
    obj.get(key).and_then(Value::as_array)
}

/// Decode a JSON string that contains another JSON document (old `arguments`).
pub(crate) fn decode_json_string(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// One content block of an old message. Only members the mapping reads are
/// carried; everything else in the block is deliberately ignored.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContentBlock {
    pub typ: String,
    pub text: String,
    pub id: String,
    pub name: String,
    pub args: String,
    pub tool_call_id: String,
    pub is_error: bool,
    pub content: Option<Value>,
}

pub(crate) fn parse_blocks(value: Option<&Value>) -> Vec<ContentBlock> {
    let Some(array) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(Value::as_object)
        .map(|block| ContentBlock {
            typ: raw_string(block, "type").unwrap_or_default(),
            text: raw_string(block, "text").unwrap_or_default(),
            id: raw_string(block, "id").unwrap_or_default(),
            name: raw_string(block, "name").unwrap_or_default(),
            args: raw_string(block, "arguments").unwrap_or_default(),
            tool_call_id: raw_string(block, "toolCallId").unwrap_or_default(),
            is_error: raw_bool(block, "isError").unwrap_or(false),
            content: block.get("content").cloned(),
        })
        .collect()
}

pub(crate) fn join_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter(|block| block.typ == "text")
        .map(|block| block.text.as_str())
        .collect()
}

pub(crate) fn has_block_type(blocks: &[ContentBlock], typ: &str) -> bool {
    blocks.iter().any(|block| block.typ == typ)
}

/// Clip at a UTF-8 character boundary; the v1 budgets are byte counts.
pub(crate) fn clip_bytes(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_owned(), true)
}
