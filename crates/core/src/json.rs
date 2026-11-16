//! Canonical JSON: the one byte sequence a value is allowed to have.
//!
//! Why this exists at all: golden files and `sha256` checks compare bytes, and
//! two encoders that disagree about key order would make every one of them
//! flaky. The rule here is "sorted keys, no whitespace, numbers keep their
//! text", which is also what the Go implementation produced, so a log written
//! by either build validates against the same fixture.

use crate::error::Result;
use serde::Serialize;
use serde_json::Value;

/// Encode a value with sorted keys and no insignificant whitespace.
///
/// Numbers keep the text they were parsed from because `serde_json` is built
/// with `arbitrary_precision`. That matters: a `u64` above 2^53 that went
/// through `f64` would come back as a different number, and a session log is
/// full of millisecond timestamps.
pub fn canonical<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let tree = serde_json::to_value(value)?;
    Ok(encode(&tree))
}

/// The same encoding for a value that is already a tree.
pub fn encode(value: &Value) -> String {
    // serde_json's Map is a BTreeMap unless `preserve_order` is on, so
    // serializing the tree *is* the sort. Do not "optimise" this into a
    // hand-written walker; the one property that matters comes from the map.
    serde_json::to_string(value).expect("a Value always serializes")
}

/// Parse and re-encode in one step, for readers that only want the stable form.
pub fn canonical_line(line: &str) -> Result<String> {
    let tree: Value = serde_json::from_str(line)?;
    Ok(encode(&tree))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_in_the_input_does_not_change_the_output() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
        assert_eq!(encode(&a), encode(&b));
        assert_eq!(encode(&a), r#"{"a":{"c":3,"d":2},"b":1}"#);
    }

    #[test]
    fn big_integers_keep_their_text() {
        // 2^53 + 1 is the smallest integer f64 cannot hold.
        let tree: Value = serde_json::from_str(r#"{"ts":9007199254740993}"#).unwrap();
        assert_eq!(encode(&tree), r#"{"ts":9007199254740993}"#);
    }

    #[test]
    fn unicode_is_emitted_as_utf8_not_escapes() {
        let tree: Value = serde_json::from_str(r#"{"t":"魔改版"}"#).unwrap();
        assert_eq!(encode(&tree), "{\"t\":\"魔改版\"}");
    }
}
