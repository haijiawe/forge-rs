//! The transcript summarizer (port of forge-v2/internal/agent/summarizer.go).
//!
//! Compaction replaces a log range with one system message. The summarizer is a
//! strict reader: no tools, no retries, no fallback. A tool call in the stream
//! is an error, a truncated stop is an error, and thinking markers are stripped
//! before the text is written — a summary must never smuggle the model's
//! private scratchpad into the durable record.

use std::sync::{Arc, Mutex};

use forge_core::{Error, ErrorKind, Event, Result};
use forge_llm::{Delta, Message, Provider, Request, Role, Stream, Usage};
use forge_session::{Projection, Snapshot};

use crate::cancel::Cancel;
use crate::err::is_cancelled;
use crate::history::to_llm_messages;

/// Frozen instruction, byte-for-byte from the Go reference. It is part of the
/// contract: a different instruction produces a different (non-replayable)
/// summary for the same events.
const SUMMARY_SYSTEM_PROMPT: &str = r#"Summarize the transcript below for a later turn of the same session. Write only the summary: no preamble, no headings beyond a short numbered list, and no closing offer.

Record, in this order:
1. What the user asked for, including every constraint, preference and scope limit they stated.
2. Decisions already made, each with the reason the transcript gives for it.
3. The current state of the work: what is done, what is in progress, and the files, commands, tools and identifiers that matter, copied exactly as they appear in the transcript.
4. Unresolved questions, open problems and known failures, quoted verbatim.

Rules:
- Use only facts present in the transcript. Do not complete, tidy or explain anything the transcript leaves unexplained.
- Do not invent decisions: a proposal, an option that was discarded, and a decision that was made are different things.
- Do not invent file paths, function names, commands, error messages or numbers. If the transcript does not contain one, say it was not recorded.
- Preserve unresolved questions verbatim; do not answer them and do not paraphrase them.
- Do not give advice, opinions or next steps; this is a record, not a continuation of the work.
- Write in the language of the transcript. If it mixes languages, write in the one most of it uses."#;

/// The summary byte budget when `max_bytes <= 0`. This is a copy of Go's
/// `session.DefaultSummaryBytes` (`forge-v2/internal/session/compact.go:513`),
/// which the frozen contract `m4-surface.md` §4 pins at 2048. It must not drift
/// from the oracle: a larger default changes what a resumed session remembers.
pub const DEFAULT_SUMMARY_BYTES: usize = 2048;

pub const ERR_NO_SUMMARY_TEXT: &str = "agent: summarizer produced no text";
pub const ERR_SUMMARY_TRUNCATED: &str = "agent: summarizer stopped at the token ceiling";
pub const ERR_SUMMARY_TOOL_CALL: &str =
    "agent: summarizer stream carried a tool call, but the summarizer offers no tools";

/// The summary and the usage the call consumed (reported when the provider sent
/// a usage delta). Callers use both for accounting.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub text: String,
    pub usage: Usage,
    pub usage_reported: bool,
}

/// Summarizes a projected log range through a provider. Thread-safe: the usage
/// counters are behind a mutex, so one instance can serve concurrent calls.
pub struct ProviderSummarizer {
    provider: Option<Arc<dyn Provider>>,
    model: String,
    max_bytes: usize,
    cancel: Cancel,
    usage: Mutex<Usage>,
    usage_reported: Mutex<bool>,
}

