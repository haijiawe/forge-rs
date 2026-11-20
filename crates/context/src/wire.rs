//! How a block is written into the log, and read back out of it.
//!
//! One event per block, written when the block is created. Nothing is updated
//! and nothing is deleted: a block that was a mistake is still in the log, and
//! the way to stop paying for it is to restore it, not to rewrite history.
//!
//! A block is still a single `context/compressed` event; there is no
//! begin/summary/commit trio. That three-event lock (start, summary, end) would
//! make a half-written block impossible to observe, but it also widens the
//! payload and the ledger's fold — so it is deliberately left out here as an
//! optional follow-up. Today a block is atomic by construction: the event either
//! parses into a valid [`Block`] or it is an error, and there is no intermediate
//! state a reader could catch.

use crate::block::Block;
use forge_core::event::{Event, EventType};
use forge_core::{Error, ErrorKind, Result};
use serde_json::{Map, Value};

/// The event type, spelled once. `EventType::CONTEXT_COMPRESSED` must equal
/// this, and the conformance corpus asserts the spelling.
pub const EVENT_TYPE: EventType = EventType::CONTEXT_COMPRESSED;

/// The one `surfaceOp.op` value this reader understands.
pub const SURFACE_REPLACE: &str = "replace";

/// How a compressed block is surfaced to the model.
///
/// This is an *optional* field on the `context/compressed` payload. Its absence
/// is [`SurfaceOp::Hide`], which is exactly the behaviour before the field
/// existed — so a log written by the old writer reads the same as it always
/// did. A `replace` op names the inclusive `seq` range it stands in for; the
/// reader turns that range into one summary message rather than a silent gap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SurfaceOp {
    /// Hide the block's range and put nothing in its place. The default.
    #[default]
    Hide,
    /// Replace `[start_seq, end_seq]` with the block's summary.
    Replace { start_seq: u64, end_seq: u64 },
}

/// A `context/compressed` record read back as both halves: the block it
/// describes and how the projection should surface it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressedRecord {
    pub block: Block,
    pub surface: SurfaceOp,
}

/// The `context/compressed` event for `block`, using the default `hide`
/// surface op (no `surfaceOp` member).
///
/// The payload carries the range and the summary. It does not carry the
/// originals: they are already in the log, a few records earlier, and copying
/// them here would make the log larger than the window it was meant to shrink.
pub fn context_compressed(session: impl Into<String>, block: &Block) -> Result<Event> {
    context_compressed_with_surface(session, block, SurfaceOp::Hide)
}

