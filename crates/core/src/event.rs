//! The event envelope — the only thing this project considers true.
//!
//! Every fact about a session is a line in that session's log, appended in
//! order, never rewritten. The envelope is the shape those lines share. Two
//! rules govern it and both are enforced by the tests in this file:
//!
//! 1. **Validation is a boundary act.** It checks the envelope, not the
//!    payload. A reader built today must be able to read a log written by a
//!    future build that added payload members, because the alternative is
//!    every upgrade invalidating every history.
//! 2. **An unknown type is data; an unknown version is an error.** Skipping a
//!    `future/thing` record loses nothing a reader understood in the first
//!    place. Guessing at `v: 2` would lose the meaning of every record at once.

use crate::error::{Error, ErrorKind, Result};
use crate::json::canonical;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The one format this build writes and the newest it reads.
pub const FORMAT_VERSION: u8 = 1;

/// An event type as a string constant.
///
/// A newtype rather than a bare `&str` so that the set of types a writer may
/// emit is one edit, and so a test can enumerate it. The values are the wire
/// format: renaming a constant must never rename a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventType(pub &'static str);

impl EventType {
    pub const SESSION_START: EventType = EventType("session/start");
    pub const SESSION_END: EventType = EventType("session/end");
    /// The session was archived: whoever owns it has decided no more turns should
    /// start in it, and no child of it should start either.
    ///
    /// One record, written at most once, carrying no payload beyond an optional
    /// `reason`. Archiving is a fact about a session, not a flag on a process: a
    /// later process reads the log and reaches the same conclusion.
    pub const SESSION_ARCHIVED: EventType = EventType("session/archived");
    pub const TURN_START: EventType = EventType("turn/start");
    pub const TURN_END: EventType = EventType("turn/end");
    pub const USER_MESSAGE: EventType = EventType("user/message");
    pub const ASSISTANT_MESSAGE: EventType = EventType("assistant/message");
    pub const TOOL_CALL: EventType = EventType("tool/call");
    pub const TOOL_RESULT: EventType = EventType("tool/result");
    pub const APPROVAL_REQUEST: EventType = EventType("approval/request");
    pub const APPROVAL_DECISION: EventType = EventType("approval/decision");
    pub const COMPACTION_SUMMARY: EventType = EventType("compaction/summary");
    pub const ERROR: EventType = EventType("error");
    pub const STEP_START: EventType = EventType("step/start");
    pub const STEP_END: EventType = EventType("step/end");
    pub const LLM_CALL: EventType = EventType("llm/call");
    pub const GOAL_CREATED: EventType = EventType("goal/created");
    pub const GOAL_CONTINUED: EventType = EventType("goal/continued");
    pub const GOAL_ENDED: EventType = EventType("goal/ended");
    pub const CONTEXT_COMPRESSED: EventType = EventType("context/compressed");
    /// One line per pre-step/request rewrite a hook actually applied. See
    /// docs/11-adr-agent-hooks.md: the loop may be extended, so the rewrite must
    /// be on the record — a log with no `hook/rewrite` proves none happened.
    pub const HOOK_REWRITE: EventType = EventType("hook/rewrite");
    /// One row per direct child a parent session created: the stable identity a
    /// name, a resumable child, and `hearth subagent list` all hang off. Written
    /// on the parent log, by the loop, from `Result.meta["subagentCatalog"]`.
    pub const SUBAGENT_CATALOG: EventType = EventType("subagent/catalog");
    /// The child's own record of what it was started as (provider/model/label and
    /// the composer's knobs). Written once on the child log, log-only, never in
    /// model history, and deliberately free of depth and output schema.
    pub const SUBAGENT_DESCRIPTOR: EventType = EventType("subagent/descriptor");
    /// Opens a compaction transaction, before the summarizer runs. Pairs with
    /// `compaction/end`: a `start` with no `end` is a compaction that died in
    /// flight, which a reader can report instead of trusting a half-written range.
    pub const COMPACTION_START: EventType = EventType("compaction/start");
    /// Closes a compaction transaction. `payload.ok` says whether a summary
    /// actually landed, so a failed round is on the record and not silent.
    pub const COMPACTION_END: EventType = EventType("compaction/end");
    /// A summary that could not be produced (provider error, or empty output).
    /// The history stays visible: a failure to summarize must never hide events.
    pub const COMPACTION_SUMMARY_ERROR: EventType = EventType("compaction/summary-error");
    /// One row per request the loop built, before it is sent: `reason` says why a
    /// fresh request was made (`initial`, `series`, `change`, `resume`) and the
    /// model it was built for. Log-only; it names what was asked for without
    /// copying the message bodies (those are already on earlier rows).
    pub const REQUEST_HEADER: EventType = EventType("request/header");
    /// One row per request the loop built, saying what it actually asked for:
    /// the endpoint that would answer, the model, the sampling parameters in
    /// force, how many messages and tools went out, and a fingerprint of the
    /// system prompt. Log-only and additive — it names the inputs rather than
    /// copying them, so a reader can tell two requests apart without paying for
    /// the prompt twice.
    pub const REQUEST_CONTEXT: EventType = EventType("request/context");
    /// One row per provider attempt that did **not** become the visible answer:
    /// a failed or cancelled attempt, with its reason and whether it was
    /// retryable. Never in model history; the successful attempt still lands as
    /// exactly one `assistant/message`. This is the attempt-level record whose
    /// absence made a retried turn look like a single call.
    pub const ASSISTANT_ATTEMPT: EventType = EventType("assistant/attempt");
    /// One row per change to the durable input queues (`next-turn`/`next-step`).
    /// The payload is a splice — `{target,start,removed,inserted}` — so
    /// appending, claiming the head and clearing all use one shape. The queue is
    /// a fold of these rows, which is what lets a second process see pending
    /// work and a crash lose nothing already accepted.
    pub const AGENT_INBOX: EventType = EventType("agent/inbox");
    /// One row per phase change of a resident loop (`idle`, `running`,
    /// `maintenance`). Log-only: it describes the agent's own liveness, not
    /// anything the model said, so it never enters model history.
    pub const AGENT_STATUS: EventType = EventType("agent/status");
    /// One row per file written through the editor, by the person, not the model.
    ///
    /// It is deliberately not a `tool/call`: the tool channel records what the
    /// loop asked a tool to do, and its `result` answers the model. A human
    /// saving a buffer has no model in the loop, and dressing it as a tool call
    /// would make the transcript claim the agent wrote a file it never touched.
    ///
    /// The row exists because of law 1: after a restart the file on disk is the
    /// only surviving evidence, and "the agent read `main.rs` and it said X, but
    /// the disk says Y" is a discrepancy a reader should be able to explain
    /// without guessing at which process wrote last. Payload:
    /// `{path, bytes, sha256}` — the digest is of the bytes written, so a later
    /// reader can tell whether the file still matches what was saved.
    ///
    /// The projection ignores it and model history never sees it; it is a fact
    /// about the workspace, not about the conversation.
    pub const FILE_WRITE: EventType = EventType("file/write");

