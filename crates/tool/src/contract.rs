//! The frozen tool boundary, ported from `forge-go/internal/tool/contract.go`.
//!
//! A `Call` carries model-supplied JSON and an explicit workspace. A `Result`
//! is data for the loop to record; a failing tool is not a failed Rust call.

use std::fmt;

use forge_llm::{ContentPart, ToolCall, ToolSpec};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Tool-result metadata. Kept as JSON so the event payload needs no conversion.
pub type Meta = Map<String, Value>;

pub const ERR_BAD_ARGS: &str = "bad_args";
pub const CODE_CANCELLED: &str = "cancelled";
pub const CODE_TIMEOUT: &str = "timeout";
pub const CODE_OUTSIDE_WORKSPACE: &str = "outside_workspace";
pub const CODE_NOT_FOUND: &str = "not_found";
pub const CODE_DENIED: &str = "denied";
pub const CODE_IO_ERROR: &str = "io_error";

/// One tool invocation. Workspace is supplied by the caller, never from CWD.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    #[serde(rename = "callId")]
    pub id: String,
    #[serde(rename = "tool")]
    pub name: String,
    #[serde(default = "empty_object")]
    pub args: Value,
    #[serde(default)]
    pub workspace: String,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

impl Call {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        args: Value,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            args,
            workspace: workspace.into(),
        }
    }

    /// Adapt a frozen provider `ToolCall` without changing its shape.
    pub fn from_tool_call(call: &ToolCall, workspace: impl Into<String>) -> Self {
        Self {
            id: call.id.clone(),
            name: call.name.clone(),
            args: call.args.clone(),
            workspace: workspace.into(),
        }
    }
}

/// The result of a tool call. `code` distinguishes expected failures from
/// ordinary text output; `is_error` is the coarse signal for the model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Result {
    pub output: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default, skip_serializing_if = "Meta::is_empty")]
    pub meta: Meta,
    /// Non-text parts this tool produced (currently images).
    ///
    /// A tool result is a text message on every chat-completions wire, so bytes
    /// cannot ride here — the loop turns them into a follow-up user message.
    /// Skipped by serde because a result is what gets written to the log, and the
    /// log records metadata, never bodies (`docs/contracts/events-v1.md` §4.0.1).
    #[serde(skip)]
    pub parts: Vec<ContentPart>,
}

impl Result {
    /// A successful textual result.
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            ..Self::default()
        }
    }

    /// A failure with a stable machine-readable code.
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            output: message.into(),
            is_error: true,
            code: code.into(),
            meta: Map::new(),
            truncated: false,
            parts: Vec::new(),
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }

    /// Attach non-text parts to this result.
    ///
    /// The loop, not this crate, decides what to do with them: a tool result is
    /// a text message on the wire, so bytes that must reach the model travel as
    /// a follow-up user message carrying exactly these parts.
    pub fn with_parts(mut self, parts: Vec<ContentPart>) -> Self {
        self.parts = parts;
        self
    }

    pub fn has_parts(&self) -> bool {
        !self.parts.is_empty()
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    pub fn is_ok(&self) -> bool {
        !self.is_error && self.code.is_empty()
    }
}

impl fmt::Display for Result {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.output)
    }
}

/// Decode a call's JSON arguments. Null and a missing object both mean `{}`.
pub fn decode_args<T: DeserializeOwned>(call: &Call) -> std::result::Result<T, Result> {
    let value = if call.args.is_null() {
        json!({})
    } else {
        call.args.clone()
    };
    serde_json::from_value(value).map_err(|err| {
        Result::error(
            ERR_BAD_ARGS,
            format!("{}: invalid arguments: {}", call.name, err),
        )
    })
}

/// The minimal surface every builtin implements.
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn call(&self, call: Call) -> Result;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_null_args_as_empty_object() {
        #[derive(Deserialize)]
        struct Empty {}
        let call = Call::new("c1", "tool", Value::Null, ".");
        let decoded: std::result::Result<Empty, Result> = decode_args(&call);
        assert!(decoded.is_ok());
    }
}
