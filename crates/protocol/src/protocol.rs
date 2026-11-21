//! The wire types shared by the server and the browser UI.
//!
//! Layer L2. This file is the frozen Rust half of `forge-go/internal/protocol/
//! protocol.go`; the Go tree is the oracle, and `forge-go/web/src/api/
//! protocol.ts` is generated from the same shapes. Nothing here touches the
//! filesystem, the log or the network — changing a field here is changing a
//! published contract and must be made in the Go oracle first.
//!
//! Two rules that are easy to break by accident:
//!   - An optional member is omitted, never sent as null. Every Option field
//!     therefore carries `skip_serializing_if = "Option::is_none"`.
//!   - Timestamps are integer Unix epoch milliseconds, never ISO strings and
//!     never floats (a float cannot represent every i64 millisecond).

#![forbid(unsafe_op_in_unsafe_fn)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version carried in `X-Hearth-Protocol` and in every server-sent
/// WebSocket frame. A client that does not know this number must refuse the
/// connection rather than guess.
pub const VERSION: u8 = 1;
/// Header used by a client to state the protocol it speaks; echoed on responses.
pub const HEADER_VERSION: &str = "X-Hearth-Protocol";
/// Makes `POST /messages` retry-safe: a replay must not append a second message.
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";

/// Stable error identifiers. A client branches on the code, a human reads the
/// message. Adding a code is additive; renaming one is not.
pub mod code {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const FORBIDDEN: &str = "forbidden";
    pub const NOT_FOUND: &str = "not_found";
    pub const CONFLICT: &str = "conflict";
    pub const UNSUPPORTED_MEDIA: &str = "unsupported_media_type";
    pub const UNPROCESSABLE: &str = "unprocessable_entity";
    pub const PRECONDITION_FAILED: &str = "precondition_required";
    pub const METHOD_NOT_ALLOWED: &str = "method_not_allowed";
    /// The one code whose response also carries a header: `Retry-After` is the
    /// only thing a client can act on, and deriving it from a message string
    /// would make the contract prose load-bearing.
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const INTERNAL: &str = "internal_error";
    pub const LOG_READ_FAILED: &str = "log_read_failed";
}

/// Session status values. Derived from the log, never stored.
pub mod status {
    pub const IDLE: &str = "idle";
    pub const RUNNING: &str = "running";
    pub const AWAITING_APPROVAL: &str = "awaiting_approval";
    pub const ENDED: &str = "ended";
    pub const ABORTED: &str = "aborted";
}

/// The same two words appear in the `approval/decision` event payload.
pub mod decision {
    pub const ALLOW: &str = "allow";
    pub const DENY: &str = "deny";
}

/// WebSocket frame kinds. The discriminator is always `kind`.
///
/// The event stream and the terminal share this vocabulary but never a socket:
/// `snapshot`/`event`/`delta` belong to `GET /api/sessions/{id}/stream`,
/// `run`/`cancel`/`start`/`output`/`exit`/`denied` to
/// `GET /api/sessions/{id}/terminal`. A client that meets an unknown kind on
/// either socket ignores that frame instead of guessing.
pub mod kind {
    pub const SNAPSHOT: &str = "snapshot";
    pub const EVENT: &str = "event";
    pub const DELTA: &str = "delta";
    pub const ERROR: &str = "error";
    pub const PING: &str = "ping";
    pub const PONG: &str = "pong";
    /// Client to server: run the command line in `cwd`.
    pub const RUN: &str = "run";
    /// Client to server: stop the command that is running, if any.
    pub const CANCEL: &str = "cancel";
    /// Server to client: the command was accepted and a process is starting.
    pub const START: &str = "start";
    /// Server to client: bytes the process wrote, already split on a character
    /// boundary so `text` is never half a UTF-8 sequence.
    pub const OUTPUT: &str = "output";
    /// Server to client: the process ended (or was cancelled); `exit` is the
    /// last frame a client needs before it may enable its input again.
    pub const EXIT: &str = "exit";
    /// Server to client: the request was refused and nothing ran.
    pub const DENIED: &str = "denied";
}