    /// One row per file whose committed bytes a person put back through the
    /// editor, discarding the working-tree change.
    ///
    /// It is a separate type from `file/write` for the reason `file/write` is a
    /// separate type from `tool/call`: the two rows describe different actions
    /// even though both move bytes. `file/write` says a person wrote this buffer;
    /// `file/revert` says a person threw away an edit and asked for the version
    /// git has. A log that could not tell them apart would make "who changed
    /// this file last, and did they mean to" unanswerable — which is the whole
    /// reason either row exists.
    ///
    /// A revert only succeeds for a path that has a committed version; the route
    /// refuses untracked files and repositories that are not there, and a refused
    /// revert appends nothing. Payload: `{path, bytes, sha256}` of the bytes now on
    /// disk, so a reader can still match the file against the row after the fact.
    ///
    /// The projection ignores it and model history never sees it.
    pub const FILE_REVERT: EventType = EventType("file/revert");

    /// Every type this build knows. A test asserts it matches the contract
    /// document, and the reader uses it to decide what it can fold.
    pub const ALL: &'static [EventType] = &[
        EventType::SESSION_START,
        EventType::SESSION_END,
        EventType::SESSION_ARCHIVED,
        EventType::TURN_START,
        EventType::TURN_END,
        EventType::USER_MESSAGE,
        EventType::ASSISTANT_MESSAGE,
        EventType::TOOL_CALL,
        EventType::TOOL_RESULT,
        EventType::APPROVAL_REQUEST,
        EventType::APPROVAL_DECISION,
        EventType::COMPACTION_SUMMARY,
        EventType::ERROR,
        EventType::STEP_START,
        EventType::STEP_END,
        EventType::LLM_CALL,
        EventType::GOAL_CREATED,
        EventType::GOAL_CONTINUED,
        EventType::GOAL_ENDED,
        EventType::CONTEXT_COMPRESSED,
        EventType::HOOK_REWRITE,
        EventType::SUBAGENT_CATALOG,
        EventType::SUBAGENT_DESCRIPTOR,
        EventType::COMPACTION_START,
        EventType::COMPACTION_END,
        EventType::COMPACTION_SUMMARY_ERROR,
        EventType::REQUEST_HEADER,
        EventType::REQUEST_CONTEXT,
        EventType::ASSISTANT_ATTEMPT,
        EventType::AGENT_INBOX,
        EventType::AGENT_STATUS,
        EventType::FILE_WRITE,
        EventType::FILE_REVERT,
    ];

    pub fn as_str(self) -> &'static str {
        self.0
    }

    pub fn known(self) -> bool {
        EventType::ALL.iter().any(|t| t.0 == self.0)
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// One record.
///
/// Field order here is the order in the struct, not the order on disk: line
/// order comes from [`canonical`], which sorts keys. Do not add
/// `#[serde(rename_all)]` — the wire names are the Rust names, deliberately, so
/// a grep for a payload key finds both sides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Format version. Present in every record so a reader meeting a foreign
    /// log can refuse it before it interprets a single field.
    pub v: u8,
    /// Dense, starts at 1, one higher than the record before it.
    pub seq: u64,
    pub id: String,
    /// Milliseconds since the Unix epoch, UTC. A wall clock, so it orders
    /// nothing — `seq` does that.
    pub ts: i64,
    pub session: String,
    /// Omitted when zero: a record that exists before any turn opened is a
    /// `session/start`, and writing `"turn": 0` would invite a reader to treat
    /// zero as a turn number.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub turn: u32,
    /// One-based, as a human counts steps.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub step: u32,
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Event {
    /// Build a record. `seq`, `ts` and `id` are the store's to assign: a
    /// caller that invents them is a caller that can invent two of the same.
    pub fn new(session: impl Into<String>, typ: EventType, payload: Option<Value>) -> Event {
        Event {
            v: FORMAT_VERSION,
            seq: 0,
            id: String::new(),
            ts: 0,
            session: session.into(),
            turn: 0,
            step: 0,
            typ: typ.0.to_string(),
            payload,
        }
    }

    /// Is this a type this build understands?
    pub fn known(&self) -> bool {
        EventType::ALL.iter().any(|t| t.0 == self.typ)
    }

    /// Parse one line of a log.
    pub fn parse_line(line: &str) -> Result<Event> {
        let tree: Value = serde_json::from_str(line).map_err(|e| {
            Error::new(ErrorKind::Json, format!("invalid JSON: {e}")).with_source(e)
        })?;
        Event::from_value(tree)
    }

    /// Parse an already-decoded tree. Split out from [`Event::parse_line`]
    /// because the HTTP layer has JSON in hand and should not re-stringify it.
    pub fn from_value(tree: Value) -> Result<Event> {
        if !tree.is_object() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "does not match the v1 envelope: a record is a JSON object",
            ));
        }
        // Version first. A log from the future must be refused for its version,
        // not for whichever field the reader happens to notice first.
        match tree.get("v").and_then(Value::as_u64) {
            Some(v) if v == FORMAT_VERSION as u64 => {}
            Some(v) => {
                return Err(Error::new(
                    ErrorKind::UnsupportedVersion,
                    format!(
                        "unsupported event version: got v{v}, this build reads v{FORMAT_VERSION}"
                    ),
                ))
            }
            None => {
                return Err(Error::new(
                    ErrorKind::UnsupportedVersion,
                    "unsupported event version: the record has no \"v\" member",
                ))
            }
        }
        let event: Event = serde_json::from_value(tree).map_err(|e| {
            Error::new(
                ErrorKind::Invalid,
                format!("does not match the v1 envelope: {e}"),
            )
            .with_source(e)
        })?;
        event.validate()?;
        Ok(event)
    }

    /// The envelope checks. Payload members are described by the contract and
    /// deliberately not enforced here.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("empty id on seq {}", self.seq),
            ));
        }
        if self.session.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("empty session on seq {}", self.seq),
            ));
        }
        if self.typ.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("empty type on seq {}", self.seq),
            ));
        }
        if self.ts <= 0 {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("empty ts on seq {}: {}", self.seq, self.ts),
            ));
        }
        Ok(())
    }

    /// The bytes this record has on disk.
    pub fn canonical(&self) -> Result<String> {
        canonical(self)
    }
}

