//! SSE framing and chunk decoding for OpenAI-compatible streams.
//!
//! This module owns the wire-level details: the 8 MiB line limit, the
//! `text/event-stream` event boundary rules, and the mapping of a decoded
//! chunk into zero or more [`Delta`]s.
//!
//! [`Delta`]: crate::Delta

use std::error::Error as _;
use std::fmt;
use std::io::{self, Read};
use std::time::Duration;

use forge_core::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::Value;

use crate::{Delta, ProviderError, ToolCallDelta, Usage};

/// Explicit SSE line ceiling. The bufio.Scanner default (64 KiB) is far too
/// small for large `function.arguments` fragments.
pub const MAX_EVENT_BYTES: usize = 8 << 20;

/// Sentinel that terminates an OpenAI-compatible stream.
pub const DONE_TOKEN: &str = "[DONE]";

/// Cap applied to error bodies embedded in [`ProviderError::body`].
pub const MAX_ERROR_BODY: usize = 4 << 10;

pub const PROVIDER_NAME: &str = "openai";
pub const CODE_BAD_EVENT: &str = "bad_event";
pub const CODE_STREAM: &str = "stream_broken";
pub const CODE_INVALID_REQUEST: &str = "invalid_request";
pub const CODE_TRANSPORT: &str = "transport";
#[allow(dead_code)]
pub const CODE_CANCELED: &str = "canceled";
#[allow(dead_code)]
pub const CODE_TIMEOUT: &str = "timeout";
pub const CODE_CLOSED: &str = "stream_closed";

/// Text of the error returned after [`crate::Stream::close`].
pub const ERR_CLOSED: &str = "llm openai: stream closed";

/// Error used for a clean EOF that never delivered `[DONE]`.
pub const ERR_MISSING_DONE: &str = "stream ended without data: [DONE]: the response was cut";

/// A locally-owned carrier that lets [`ProviderError`] travel as the `source`
/// of a `forge_core::Error` without modifying the frozen contract.
#[derive(Debug)]
pub struct ProviderFailure(pub ProviderError);

impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ProviderFailure {}

/// Rebuild a `forge_core::Error` whose message and source both carry `pe`.
pub(crate) fn provider_error_with(kind: ErrorKind, pe: ProviderError) -> Error {
    let message = pe.to_string();
    Error::new(kind, message).with_source(ProviderFailure(pe))
}

/// Extract the [`ProviderError`] attached to an error, if any.
pub fn provider_error(err: &Error) -> Option<&ProviderError> {
    let mut current = err.source();
    while let Some(src) = current {
        if let Some(failure) = src.downcast_ref::<ProviderFailure>() {
            return Some(&failure.0);
        }
        current = src.source();
    }
    None
}

/// Build a [`ProviderError`] with the common `provider` field filled in.
pub(crate) fn pe(
    code: &str,
    status: i32,
    retryable: bool,
    retry_after: Duration,
    body: String,
    message: String,
) -> ProviderError {
    ProviderError {
        provider: PROVIDER_NAME.to_string(),
        code: code.to_string(),
        status,
        retryable,
        retry_after,
        body,
        message,
    }
}

/// Trim and, when necessary, truncate an error body at a UTF-8 boundary.
pub(crate) fn clip(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_ERROR_BODY {
        return String::from_utf8_lossy(bytes).trim().to_string();
    }
    let mut end = MAX_ERROR_BODY;
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    format!(
        "{}…(truncated)",
        String::from_utf8_lossy(&bytes[..end]).trim()
    )
}

/// An incremental `text/event-stream` reader.
///
/// Lines are read one byte at a time so that a Unicode code point split across
/// two TCP writes is never decoded until the whole line is present.
pub struct SseReader<R: Read> {
    inner: R,
    line: Vec<u8>,
    done: bool,
}

