//! Consumer-owned hooks the agent loop needs, kept here so `hearth-agent` never
//! depends on the tool crate (which lands in a later wave).
//!
//! The W3 tool crate will implement [`ToolRunner`]; until then a test fake or
//! an embedding application supplies it. [`EventLog`] is the narrow slice of a
//! session store the loop writes through — the log stays the single source of
//! truth (invariant I1) because history is always re-projected from it.

use hearth_core::Event;
use hearth_llm::{ContentPart, ToolCall, ToolSpec};

/// One tool invocation's outcome, in the loop's own vocabulary.
#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    /// Text returned to the model as the tool message.
    pub output: String,
    /// Whether the tool considers this a failure (drives `tool/result.ok`).
    pub is_error: bool,
    /// Stable machine code, e.g. `denied`, `timeout`, `cancelled`.
    pub code: String,
    /// Whether the tool already clipped its output to the cap.
    pub truncated: bool,
    /// Optional structured details (recorded on `tool/result.meta`).
    pub meta: Option<serde_json::Value>,
    /// Bytes that cannot travel in a tool message. The chat-completions
    /// protocol only accepts text there, so the loop forwards these on the
    /// next user turn; nothing here is ever written to the event log.
    pub parts: Vec<ContentPart>,
}

/// Runs the tools advertised by `specs`. Implemented later by the W3 tool
/// crate; owned here so the agent crate has no upward dependency.
pub trait ToolRunner: Send + Sync {
    fn specs(&self) -> Vec<ToolSpec>;
    fn run(&self, call: ToolCall) -> hearth_core::Result<ToolResult>;
}

/// The slice of a session store the agent appends events to and reads identity
/// from. The real `hearth_session::Store` satisfies it; tests use the same type
/// so the loop is exercised against a real append-only log.
pub trait EventLog: Send + Sync {
    fn append(&self, event: &Event) -> hearth_core::Result<u64>;
    fn last_seq(&self) -> u64;
    /// Read every event back, so history can be re-projected from the log
    /// every turn (invariant I1) without opening a second file handle.
    fn read_all(&self) -> hearth_core::Result<Vec<Event>>;
}

impl EventLog for hearth_session::Store {
    fn append(&self, event: &Event) -> hearth_core::Result<u64> {
        hearth_session::Store::append(self, event)
    }
    fn last_seq(&self) -> u64 {
        hearth_session::Store::last_seq(self)
    }
    fn read_all(&self) -> hearth_core::Result<Vec<Event>> {
        let mut events = Vec::new();
        self.read_from(0, |e| {
            events.push(e.clone());
            Ok(())
        })?;
        Ok(events)
    }
}
