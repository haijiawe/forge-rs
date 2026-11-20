//! The JSON-RPC 2.0 wire shapes, and the framing rules on top of them.
//!
//! One object per line, nothing else. This is the same framing `hearth-mcp`
//! speaks, and it is deliberately blunt: a length-prefixed binary format would
//! make a plugin in Python or Node harder to write for no gain, because the
//! numbers already say the round trip costs the same either way
//! (`docs/09-platform-tier.md` §2.3).
//!
//! Every name here is the name on the wire. `PROTOCOL.md` documents exactly
//! these fields, and the reference plugin in `tests/reference-node/` is written
//! from that document alone — so a rename in this file without the same rename
//! there is a broken contract, not a refactor.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Kind, Result};

/// The protocol version this host speaks.
///
/// An integer, not a date: the plugin protocol is this crate's own contract and
/// has no MCP revision to track. A plugin that answers a different number is
/// refused rather than "upgraded", because a silent version mismatch is how a
/// half-working plugin ships.
pub const PROTOCOL_VERSION: u32 = 1;

/// The default cap on one reply, in bytes.
///
/// Larger than any tool output the harness prints, small enough that a plugin
/// stuck in a loop cannot exhaust memory before the cap notices. A plugin with
/// a legitimate 10 MB result is a plugin that should write a file and return
/// the path — that is what `present` is for.
pub const DEFAULT_MAX_BYTES: usize = 4 << 20;

/// The default call deadline.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// JSON-RPC error codes this host uses when it answers, and recognises when it
/// asks. A plugin may use any code in a reply; the host reports the number and
/// the message, it does not interpret them.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// "I cannot honour this request" — an unsupported protocol version, an
    /// unknown capability. Distinct from `INTERNAL_ERROR`, which means the
    /// plugin tried and broke.
    pub const REFUSED: i64 = -32000;
}

/// An outgoing request. One line, newline included.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Request {
        Request {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }

    /// The bytes to write, newline included.
    ///
    /// Serialising a `Request` cannot fail in practice (the fields are a string,
    /// a `u64` and a `Value`), but it is a `Result` because the alternative at
    /// the last call site would be an `expect` in library code — and the one
    /// case this must never do is take the host down because a plugin sent a
    /// number `serde_json` refuses to re-encode.
    pub fn to_line(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self).map_err(|error| {
            Error::new(
                Kind::ProtocolError,
                format!("cannot encode the request {}: {error}", self.method),
            )
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// An incoming message: a reply to us, or something the plugin volunteered.
#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<WireError>,
    #[serde(default)]
    pub params: Option<Value>,
}

impl Message {
    /// A reply carries `result` or `error`; a notification carries `method`.
    pub fn is_reply(&self) -> bool {
        self.result.is_some() || self.error.is_some()
    }

    /// Whether this is a notification (a request from the plugin, which has no
    /// reply). The host has no callback surface, so it logs and skips these.
    pub fn is_notification(&self) -> bool {
        self.method.is_some() && !self.is_reply()
    }

    /// The id as a `u64`, or `None` when it is absent or not a number.
    ///
    /// A quoted numeric id is accepted because JSON-RPC clients in the wild
    /// emit them and refusing would be a needless incompatibility — but it is
    /// only ever a match for the *same* number: an id that differs by one is a
    /// protocol error, never "close enough".
    pub fn numeric_id(&self) -> Option<u64> {
        match self.id.as_ref()? {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse().ok(),
            _ => None,
        }
    }
}

/// A JSON-RPC error object as a plugin sends it.
#[derive(Debug, Clone, Deserialize)]
pub struct WireError {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// Parse one line into a message, or say why it is not one.
///
/// The checks are the ones a reply can fail: valid JSON, an object, and an
/// explicit `"jsonrpc": "2.0"`. They are strict on purpose — a host that
/// accepted a shape because it was probably fine would be a host that reports
/// its own guesses as a plugin's answers.
pub fn parse_message(line: &[u8]) -> std::result::Result<Message, String> {
    let value: Value =
        serde_json::from_slice(line).map_err(|error| format!("not JSON: {error}"))?;
    let object: &Map<String, Value> = value
        .as_object()
        .ok_or_else(|| "not a JSON object".to_string())?;
    match object.get("jsonrpc").and_then(Value::as_str) {
        Some("2.0") => {}
        Some(other) => return Err(format!("unexpected JSON-RPC version {other:?}")),
        None => return Err("there is no \"jsonrpc\": \"2.0\" field".to_string()),
    }
    serde_json::from_value(value).map_err(|error| format!("not a JSON-RPC message: {error}"))
}

/// The first 200 bytes of a line, quoted for an error message.
///
/// Why exactly this much: enough to recognise the offending output (a stray
/// `console.log` line, a stack trace, a BOM), short enough that a 5 MB line
/// does not become a 5 MB error message in a log file.
pub const MALFORMED_HEAD_BYTES: usize = 200;

pub fn quote_head(line: &[u8]) -> String {
    let head = &line[..line.len().min(MALFORMED_HEAD_BYTES)];
    format!("{:?}", String::from_utf8_lossy(head))
}

/// A tool a plugin advertises. `schema` is a JSON Schema object describing
/// `arguments`; an absent schema is `{}`, never `null`, so a caller can index it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "empty_schema")]
    pub schema: Value,
}

fn empty_schema() -> Value {
    Value::Object(Map::new())
}

/// A prompt a plugin contributes. A prompt is text, addressed by name; the host
/// concatenates it into the system prompt, it does not run anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// What a plugin says it can do. The host calls nothing the plugin did not
/// claim: a plugin that does not list `before_turn` is not asked to run it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub prompts: bool,
    #[serde(default)]
    pub hooks: Vec<String>,
}