impl<R: Read> SseReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            line: Vec::with_capacity(4096),
            done: false,
        }
    }

    /// Read one raw line, stripping the trailing `\n` and one optional `\r`.
    /// `Ok(None)` means EOF with no partial line.
    fn read_line(&mut self) -> Result<Option<String>, io::Error> {
        self.line.clear();
        let mut byte = [0u8; 1];
        loop {
            match self.inner.read(&mut byte) {
                Ok(0) => {
                    if self.line.is_empty() {
                        return Ok(None);
                    }
                    return Ok(Some(take_line(&mut self.line)));
                }
                Ok(_) => {
                    if byte[0] == b'\n' {
                        return Ok(Some(take_line(&mut self.line)));
                    }
                    self.line.push(byte[0]);
                    if self.line.len() > MAX_EVENT_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("llm openai: SSE line exceeds {MAX_EVENT_BYTES} bytes"),
                        ));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    // The body wrapper decides when a timeout means cancellation.
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Return the `data` lines of the next event, or `None` at end of stream.
    pub fn next(&mut self) -> Result<Option<Vec<String>>, io::Error> {
        if self.done {
            return Ok(None);
        }
        let mut data: Vec<String> = Vec::new();
        loop {
            match self.read_line()? {
                None => {
                    self.done = true;
                    return Ok((!data.is_empty()).then_some(data));
                }
                Some(line) => {
                    if line.is_empty() {
                        if !data.is_empty() {
                            return Ok(Some(data));
                        }
                        // A bare blank line is a keep-alive.
                        continue;
                    }
                    if line.starts_with(':') {
                        continue;
                    }
                    let Some((field, value)) = line.split_once(':') else {
                        continue;
                    };
                    if field != "data" {
                        continue;
                    }
                    // Strip exactly one leading space, as the Go scanner does.
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    if value.is_empty() {
                        continue;
                    }
                    data.push(value.to_string());
                }
            }
        }
    }
}

fn take_line(buf: &mut Vec<u8>) -> String {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\r' {
        end -= 1;
    }
    let line = String::from_utf8_lossy(&buf[..end]).into_owned();
    buf.clear();
    line
}

/// Group data lines into JSON payloads.
///
/// A single line is returned as-is. Multiple lines are joined with `\n` when
/// that produces a valid JSON document; otherwise they are returned as
/// independent payloads.
pub fn payloads(data: &[String]) -> Vec<String> {
    if data.len() == 1 {
        return data.to_vec();
    }
    let joined = data.join("\n");
    if serde_json::from_str::<Value>(&joined).is_ok() {
        vec![joined]
    } else {
        data.to_vec()
    }
}

#[derive(Deserialize, Default)]
struct ApiUsage {
    #[serde(default)]
    prompt_tokens: i64,
    #[serde(default)]
    completion_tokens: i64,
    #[serde(default)]
    prompt_tokens_details: PromptDetails,
    #[serde(default)]
    prompt_cache_hit_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

#[derive(Deserialize, Default)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: i64,
}

#[derive(Deserialize, Default)]
struct Event {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<ApiUsage>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize, Default)]
struct Choice {
    #[serde(default)]
    index: i64,
    #[serde(default)]
    delta: DeltaWire,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct DeltaWire {
    #[serde(default)]
    content: String,
    #[serde(default)]
    reasoning_content: String,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    tool_calls: Vec<ToolCallWire>,
}

#[derive(Deserialize, Default)]
struct ToolCallWire {
    #[serde(default)]
    index: i64,
    #[serde(default)]
    id: String,
    #[serde(default)]
    function: FunctionWire,
}

#[derive(Deserialize, Default)]
struct FunctionWire {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Deserialize)]
struct ApiErrorWire {
    #[serde(default)]
    message: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    code: Value,
}

/// A minimal decoded API error; `code` is the raw `code` member when present.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub message: String,
    pub kind: String,
    pub code: String,
}

