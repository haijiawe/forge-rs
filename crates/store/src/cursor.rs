//! Opaque list cursor: Go's `base64.RawURLEncoding` of `{"u":...,"i":"..."}`.
//!
//! Keeping the wire format identical means a cursor minted by the Go store is
//! accepted here. Writing the codec by hand avoids adding a base64 crate that
//! is not in the workspace dependency budget.

use serde::{Deserialize, Serialize};

use crate::store::err_bad_cursor;
use forge_core::{Error, ErrorKind, Result};

const MAX_CURSOR_BYTES: usize = 512;
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

#[derive(Serialize, Deserialize)]
struct CursorJson {
    u: i64,
    i: String,
}

#[derive(Debug)]
pub(crate) struct Cursor {
    pub(crate) updated_at: i64,
    pub(crate) id: String,
}

fn encode_raw(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[((bits >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((bits >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((bits >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(bits & 0x3f) as usize] as char);
        }
    }
    out
}

fn decode_raw(text: &str) -> Result<Vec<u8>> {
    if text.len() > MAX_CURSOR_BYTES {
        return Err(err_bad_cursor("too long"));
    }
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let rem = text.len() % 4;
    if rem == 1 {
        return Err(err_bad_cursor("bad length"));
    }
    let mut values = Vec::with_capacity(text.len());
    for byte in text.bytes() {
        let Some(index) = ALPHABET.iter().position(|candidate| *candidate == byte) else {
            return Err(err_bad_cursor("invalid character"));
        };
        values.push(index as u8);
    }
    let mut out = Vec::with_capacity(values.len() * 3 / 4);
    for chunk in values.chunks(4) {
        let bits = (u32::from(chunk[0]) << 18)
            | (u32::from(chunk[1]) << 12)
            | (u32::from(*chunk.get(2).unwrap_or(&0)) << 6)
            | u32::from(*chunk.get(3).unwrap_or(&0));
        out.push((bits >> 16) as u8);
        if chunk.len() > 2 {
            out.push((bits >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(bits as u8);
        }
    }
    Ok(out)
}

pub(crate) fn encode_cursor(updated_at: i64, id: &str) -> Result<String> {
    let json = serde_json::to_vec(&CursorJson {
        u: updated_at,
        i: id.to_owned(),
    })
    .map_err(|e| Error::new(ErrorKind::Json, "store: cursor encode").with_source(e))?;
    Ok(encode_raw(&json))
}

pub(crate) fn decode_cursor(text: &str) -> Result<Cursor> {
    if text.len() > MAX_CURSOR_BYTES {
        return Err(err_bad_cursor("too long"));
    }
    let bytes = decode_raw(text)?;
    let decoded: CursorJson = serde_json::from_slice(&bytes)
        .map_err(|e| err_bad_cursor(&format!("invalid JSON: {e}")))?;
    if decoded.i.is_empty() {
        return Err(err_bad_cursor("empty id"));
    }
    Ok(Cursor {
        updated_at: decoded.u,
        id: decoded.i,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let cursor = encode_cursor(1_700_000_000_123, "s-42").expect("encode");
        let decoded = decode_cursor(&cursor).expect("decode");
        assert_eq!(decoded.updated_at, 1_700_000_000_123);
        assert_eq!(decoded.id, "s-42");
    }

    #[test]
    fn go_cursors_are_accepted() {
        // base64url of {"u":1,"i":"x"} with no padding.
        assert!(decode_cursor("eyJ1IjoxLCJpIjoieCJ9").is_ok());
    }

    #[test]
    fn malformed_cursors_are_rejected() {
        let too_long = "A".repeat(513);
        let bad = [
            "!!!",
            "e30",
            "eyJ1IjoxfQ",
            "eyJ1IjoxLCJpIjoifQ",
            too_long.as_str(),
        ];
        for token in bad {
            let error = decode_cursor(token).expect_err("must reject");
            assert!(crate::is_bad_cursor(&error), "{token}: {error:?}");
        }
    }
}