impl ProviderSummarizer {
    pub fn new(provider: Option<Arc<dyn Provider>>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            max_bytes: 0,
            cancel: Cancel::default(),
            usage: Mutex::new(Usage::default()),
            usage_reported: Mutex::new(false),
        }
    }

    /// Override the byte budget (0 selects [`DEFAULT_SUMMARY_BYTES`]).
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// A handle callers use to cancel an in-flight summarization.
    pub fn canceller(&self) -> Cancel {
        self.cancel.clone()
    }

    /// The last call's usage and whether the provider reported it.
    pub fn last_usage(&self) -> (Usage, bool) {
        let u = self.usage.lock().map(|g| *g).unwrap_or_default();
        let reported = self.usage_reported.lock().map(|g| *g).unwrap_or(false);
        (u, reported)
    }

    /// Fold an ordered event range into messages, then summarize them.
    pub fn summarize(&self, events: &[Event]) -> Result<String> {
        let msgs = summary_history(events)?;
        let sum = self.summarize_messages(&msgs)?;
        Ok(sum.text)
    }

    /// Summarize messages the caller already projected. Never retries, never
    /// falls back to local text, never offers tools.
    pub fn summarize_messages(&self, msgs: &[Message]) -> Result<Summary> {
        if msgs.is_empty() {
            self.record_usage(Usage::default(), false);
            return Ok(Summary::default());
        }
        if msgs.len() == 1 {
            self.record_usage(Usage::default(), false);
            return Ok(Summary {
                text: clip_summary(msgs[0].content.trim(), self.budget()),
                usage: Usage::default(),
                usage_reported: false,
            });
        }
        let provider = match &self.provider {
            Some(p) => p,
            None => {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    "agent: summarizer has no provider",
                ))
            }
        };
        if self.model.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "agent: summarizer has no model",
            ));
        }
        let req = Request {
            model: self.model.clone(),
            messages: summary_messages(msgs),
            tools: Vec::new(),
            ..Request::default()
        };
        if req.messages.len() == 1 {
            self.record_usage(Usage::default(), false);
            return Ok(Summary::default());
        }
        req.validate()?;
        let stream = match provider.stream(&req) {
            Ok(s) => s,
            Err(e) => {
                self.record_usage(Usage::default(), false);
                return Err(e);
            }
        };
        let sum = self.consume_and_finish(stream);
        // A tool call or parse failure still reports usage that arrived before it.
        self.record_usage(sum.usage, sum.usage_reported);
        match sum.result {
            Ok(text) => Ok(Summary {
                text,
                usage: sum.usage,
                usage_reported: sum.usage_reported,
            }),
            Err(e) => Err(e),
        }
    }

    // ---- internals --------------------------------------------------------

    fn budget(&self) -> usize {
        if self.max_bytes > 0 {
            self.max_bytes
        } else {
            DEFAULT_SUMMARY_BYTES
        }
    }

    fn record_usage(&self, usage: Usage, reported: bool) {
        if let Ok(mut u) = self.usage.lock() {
            *u = usage;
        }
        if let Ok(mut r) = self.usage_reported.lock() {
            *r = reported;
        }
    }

    /// Single stream, closed exactly once, no retries. Reasoning is ignored; a
    /// tool call aborts with [`ERR_SUMMARY_TOOL_CALL`].
    fn consume_and_finish(&self, stream: Box<dyn Stream>) -> SummarizeOut {
        let mut closer = SummarizerClose {
            stream,
            closed: false,
        };
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut usage_reported = false;
        let mut stop_reason: Option<String> = None;
        let mut early_error: Option<Error> = None;

        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            match closer.stream.recv() {
                Ok(Some(Delta {
                    text: fragment,
                    usage: u,
                    tool_call,
                    stop_reason: stop,
                    ..
                })) => {
                    if let Some(tc) = tool_call {
                        // Keep any text that arrived first as the source of a
                        // useful message, but never treat the call as valid.
                        let _ = tc;
                        early_error = Some(Error::new(ErrorKind::Refused, ERR_SUMMARY_TOOL_CALL));
                        break;
                    }
                    if let Some(u) = u {
                        usage.add(u);
                        usage_reported = true;
                    }
                    text.push_str(&fragment);
                    if stop.is_some() {
                        stop_reason = stop;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    if !is_cancelled(&e) {
                        // A stream error is deferred: the earlier Go code
                        // returns the error while keeping reported usage.
                        early_error = Some(e);
                    }
                    break;
                }
            }
        }

        let result = if let Some(e) = early_error {
            Err(e)
        } else {
            finish(&text, stop_reason.as_deref(), self.budget())
        };
        SummarizeOut {
            result,
            usage,
            usage_reported,
        }
    }
}

// ---- free helpers (ported verbatim) ---------------------------------------

/// Build the messages the summarizer sends: the frozen system instruction,
/// followed by role/content/tool-call-id only. Tool calls and reasoning never
/// reach the summarizer's model.
fn summary_messages(msgs: &[Message]) -> Vec<Message> {
    let mut out = Vec::with_capacity(msgs.len() + 1);
    out.push(Message::system(SUMMARY_SYSTEM_PROMPT));
    for m in msgs {
        if m.content.trim().is_empty() {
            continue;
        }
        if m.role == Role::Tool && m.tool_call_id.as_deref().unwrap_or("").is_empty() {
            continue;
        }
        let mut clean = Message {
            role: m.role,
            content: m.content.clone(),
            tool_calls: Vec::new(),
            tool_call_id: m.tool_call_id.clone(),
        };
        // The frozen role enum is exhaustive, so no unknown-role filter is
        // needed; the Go check existed only for JSON-decoded messages.
        if clean.role == Role::Assistant {
            clean.tool_calls.clear();
        }
        out.push(clean);
    }
    out
}