impl Capabilities {
    pub fn runs_hook(&self, phase: &str) -> bool {
        self.hooks.iter().any(|name| name == phase)
    }
}

/// What `initialize` returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handshake {
    #[serde(default)]
    pub protocol_version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// The result shape of `tools/call`.
///
/// A tool-level failure is data (`is_error == true`), not a Rust `Err`: "the
/// command ran and said 3" and "the plugin could not be reached" are different
/// facts, and the model has to see the first one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    pub output: String,
    #[serde(default, alias = "is_error")]
    pub is_error: bool,
}

/// The hooks the host will call, spelled once so a plugin cannot invent a phase
/// name and silently never be called.
pub const HOOK_PHASES: &[&str] = &["before_turn", "after_tool"];

pub fn is_hook_phase(phase: &str) -> bool {
    HOOK_PHASES.contains(&phase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_one_line_and_ends_in_a_newline() {
        let line = Request::new(7, "tools/list", None)
            .to_line()
            .expect("encode");
        assert!(line.ends_with(b"\n"));
        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        let text = std::str::from_utf8(&line).expect("utf8");
        assert_eq!(
            text.trim_end(),
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#
        );
    }

    #[test]
    fn params_are_included_only_when_present() {
        let with = Request::new(1, "tools/call", Some(serde_json::json!({"name": "echo"})))
            .to_line()
            .expect("encode");
        assert!(std::str::from_utf8(&with)
            .expect("utf8")
            .contains("\"params\""));
        let without = Request::new(1, "tools/list", None)
            .to_line()
            .expect("encode");
        assert!(!std::str::from_utf8(&without)
            .expect("utf8")
            .contains("\"params\""));
    }

    #[test]
    fn a_reply_is_recognised_by_result_or_error_never_by_method() {
        let reply: Message =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).expect("parse");
        assert!(reply.is_reply());
        assert_eq!(reply.numeric_id(), Some(1));
        let notification: Message =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"log","params":{}}"#).expect("parse");
        assert!(!notification.is_reply());
        assert!(notification.is_notification());
        assert_eq!(notification.numeric_id(), None);
        let failure: Message = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"boom"}}"#,
        )
        .expect("parse");
        assert!(failure.is_reply());
        assert!(failure.result.is_none());
        assert_eq!(failure.error.as_ref().map(|e| e.code), Some(-32603));
    }

    #[test]
    fn a_quoted_numeric_id_is_accepted_but_only_as_the_same_number() {
        let quoted: Message = serde_json::from_str(r#"{"id":"42","result":{}}"#).expect("parse");
        assert_eq!(quoted.numeric_id(), Some(42));
        let not_a_number: Message =
            serde_json::from_str(r#"{"id":"x","result":{}}"#).expect("parse");
        assert_eq!(not_a_number.numeric_id(), None);
    }

    #[test]
    fn a_line_that_is_not_json_rpc_is_refused_with_a_reason() {
        assert!(parse_message(b"console.log('hi')")
            .expect_err("not json")
            .contains("not JSON"));
        assert!(parse_message(b"[1,2,3]")
            .expect_err("not object")
            .contains("not a JSON object"));
        assert!(parse_message(br#"{"id":1,"result":{}}"#)
            .expect_err("no version")
            .contains("jsonrpc"));
        assert!(parse_message(br#"{"jsonrpc":"1.0","id":1,"result":{}}"#)
            .expect_err("wrong version")
            .contains("1.0"));
        assert!(parse_message(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#).is_ok());
    }

    #[test]
    fn a_malformed_line_is_quoted_no_further_than_two_hundred_bytes() {
        let long = format!("{}rest of a very long line", "x".repeat(500));
        let quoted = quote_head(long.as_bytes());
        assert!(!quoted.contains(&"x".repeat(201)), "{quoted}");
        assert!(quoted.contains(&"x".repeat(200)), "{quoted}");
        assert!(quoted.starts_with('"') && quoted.ends_with('"'), "{quoted}");
    }

    #[test]
    fn a_capability_list_decides_which_hooks_are_called() {
        let capabilities = Capabilities {
            tools: true,
            prompts: false,
            hooks: vec!["before_turn".to_string()],
        };
        assert!(capabilities.runs_hook("before_turn"));
        assert!(!capabilities.runs_hook("after_tool"));
        // A phase that is not in the closed set is not a hook, whatever the
        // plugin claims.
        assert!(!is_hook_phase("during_breakfast"));
        assert!(is_hook_phase("after_tool"));
    }

    #[test]
    fn a_tool_without_a_schema_gets_an_empty_object_not_null() {
        let tool: ToolSpec =
            serde_json::from_str(r#"{"name":"echo","description":"says it back"}"#).expect("parse");
        assert_eq!(tool.schema, serde_json::json!({}));
    }

    #[test]
    fn the_handshake_and_tool_result_use_the_camel_case_names_from_the_document() {
        let handshake: Handshake = serde_json::from_str(
            r#"{"protocolVersion":1,"name":"p","version":"1.0.0","capabilities":{"tools":true,"prompts":true,"hooks":["before_turn"]}}"#,
        )
        .expect("parse");
        assert_eq!(handshake.protocol_version, 1);
        assert!(handshake.capabilities.tools);
        assert_eq!(handshake.capabilities.hooks, vec!["before_turn"]);

        let output: ToolOutput =
            serde_json::from_str(r#"{"output":"42","isError":false}"#).expect("parse");
        assert_eq!(output.output, "42");
        assert!(!output.is_error);
    }
}