/// The same event, with an explicit surface op. `SurfaceOp::Hide` writes no
/// `surfaceOp` member, so the bytes are identical to [`context_compressed`].
pub fn context_compressed_with_surface(
    session: impl Into<String>,
    block: &Block,
    surface: SurfaceOp,
) -> Result<Event> {
    block.validate()?;
    let mut payload = Map::new();
    payload.insert("blockId".into(), Value::String(block.id.clone()));
    payload.insert("from".into(), Value::from(block.from));
    payload.insert("to".into(), Value::from(block.to));
    payload.insert("summary".into(), Value::String(block.summary.clone()));
    if block.messages > 0 {
        payload.insert("messages".into(), Value::from(block.messages));
    }
    if let SurfaceOp::Replace { start_seq, end_seq } = surface {
        validate_surface_range(start_seq, end_seq, block)?;
        let mut op = Map::new();
        op.insert("op".into(), Value::String(SURFACE_REPLACE.into()));
        op.insert("startSeq".into(), Value::from(start_seq));
        op.insert("endSeq".into(), Value::from(end_seq));
        payload.insert("surfaceOp".into(), Value::Object(op));
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

/// Reads both the block and its surface op out of an event. `None` when the
/// event is another type; an error when it is a malformed compressed record.
pub fn parse_record(event: &Event) -> Result<Option<CompressedRecord>> {
    if event.typ != EVENT_TYPE.as_str() {
        return Ok(None);
    }
    let payload = event.payload.as_ref().ok_or_else(|| {
        Error::new(
            ErrorKind::Invalid,
            format!("context: event {} has no payload", event.id),
        )
    })?;
    let block = decode(payload, event.seq)?;
    let surface = decode_surface_op(payload, event.seq)?;
    if let SurfaceOp::Replace { start_seq, end_seq } = surface {
        validate_surface_range(start_seq, end_seq, &block)?;
    }
    Ok(Some(CompressedRecord { block, surface }))
}

/// Reads only the surface op out of an event. `None` when the event is another
/// type; `Some(SurfaceOp::Hide)` when the payload has no `surfaceOp` member.
pub fn parse_surface_op(event: &Event) -> Result<Option<SurfaceOp>> {
    if event.typ != EVENT_TYPE.as_str() {
        return Ok(None);
    }
    let payload = event.payload.as_ref().ok_or_else(|| {
        Error::new(
            ErrorKind::Invalid,
            format!("context: event {} has no payload", event.id),
        )
    })?;
    decode_surface_op(payload, event.seq).map(Some)
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

/// Reads the optional `surfaceOp` member out of a bare payload.
///
/// Missing (or `null`) is `Hide`. When present it must be an object with
/// `op: "replace"` and numeric `startSeq`/`endSeq`: an unknown op is an error,
/// because the record was supposed to be understandable and silently treating a
/// typo as "hide" would lose the summary the writer meant to surface.
pub fn decode_surface_op(payload: &Value, seq: u64) -> Result<SurfaceOp> {
    let object = payload.as_object().ok_or_else(|| {
        Error::new(
            ErrorKind::Invalid,
            format!("context: block payload at seq {seq} is not an object"),
        )
    })?;
    match object.get("surfaceOp") {
        None | Some(Value::Null) => Ok(SurfaceOp::Hide),
        Some(Value::Object(op)) => {
            let name = match op.get("op") {
                Some(Value::String(text)) => text.clone(),
                Some(_) => {
                    return Err(Error::new(
                        ErrorKind::Invalid,
                        format!("context: surfaceOp field \"op\" at seq {seq} is not a string"),
                    ))
                }
                None => {
                    return Err(Error::new(
                        ErrorKind::Invalid,
                        format!("context: surfaceOp at seq {seq} has no \"op\""),
                    ))
                }
            };
            if name != SURFACE_REPLACE {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "context: surfaceOp op {name:?} at seq {seq} is not {SURFACE_REPLACE:?}"
                    ),
                ));
            }
            let start_seq = surface_number(op, "startSeq", seq)?;
            let end_seq = surface_number(op, "endSeq", seq)?;
            if start_seq == 0 {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("context: surfaceOp startSeq at seq {seq} must be at least 1 (got 0)"),
                ));
            }
            if end_seq < start_seq {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "context: surfaceOp range [{start_seq}..{end_seq}] at seq {seq} is inverted"
                    ),
                ));
            }
            Ok(SurfaceOp::Replace { start_seq, end_seq })
        }
        Some(_) => Err(Error::new(
            ErrorKind::Invalid,
            format!("context: surfaceOp at seq {seq} is not an object"),
        )),
    }
}

/// A surface op may only rewrite seqs the block itself covers. Replacing a
/// record the block does not summarize would surface a summary for a range the
/// ledger does not know about, and restoring would have no answer.
fn validate_surface_range(start_seq: u64, end_seq: u64, block: &Block) -> Result<()> {
    if start_seq == 0 {
        return Err(Error::new(
            ErrorKind::Invalid,
            "context: surfaceOp startSeq must be at least 1 (got 0)".to_string(),
        ));
    }
    if end_seq < start_seq {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("context: surfaceOp range [{start_seq}..{end_seq}] is inverted"),
        ));
    }
    if start_seq < block.from || end_seq > block.to {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!(
                "context: surfaceOp range [{start_seq}..{end_seq}] falls outside block {} range [{}..{}]",
                block.id, block.from, block.to
            ),
        ));
    }
    Ok(())
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

fn surface_number(object: &Map<String, Value>, name: &str, seq: u64) -> Result<u64> {
    match object.get(name) {
        Some(value) => value.as_u64().ok_or_else(|| {
            Error::new(
                ErrorKind::Invalid,
                format!("context: surfaceOp field {name:?} at seq {seq} is not a number"),
            )
        }),
        None => Err(Error::new(
            ErrorKind::Invalid,
            format!("context: surfaceOp at seq {seq} has no {name:?}"),
        )),
    }
}