/// Fold events in strict log order. A gap or out-of-order seq is fatal: the
/// summarizer must not silently compress a partial range.
fn summary_history(events: &[Event]) -> Result<Vec<Message>> {
    let mut prev: u64 = 0;
    for e in events {
        if e.seq == 0 {
            continue;
        }
        if e.seq <= prev {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "agent: summarizer history is out of sequence at event {} (seq {} after seq {}); refusing to summarize a partial range",
                    e.id, e.seq, prev
                ),
            ));
        }
        prev = e.seq;
    }
    let mut proj = Projection::new();
    for e in events {
        proj.apply(e)?;
    }
    let snap: Snapshot = proj.snapshot();
    Ok(to_llm_messages(&snap, ""))
}

/// Strip both the inline and XML-style thinking marker pairs. A matched pair is
/// removed with the same byte offsets as the Go original.
///
/// One deliberate difference from Go: an **unterminated** open marker is
/// treated as "the scratchpad runs to the end" and dropped. Go stops scanning
/// there, which lets a model that opens a thinking block and never closes it
/// smuggle its private reasoning into the durable record - observed on a real
/// 211-record log whose stored summary began with a literal open marker. A
/// summary is written to the log and rendered to the user, so leaking half a
/// scratchpad is worse than losing a fragment of it.
pub fn strip_thinking(s: &str) -> String {
    let mut cur = s.to_string();
    // Inline marker pair: a space, then the word, then the same for the close.
    while let Some(start) = cur.find(" thinking") {
        let Some(rel_end) = cur[start..].find(" response") else {
            cur.truncate(start);
            break;
        };
        let end = start + rel_end;
        cur = format!("{}{}", &cur[..start], &cur[end..]);
    }
    // XML-style marker pairs. Both spellings are stripped: models emit
    // <thinking>...</thinking> and the shorter <think>...</think>, and the
    // shorter one was found leaking into a real stored summary.
    for (open, close) in [("<thinking>", "</thinking>"), ("<think>", "</think>")] {
        while let Some(start) = cur.find(open) {
            let body = start + open.len();
            let Some(rel_end) = cur[body..].find(close) else {
                cur.truncate(start);
                break;
            };
            let end = body + rel_end + close.len();
            cur = format!("{}{}", &cur[..start], &cur[end..]);
        }
    }
    cur.trim().to_string()
}
fn finish(text: &str, stop_reason: Option<&str>, budget: usize) -> Result<String> {
    let cleaned = strip_thinking(text);
    if matches!(stop_reason, Some("length") | Some("max_tokens")) {
        return Err(Error::new(ErrorKind::Refused, ERR_SUMMARY_TRUNCATED));
    }
    if cleaned.is_empty() {
        return Err(Error::new(ErrorKind::Invalid, ERR_NO_SUMMARY_TEXT));
    }
    Ok(clip_summary(&cleaned, budget))
}

/// Clip a summary to `budget` bytes on a rune boundary, appending "…" if it
/// fits. The result is always valid UTF-8.
pub fn clip_summary(s: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if s.len() <= budget {
        return s.to_string();
    }
    const ELLIPSIS: &str = "…"; // 3 bytes
    let limit = if budget >= ELLIPSIS.len() {
        budget - ELLIPSIS.len()
    } else {
        budget
    };
    let mut out = String::with_capacity(budget);
    for ch in s.chars() {
        let mut buf = [0u8; 4];
        let bytes = ch.encode_utf8(&mut buf).as_bytes();
        // Rust's &str is always valid UTF-8; a char never encodes to invalid
        // bytes, so the Go invalid-rune case cannot occur here.
        if out.len() + bytes.len() > limit {
            break;
        }
        out.push(ch);
    }
    if budget >= ELLIPSIS.len() {
        out.push('…');
    }
    out
}

// ---- result / close guards -------------------------------------------------

struct SummarizeOut {
    result: Result<String>,
    usage: Usage,
    usage_reported: bool,
}

struct SummarizerClose {
    stream: Box<dyn Stream>,
    closed: bool,
}

impl SummarizerClose {
    fn close_once(&mut self) {
        if !self.closed {
            self.stream.close();
            self.closed = true;
        }
    }
}

impl Drop for SummarizerClose {
    fn drop(&mut self) {
        self.close_once();
    }
}

/// `forge-session` owns the compaction writer and declares `Summarizer` in the
/// consumer, exactly as Go's `session.Summarizer` interface does. This impl is
/// the bridge: the provider-backed summary is one implementation of that trait,
/// not a dependency of the session crate (which must not know a model exists).
impl forge_session::Summarizer for ProviderSummarizer {
    fn summarize(&self, events: &[Event]) -> Result<String> {
        ProviderSummarizer::summarize(self, events)
    }
}