/// Parse a whole log, enforcing the two properties a single line cannot show:
/// sequence numbers are dense from 1, and ids are unique.
///
/// Blank lines are skipped rather than refused: a torn write during recovery
/// can leave one, and a reader that dies on whitespace is a reader that cannot
/// repair what it was written to repair.
pub fn parse_log(text: &str) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        let event = Event::parse_line(raw)
            .map_err(|e| Error::new(e.kind(), format!("line {}: {e}", index + 1)))?;
        let want = events.len() as u64 + 1;
        if event.seq != want {
            return Err(Error::new(
                ErrorKind::NotDense,
                format!(
                    "sequence numbers are not dense: expected {want}, found {} at line {}",
                    event.seq,
                    index + 1
                ),
            ));
        }
        if !seen.insert(event.id.clone()) {
            return Err(Error::new(
                ErrorKind::DuplicateId,
                format!("duplicate event id {} at line {}", event.id, index + 1),
            ));
        }
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Event {
        Event {
            v: 1,
            seq: 1,
            id: "a1f0c3d47b2e4f6198a5c7d3e1b0a9f2".into(),
            ts: 1794787200000,
            session: "s_01jya7c9q3r5t7v9w1x3y5z7".into(),
            turn: 0,
            step: 0,
            typ: "session/start".into(),
            payload: None,
        }
    }

    #[test]
    fn zero_turn_and_step_are_omitted_from_the_wire() {
        let line = sample().canonical().unwrap();
        assert!(!line.contains("\"turn\""), "{line}");
        assert!(!line.contains("\"step\""), "{line}");
        assert!(!line.contains("\"payload\""), "{line}");
        assert_eq!(
            line,
            r#"{"id":"a1f0c3d47b2e4f6198a5c7d3e1b0a9f2","seq":1,"session":"s_01jya7c9q3r5t7v9w1x3y5z7","ts":1794787200000,"type":"session/start","v":1}"#
        );
    }

    #[test]
    fn an_unknown_type_is_kept_not_refused() {
        let mut e = sample();
        e.typ = "future/thing".into();
        e.payload = Some(json!({"anything": [1, 2, 3]}));
        let round = Event::parse_line(&e.canonical().unwrap()).unwrap();
        assert_eq!(round, e);
        assert!(!round.known());
    }

    #[test]
    fn unknown_payload_members_survive_a_round_trip() {
        let mut e = sample();
        e.typ = "assistant/message".into();
        e.payload = Some(json!({"text": "hi", "fromTheFuture": {"x": true}}));
        let round = Event::parse_line(&e.canonical().unwrap()).unwrap();
        assert_eq!(round.payload, e.payload);
    }

    #[test]
    fn a_future_version_is_refused_before_its_fields_are_read() {
        let err = Event::parse_line(r#"{"v":2,"seq":1,"id":"a","ts":1,"session":"s","type":"x"}"#)
            .unwrap_err();
        assert!(
            err.to_string().contains("unsupported event version"),
            "{err}"
        );
        let err =
            Event::parse_line(r#"{"seq":1,"id":"a","ts":1,"session":"s","type":"x"}"#).unwrap_err();
        assert!(
            err.to_string().contains("unsupported event version"),
            "{err}"
        );
    }

    #[test]
    fn envelope_members_are_checked_one_by_one() {
        for (mut e, want) in [
            (sample(), "empty id"),
            (sample(), "empty session"),
            (sample(), "empty type"),
            (sample(), "empty ts"),
        ] {
            match want {
                "empty id" => e.id.clear(),
                "empty session" => e.session.clear(),
                "empty type" => e.typ.clear(),
                _ => e.ts = -1,
            }
            let err = e.validate().unwrap_err().to_string();
            assert!(err.contains(want), "expected {want}, got {err}");
        }
    }

    #[test]
    fn density_and_uniqueness_are_checked_across_lines() {
        let a = sample();
        let mut b = sample();
        b.seq = 3;
        b.id = "b".into();
        let text = format!("{}\n{}\n", a.canonical().unwrap(), b.canonical().unwrap());
        let err = parse_log(&text).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotDense, "{err}");
        assert!(err.to_string().contains("not dense"), "{err}");

        let mut dup = sample();
        dup.seq = 2;
        let text = format!(
            "{}\n{}\n",
            sample().canonical().unwrap(),
            dup.canonical().unwrap()
        );
        let err = parse_log(&text).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DuplicateId, "{err}");
    }

    #[test]
    fn blank_lines_are_skipped() {
        let text = format!("\n\n{}\n   \n", sample().canonical().unwrap());
        assert_eq!(parse_log(&text).unwrap().len(), 1);
    }
}