/// Attachment kinds accepted by `POST /messages`.
pub mod attachment_kind {
    pub const FILE: &str = "file";
    pub const IMAGE: &str = "image";
    pub const DIFF: &str = "diff";
}

/// `GET /healthz`. Unauthenticated, no disk access: it claims only that the
/// process is alive, not that a provider is reachable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub protocol: u8,
    pub version: String,
    #[serde(rename = "uptimeMs")]
    pub uptime_ms: i64,
}

/// One row of `GET /api/sessions`, and the body of `POST /api/sessions`.
/// It deliberately carries no transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    pub status: String,
    #[serde(rename = "lastSeq")]
    pub last_seq: u64,
    pub turns: u32,
    pub workspace: String,
    pub model: String,
}

/// `GET /api/sessions/{id}`. `pending_approval` is omitted unless something is
/// actually waiting — an empty object would make a client think it can answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub summary: SessionSummary,
    #[serde(rename = "pendingApproval", skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<PendingApproval>,
}

/// An `approval/request` with no matching `approval/decision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingApproval {
    #[serde(rename = "approvalId")]
    pub approval_id: String,
    #[serde(rename = "callId")]
    pub call_id: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "requestedAt")]
    pub requested_at: i64,
    #[serde(rename = "timeoutMs", skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

/// `GET /api/sessions`. `next_cursor` absent (not null) means the last page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `POST /api/sessions`. Workspace must be an absolute existing directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// 201 from `POST /api/sessions`: the new log already has `session/start` at 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    pub session: SessionSummary,
    #[serde(rename = "lastSeq")]
    pub last_seq: u64,
}

/// `GET /api/sessions/{id}/events`. Events are verbatim log values so the
/// server can never accidentally reshape history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsPage {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub from: u64,
    pub to: u64,
    #[serde(rename = "lastSeq")]
    pub last_seq: u64,
    pub events: Vec<Value>,
    #[serde(rename = "nextAfter")]
    pub next_after: u64,
}

/// One entry of `PostMessageRequest.attachments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// What the client calls the file. Optional: a client that only sends
    /// bytes is still understood.
    #[serde(rename = "mediaType", skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `POST /api/sessions/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostMessageRequest {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
}

/// `PUT /api/sessions/{id}/file`: save an edited buffer.
///
/// `expectedSha256` is the digest `GET /api/sessions/{id}/file` handed back with
/// the body. When it is present the file must still hash to it, so an editor that
/// opened a file ten minutes ago cannot silently undo an agent that rewrote it.
/// Absent means "this is a new file, and I am not claiming to have read it".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWriteRequest {
    pub path: String,
    pub text: String,
    #[serde(rename = "expectedSha256", skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
}

/// What `POST /api/sessions/{id}/revert` asks for: one path, no body bytes.
///
/// Reverting takes no text because the bytes come from git, not from the caller.
/// That is also why it takes no digest: a digest would be the caller claiming to
/// know what the committed version holds, and the point of asking git is that
/// nobody has to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRevertRequest {
    pub path: String,
}

/// What a `user/message` event records about an attachment.
///
/// The body is not in the event: it lives under a name in the data
/// directory's blob store, and `GET /api/sessions/{id}/attachments/{id}`
/// serves it. A log line that carried a base64 body would be paid for by
/// every reader of the log — projection, fork, the index, the browser — and
/// could never be deleted, because the log is append-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub kind: String,
    /// 32 hex characters, the name in the data directory's blob store. Absent
    /// when the attachment names a path instead of carrying a body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "mediaType", skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// What the file was called, for a human reading the transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub bytes: u64,
    /// The workspace-relative path a `kind: "file"` attachment referred to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// 202 (accepted) or 200 (replay of the same `Idempotency-Key`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostMessageResponse {
    pub seq: u64,
    pub accepted: bool,
    pub deduplicated: bool,
}

/// `POST /api/sessions/{id}/steer`: text for the turn already running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerRequest {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerResponse {
    pub seq: u64,
    pub accepted: bool,
    pub deduplicated: bool,
}

