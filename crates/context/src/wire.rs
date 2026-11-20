//! How a block is written into the log, and read back out of it.
//!
//! One event per block, written when the block is created. Nothing is updated
//! and nothing is deleted: a block that was a mistake is still in the log, and
//! the way to stop paying for it is to restore it, not to rewrite history.

use crate::block::Block;
use forge_core::event::{Event, EventType};
use forge_core::{Error, ErrorKind, Result};
use serde_json::{Map, Value};

/// The event type, spelled once. `EventType::CONTEXT_COMPRESSED` must equal
/// this, and the conformance corpus asserts the spelling.
pub const EVENT_TYPE: EventType = EventType::CONTEXT_COMPRESSED;

/// The `context/compressed` event for `block`.
///
/// The payload carries the range and the summary. It does not carry the
/// originals: they are already in the log, a few records earlier, and copying
/// them here would make the log larger than the window it was meant to shrink.
pub fn context_compressed(session: impl Into<String>, block: &Block) -> Result<Event> {
    block.validate()?;
    let mut payload = Map::new();
    payload.insert("blockId".into(), Value::String(block.id.clone()));
    payload.insert("from".into(), Value::from(block.from));
    payload.insert("to".into(), Value::from(block.to));
    payload.insert("summary".into(), Value::String(block.summary.clone()));
    if block.messages > 0 {
        payload.insert("messages".into(), Value::from(block.messages));
    }
    Ok(Event::new(
        session,
        EVENT_TYPE,
        Some(Value::Object(payload)),
    ))
}

/// Reads a block out of an event, or `None` when the event is another type.
///
/// Only the type decides whether the payload is read. A `context/compressed`
/// record with a missing or mistyped field is an error: the type is known, so
/// there is nothing to be tolerant *of*.
pub fn parse_block(event: &Event) -> Result<Option<Block>> {
    if event.typ != EVENT_TYPE.as_str() {
        return Ok(None);
    }
    let payload = event.payload.as_ref().ok_or_else(|| {
        Error::new(
            ErrorKind::Invalid,
            format!("context: event {} has no payload", event.id),
        )
    })?;
    decode(payload, event.seq).map(Some)
}

/// Reads a block out of a bare payload. Split out so tests and the tool layer
/// can validate a payload before it is ever appended.
pub fn decode(payload: &Value, seq: u64) -> Result<Block> {
    let object = payload.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::Invalid,
            format!("context: block payload at seq {seq} is not an object"),
        )
    })?;
    let block_id = string_field(object, "blockId", seq)?;
    let summary = string_field(object, "summary", seq)?;
    let from = number_field(object, "from", seq)?;
    let to = number_field(object, "to", seq)?;
    let messages = match object.get("messages") {
        None | Some(Value::Null) => 0,
        Some(value) => value.as_u64().ok_or_else(|| {
            Error::new(
                ErrorKind::Invalid,
                format!("context: block field \"messages\" at seq {seq} is not a number"),
            )
        })? as u32,
    };
    let block = Block::new(block_id, from, to, summary)?.with_messages(messages);
    block.validate()?;
    Ok(block)
}

fn string_field(object: &Map<String, Value>, name: &str, seq: u64) -> Result<String> {
    match object.get(name) {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(Error::new(
            ErrorKind::Invalid,
            format!("context: block field {name:?} at seq {seq} is not a string"),
        )),
        None => Err(Error::new(
            ErrorKind::Invalid,
            format!("context: block at seq {seq} has no {name:?}"),
        )),
    }
}

fn number_field(object: &Map<String, Value>, name: &str, seq: u64) -> Result<u64> {
    match object.get(name) {
        Some(value) => value.as_u64().ok_or_else(|| {
            Error::new(
                ErrorKind::Invalid,
                format!("context: block field {name:?} at seq {seq} is not a number"),
            )
        }),
        None => Err(Error::new(
            ErrorKind::Invalid,
            format!("context: block at seq {seq} has no {name:?}"),
        )),
    }
}
