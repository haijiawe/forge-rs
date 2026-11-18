//! The contract between the agent loop and a language-model provider.
//!
//! This file is owned by the consumer: it declares only what the loop needs.
//! Provider-specific knobs live in the provider's own options, never here.
//! A provider that needs a new method must first prove the agent uses it.

use std::fmt;
use std::time::Duration;

use forge_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who produced a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A model request to run a tool. Arguments are the model's own JSON; checking
/// them is the tool's job, not this crate's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default = "empty_object")]
    pub args: Value,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// A non-text piece of a message.
///
/// The provider wire format is where this matters: an OpenAI-compatible server
/// wants `{"role":"user","content":[{"type":"text",...},{"type":"image_url",...}]}`
/// while a plain message wants a bare string. `Message::parts` is therefore
/// skipped by serde — a message with no parts serializes exactly as it did
/// before this existed — and only the request builder looks at it.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    Text(String),
    /// Bytes rather than a URL: the harness stores attachments in its own blob
    /// store, so there is no URL to hand over. The request builder turns this
    /// into a `data:` URI.
    Image {
        media_type: String,
        data: Vec<u8>,
    },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn image(media_type: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self::Image {
            media_type: media_type.into(),
            data: data.into(),
        }
    }
}

/// One entry of the conversation.
///
/// `tool_call_id` is set on a `Tool` message; `tool_calls` on an `Assistant`
/// message that decided to call tools. `content` may be empty when tool calls
/// are present, but never when both are empty — providers reject that, and the
/// error would otherwise surface as a confusing 400 after a round trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Images and other non-text pieces. Never serialized here: this is the
    /// harness's own model of the message, and the JSON this struct produces is
    /// compared byte for byte against frozen fixtures.
    #[serde(skip)]
    pub parts: Vec<ContentPart>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            parts: Vec::new(),
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            parts: Vec::new(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            parts: Vec::new(),
        }
    }

    pub fn tool_result(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(id.into()),
            parts: Vec::new(),
        }
    }

    /// Attach non-text parts to this message. Chainable so a constructor keeps
    /// reading as one expression.
    pub fn with_parts(mut self, parts: Vec<ContentPart>) -> Self {
        self.parts = parts;
        self
    }

    /// Whether anything in this message needs the array form on the wire.
    pub fn has_parts(&self) -> bool {
        !self.parts.is_empty()
    }
}

/// A tool as described to the model. `schema` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// One model call. `None`/empty means "provider default".
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub stop: Vec<String>,
}

impl Request {
    /// Catch the mistakes that would otherwise cost a round trip and produce a
    /// provider-shaped error message. Error text matches the Go reference so
    /// cross-language fixture tests can compare it literally.
    pub fn validate(&self) -> Result<()> {
        if self.model.is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "llm: request has no model"));
        }
        if self.messages.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "llm: request has no messages",
            ));
        }
        for (i, m) in self.messages.iter().enumerate() {
            if m.role == Role::Tool && m.tool_call_id.as_deref().unwrap_or("").is_empty() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("llm: message {i} is a tool result without toolCallId"),
                ));
            }
            if m.content.is_empty() && m.tool_calls.is_empty() && !m.has_parts() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("llm: message {i} is empty"),
                ));
            }
            for (j, tc) in m.tool_calls.iter().enumerate() {
                if tc.id.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Invalid,
                        format!("llm: message {i} tool call {j} has no id"),
                    ));
                }
                if tc.name.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Invalid,
                        format!("llm: message {i} tool call {j} has no name"),
                    ));
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        for (i, t) in self.tools.iter().enumerate() {
            if t.name.is_empty() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("llm: tool {i} has no name"),
                ));
            }
            if !seen.insert(&t.name) {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("llm: duplicate tool {:?}", t.name),
                ));
            }
        }
        Ok(())
    }
}

/// What one call cost. Providers that omit it leave it zero; callers must not
/// assume it is present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, rename = "inputTokens")]
    pub input_tokens: i64,
    #[serde(default, rename = "outputTokens")]
    pub output_tokens: i64,
    #[serde(default, rename = "cacheReadTokens")]
    pub cache_read_tokens: i64,
    #[serde(default, rename = "cacheWriteTokens")]
    pub cache_write_tokens: i64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }

    pub fn total(&self) -> i64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