/// 202 from `POST /api/sessions/{id}/cancel`. The loop writes `turn/end` later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelResponse {
    pub cancelled: bool,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecisionRequest {
    pub decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecisionResponse {
    #[serde(rename = "approvalId")]
    pub approval_id: String,
    pub decision: String,
    pub seq: u64,
}

/// Machine-readable part of a failure. A client must not depend on `details`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct APIError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// Envelope of every non-2xx body that has one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: APIError,
}

/// One struct per frame kind: an illegal field combination cannot be expressed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotFrame {
    pub kind: String,
    pub protocol: u8,
    #[serde(rename = "lastSeq")]
    pub last_seq: u64,
    pub session: SessionDetail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventFrame {
    pub kind: String,
    pub protocol: u8,
    pub event: Value,
}

/// Advisory text: the authoritative text is the `assistant/message` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaFrame {
    pub kind: String,
    pub protocol: u8,
    pub turn: u32,
    pub step: u32,
    pub seq: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorFrame {
    pub kind: String,
    pub protocol: u8,
    pub fatal: bool,
    pub error: APIError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PongFrame {
    pub kind: String,
    pub protocol: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingFrame {
    pub kind: String,
}

/// `GET /api/sessions/{id}/terminal`, client to server: run one command line.
///
/// `command` is one line as a person typed it, handed to the platform shell by
/// the runner. `cwd` is relative to the session workspace and defaults to its
/// root; the server resolves it and refuses anything that escapes, so this field
/// can never name a directory the session does not own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRunFrame {
    pub kind: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `GET /api/sessions/{id}/terminal`, client to server: stop the live command.
///
/// Cancelling costs no event of its own: the `terminal/end` line records
/// `cancelled: true`, so the log still says what happened without a second
/// write racing the one that does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCancelFrame {
    pub kind: String,
}

/// `GET /api/sessions/{id}/terminal`, server to client: the command was accepted.
///
/// Sent before the process starts, so a client that never sees `start` knows
/// nothing ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalStartFrame {
    pub kind: String,
    pub protocol: u8,
    pub command: String,
    pub cwd: String,
}

/// `GET /api/sessions/{id}/terminal`, server to client: streamed output.
///
/// `stream` is `"stdout"` or `"stderr"`; `text` is always whole UTF-8
/// characters, because a reader that split a multi-byte character would show a
/// replacement glyph where the process wrote a letter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOutputFrame {
    pub kind: String,
    pub protocol: u8,
    pub stream: String,
    pub text: String,
}

/// `GET /api/sessions/{id}/terminal`, server to client: the command is over.
///
/// The tails are the same bytes that reached `terminal/end`, bounded and
/// possibly marked truncated. `exitCode` is `-1` when nothing ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalExitFrame {
    pub kind: String,
    pub protocol: u8,
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
    #[serde(rename = "durationMs")]
    pub duration_ms: i64,
    pub cancelled: bool,
    #[serde(rename = "stdoutTail")]
    pub stdout_tail: String,
    #[serde(rename = "stderrTail")]
    pub stderr_tail: String,
}

/// `GET /api/sessions/{id}/terminal`, server to client: the request was refused.
///
/// Nothing ran and no `terminal/run` was written; a refusal that left a log line
/// would make a denied click look like a command that ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalDeniedFrame {
    pub kind: String,
    pub protocol: u8,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_members_are_omitted_not_null() {
        let value = serde_json::to_value(SessionList {
            sessions: vec![],
            next_cursor: None,
        })
        .unwrap();
        assert_eq!(value, serde_json::json!({"sessions": []}));
    }

    #[test]
    fn a_session_detail_flattens_the_summary() {
        let detail = SessionDetail {
            summary: SessionSummary {
                id: "s_test".into(),
                created_at: 1,
                updated_at: 2,
                status: status::IDLE.into(),
                last_seq: 1,
                turns: 0,
                workspace: "/tmp".into(),
                model: "m".into(),
            },
            pending_approval: None,
        };
        let value = serde_json::to_value(&detail).unwrap();
        assert_eq!(value["id"], "s_test");
        assert_eq!(value["lastSeq"], 1);
        assert!(value.get("pendingApproval").is_none());
    }
}