/// Decode the `error` member of a stream event or an HTTP error response.
pub fn decode_api_error(value: &Value) -> Option<ApiError> {
    let wire: ApiErrorWire = serde_json::from_value(value.clone()).ok()?;
    let code = match &wire.code {
        Value::Null | Value::Object(_) | Value::Array(_) => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if wire.message.is_empty() && wire.kind.is_empty() && code.is_empty() {
        return None;
    }
    Some(ApiError {
        message: wire.message,
        kind: wire.kind,
        code,
    })
}

/// The provider's preferred error code: raw `code`, else `type`.
pub fn api_error_code(err: &ApiError) -> String {
    if !err.code.is_empty() {
        err.code.clone()
    } else {
        err.kind.clone()
    }
}

fn to_usage(u: ApiUsage) -> Usage {
    let cache_read = if u.prompt_tokens_details.cached_tokens > 0 {
        u.prompt_tokens_details.cached_tokens
    } else if u.prompt_cache_hit_tokens > 0 {
        u.prompt_cache_hit_tokens
    } else {
        u.cache_read_input_tokens.max(0)
    };
    Usage {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: u.cache_creation_input_tokens.max(0),
    }
}

fn first_choice(choices: &[Choice]) -> Option<&Choice> {
    choices
        .iter()
        .find(|choice| choice.index == 0)
        .or_else(|| choices.first())
}

/// Decode one JSON payload into the deltas it carries.
///
/// Reasoning is emitted before text, and never mixed into [`Delta::text`].
/// A malformed payload becomes a non-retryable `bad_event` failure carrying a
/// clipped copy of the offending bytes.
pub fn decode_event(payload: &str) -> Result<Vec<Delta>, Error> {
    let event: Event = serde_json::from_str(payload).map_err(|e| {
        let error = pe(
            CODE_BAD_EVENT,
            0,
            false,
            Duration::ZERO,
            clip(payload.as_bytes()),
            e.to_string(),
        );
        provider_error_with(ErrorKind::Json, error)
    })?;

    if let Some(raw) = event.error.as_ref() {
        if !raw.is_null() {
            if let Some(api_error) = decode_api_error(raw) {
                let code = {
                    let c = api_error_code(&api_error);
                    if c.is_empty() {
                        "stream_error".to_string()
                    } else {
                        c
                    }
                };
                let error = pe(
                    &code,
                    0,
                    false,
                    Duration::ZERO,
                    clip(payload.as_bytes()),
                    api_error.message,
                );
                return Err(provider_error_with(ErrorKind::Invalid, error));
            }
        }
    }

    let mut out = Vec::new();
    if let Some(choice) = first_choice(&event.choices) {
        let delta = &choice.delta;
        if !delta.reasoning_content.is_empty() {
            out.push(Delta {
                reasoning: delta.reasoning_content.clone(),
                ..Default::default()
            });
        } else if !delta.reasoning.is_empty() {
            out.push(Delta {
                reasoning: delta.reasoning.clone(),
                ..Default::default()
            });
        }
        if !delta.content.is_empty() {
            out.push(Delta {
                text: delta.content.clone(),
                ..Default::default()
            });
        }
        for call in &delta.tool_calls {
            if call.id.is_empty()
                && call.function.name.is_empty()
                && call.function.arguments.is_empty()
            {
                continue;
            }
            out.push(Delta {
                tool_call: Some(ToolCallDelta {
                    index: call.index.max(0) as usize,
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    args_fragment: call.function.arguments.clone(),
                }),
                ..Default::default()
            });
        }
        if let Some(reason) = choice.finish_reason.as_deref() {
            if !reason.is_empty() {
                out.push(Delta {
                    stop_reason: Some(reason.to_string()),
                    ..Default::default()
                });
            }
        }
    }
    if let Some(usage) = event.usage {
        out.push(Delta {
            usage: Some(to_usage(usage)),
            ..Default::default()
        });
    }
    Ok(out)
}