/// One fragment of a streamed tool call. Arguments arrive as text tagged with
/// an index; `crate::Aggregator` joins them into final `ToolCall`s.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub args_fragment: String,
}

/// One increment of a stream. More than one member may be set in a single
/// delta; callers must handle each independently.
///
/// `stop_reason` is the provider's own vocabulary ("stop", "tool_calls",
/// "length"). Normalising it into the harness vocabulary is the agent's job,
/// so this crate stays provider-agnostic.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Delta {
    pub text: String,
    pub reasoning: String,
    pub tool_call: Option<ToolCallDelta>,
    pub usage: Option<Usage>,
    pub stop_reason: Option<String>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.reasoning.is_empty()
            && self.tool_call.is_none()
            && self.usage.is_none()
            && self.stop_reason.is_none()
    }
}

/// An in-flight response. `recv` returns `Ok(None)` at a clean end of stream;
/// any error also ends the stream. `close` is idempotent and safe after an
/// error — the agent calls it from cancellation while the reader may be blocked.
pub trait Stream: Send {
    fn recv(&mut self) -> Result<Option<Delta>>;
    fn close(&mut self);
}

/// Starts a stream. No async runtime: a turn is one thread and the stream is
/// read on that thread, which keeps cancellation a single explicit token.
pub trait Provider: Send + Sync {
    fn stream(&self, req: &Request) -> Result<Box<dyn Stream>>;
}

/// A provider failure carrying the fields the retry policy needs. `body` is
/// truncated by the provider and is for logs only — never send it to the model.
#[derive(Debug, Clone)]
pub struct ProviderError {
    pub provider: String,
    pub code: String,
    pub status: i32,
    pub retryable: bool,
    pub retry_after: Duration,
    pub body: String,
    pub message: String,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "llm")?;
        if !self.provider.is_empty() {
            write!(f, " {}", self.provider)?;
        }
        if self.status != 0 {
            write!(f, ": status {}", self.status)?;
        }
        if !self.code.is_empty() {
            write!(f, ": {}", self.code)?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

/// Cancellation is never retryable: the caller asked for it, so it never
/// becomes a `ProviderError` in the first place.
pub fn retryable(err: &ProviderError) -> bool {
    err.retryable
}

/// The server-requested delay, or zero.
pub fn retry_after_of(err: &ProviderError) -> Duration {
    err.retry_after
}

/// Joins streamed tool-call fragments into final calls, preserving arrival
/// order by index and tolerating providers that emit `[0,1,0,1]`.
#[derive(Debug, Default)]
pub struct Aggregator {
    calls: Vec<ToolCallDelta>,
}

impl Aggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, delta: ToolCallDelta) {
        if let Some(slot) = self.calls.iter_mut().find(|c| c.index == delta.index) {
            if !delta.id.is_empty() {
                slot.id = delta.id;
            }
            if !delta.name.is_empty() {
                slot.name = delta.name;
            }
            slot.args_fragment.push_str(&delta.args_fragment);
        } else {
            self.calls.push(delta);
        }
    }

    /// Returns `(id, name, parsed args)`. Invalid JSON is an error: silently
    /// substituting `{}` would turn a malformed model response into a tool
    /// call with no arguments, which is indistinguishable from a real one.
    pub fn finish(&self) -> Result<Vec<ToolCall>> {
        let mut ordered: Vec<&ToolCallDelta> = self.calls.iter().collect();
        ordered.sort_by_key(|c| c.index);
        let mut out = Vec::with_capacity(ordered.len());
        for c in ordered {
            let args: Value = if c.args_fragment.trim().is_empty() {
                empty_object()
            } else {
                serde_json::from_str(&c.args_fragment)
                    .map_err(|e| Error::new(ErrorKind::Json, format!("llm: tool arguments: {e}")))?
            };
            out.push(ToolCall {
                id: c.id.clone(),
                name: c.name.clone(),
                args,
            });
        }
        Ok(out)
    }
}
