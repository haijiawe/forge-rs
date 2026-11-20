// The protocol shapes the browser speaks, mirroring `crates/protocol/src/protocol.rs`.
// Protocol version: 1.
//
// This file used to be generated (`scripts/gen-protocol` over the Go oracle).
// That generator did not come across when the tree was rewritten in Rust, so the
// file is now maintained by hand: change it in the same commit as the Rust
// shapes, and let `docs/contracts/api.md` win where the two disagree.

/**
 * Version is the protocol version carried in X-Hearth-Protocol and in every
 * server-sent WebSocket frame. A client that does not know this number must
 * refuse the connection rather than guess.
 */
export const Version = 1 as const;

/**
 * HeaderVersion is the request/response header a client uses to state the
 * version it speaks. The server echoes it on every response.
 */
export const HeaderVersion = "X-Hearth-Protocol" as const;

/**
 * IdempotencyKeyHeader makes POST /messages retry-safe. A retried request with
 * the same key must not append a second user message.
 */
export const IdempotencyKeyHeader = "Idempotency-Key" as const;

/**
 * Error codes. These are stable identifiers: a client branches on the code, a
 * human reads the message. Adding a code is additive; renaming one is not.
 */
export const CodeBadRequest = "bad_request" as const;
export const CodeUnauthorized = "unauthorized" as const;
export const CodeForbidden = "forbidden" as const;
export const CodeNotFound = "not_found" as const;
export const CodeConflict = "conflict" as const;
export const CodeUnsupportedMedia = "unsupported_media_type" as const;
export const CodeUnprocessable = "unprocessable_entity" as const;
export const CodePreconditionFailed = "precondition_required" as const;
export const CodeMethodNotAllowed = "method_not_allowed" as const;
export const CodeInternal = "internal_error" as const;
export const CodeLogReadFailed = "log_read_failed" as const;

/** The closed set of ErrorCode values this protocol defines. */
export type ErrorCode =
  | typeof CodeBadRequest
  | typeof CodeUnauthorized
  | typeof CodeForbidden
  | typeof CodeNotFound
  | typeof CodeConflict
  | typeof CodeUnsupportedMedia
  | typeof CodeUnprocessable
  | typeof CodePreconditionFailed
  | typeof CodeMethodNotAllowed
  | typeof CodeInternal
  | typeof CodeLogReadFailed;

/**
 * Session status values. Derived from the log, never stored: a projection that
 * disagrees with the log loses.
 */
export const StatusIdle = "idle" as const;
export const StatusRunning = "running" as const;
export const StatusAwaitingApproval = "awaiting_approval" as const;
export const StatusEnded = "ended" as const;
export const StatusAborted = "aborted" as const;

/** The closed set of SessionStatus values this protocol defines. */
export type SessionStatus =
  | typeof StatusIdle
  | typeof StatusRunning
  | typeof StatusAwaitingApproval
  | typeof StatusEnded
  | typeof StatusAborted;

/**
 * Approval decisions. The same two words appear in the approval/decision event
 * payload, which is deliberate: one vocabulary for one concept.
 */
export const DecisionAllow = "allow" as const;
export const DecisionDeny = "deny" as const;

/** The closed set of ApprovalDecision values this protocol defines. */
export type ApprovalDecision =
  | typeof DecisionAllow
  | typeof DecisionDeny;

/**
 * WebSocket frame kinds. The discriminator is always "kind" and always first in
 * the documented shape, because a client dispatches on it before decoding.
 */
export const KindSnapshot = "snapshot" as const;
export const KindEvent = "event" as const;
export const KindDelta = "delta" as const;
export const KindError = "error" as const;
export const KindPing = "ping" as const;
export const KindPong = "pong" as const;

/** The closed set of FrameKind values this protocol defines. */
export type FrameKind =
  | typeof KindSnapshot
  | typeof KindEvent
  | typeof KindDelta
  | typeof KindError
  | typeof KindPing
  | typeof KindPong;

/**
 * Attachment kinds accepted by POST /messages.
 */
export const AttachmentFile = "file" as const;
export const AttachmentImage = "image" as const;
export const AttachmentDiff = "diff" as const;

/** The closed set of AttachmentKind values this protocol defines. */
export type AttachmentKind =
  | typeof AttachmentFile
  | typeof AttachmentImage
  | typeof AttachmentDiff;

/**
 * Health is GET /healthz. It is unauthenticated and must not touch the disk: a
 * process that can answer it is alive, which is all it claims. It says nothing
 * about model providers, so a healthy process with no network still reports ok.
 */
export interface Health {
  status: string;
  protocol: number;
  version: string;
  uptimeMs: number;
}

/**
 * SessionSummary is one row of GET /api/sessions and the body returned when a
 * session is created. It carries no transcript: a list view that needs every
 * event is a list view that will be slow.
 */
export interface SessionSummary {
  id: string;
  createdAt: number;
  updatedAt: number;
  status: SessionStatus;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  lastSeq: number;
  turns: number;
  workspace: string;
  model: string;
}

/**
 * SessionDetail is GET /api/sessions/{id}: a summary plus the derived detail a
 * client needs to render the session without replaying the whole log.
 */
export interface SessionDetail extends SessionSummary {
  pendingApproval?: PendingApproval;
}

/**
 * PendingApproval is an approval/request with no matching approval/decision. It
 * is omitted unless something is actually waiting — an empty object would make a
 * client think it has something to answer.
 */
export interface PendingApproval {
  approvalId: string;
  callId: string;
  tool: string;
  args?: unknown;
  reason?: string;
  requestedAt: number;
  timeoutMs?: number;
}

/**
 * SessionList is GET /api/sessions. NextCursor nil means this is the last page;
 * ordering is updatedAt descending with ties broken on id so paging is stable.
 */
export interface SessionList {
  sessions: SessionSummary[];
  nextCursor?: string;
}

/**
 * CreateSessionRequest is POST /api/sessions. Workspace is required and must be
 * an absolute path to an existing directory; model and profile fall back to
 * config when empty.
 */
export interface CreateSessionRequest {
  workspace: string;
  model?: string;
  profile?: string;
}

/**
 * CreateSessionResponse is 201 from POST /api/sessions. The new session already
 * has one event (session/start), so LastSeq is 1 and the summary says so.
 */
export interface CreateSessionResponse {
  session: SessionSummary;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  lastSeq: number;
}

/**
 * EventsPage is GET /api/sessions/{id}/events. Events are verbatim log objects
 * (json.RawMessage) so the server can never accidentally re-shape history: what
 * the log says is what the client sees.
 */
export interface EventsPage {
  sessionId: string;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  from: number;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  to: number;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  lastSeq: number;
  events: unknown[];
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  nextAfter: number;
}

/**
 * Attachment is one entry of PostMessageRequest.Attachments. Data is base64 for
 * image attachments and empty for file attachments, whose content the server
 * reads from Path under the session workspace.
 */
export interface Attachment {
  kind: AttachmentKind;
  path?: string;
  data?: string;
}

/**
 * PostMessageRequest is POST /api/sessions/{id}/messages.
 */
export interface PostMessageRequest {
  text: string;
  attachments?: Attachment[];
}

/**
 * PostMessageResponse is 202 (accepted) or 200 (a replay of the same
 * Idempotency-Key). Deduplicated is what tells the client which one it got.
 */
export interface PostMessageResponse {
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  seq: number;
  accepted: boolean;
  deduplicated: boolean;
}

/**
 * SteerRequest is POST /api/sessions/{id}/steer: text injected into the turn
 * that is already running, instead of opening the next one. Unlike a message it
 * has no attachments: steering a running turn with files is not a thing the
 * loop can honour mid-turn.
 */
export interface SteerRequest {
  text: string;
}

/**
 * SteerResponse is 202 (appended and queued for the running turn) or 200 (a
 * replay of the same Idempotency-Key, nothing appended). Seq is the seq of the
 * user/message event that carries the steered text.
 */
export interface SteerResponse {
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  seq: number;
  accepted: boolean;
  deduplicated: boolean;
}

/**
 * CancelResponse is 202 from POST /api/sessions/{id}/cancel. Seq is the last seq
 * of the log when the cancel was delivered; the turn's own turn/end record is
 * written afterwards by the turn loop, which owns that record.
 */
export interface CancelResponse {
  cancelled: boolean;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  seq: number;
}

/**
 * ApprovalDecisionRequest is POST /api/sessions/{id}/approvals/{approvalId}.
 */
export interface ApprovalDecisionRequest {
  decision: ApprovalDecision;
  note?: string;
}

/**
 * ApprovalDecisionResponse is 200 from the approvals endpoint. A second POST for
 * the same approvalId is 409, not a second decision: a double-click must not
 * append two events.
 */
export interface ApprovalDecisionResponse {
  approvalId: string;
  decision: ApprovalDecision;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  seq: number;
}

/**
 * APIError is the machine-readable part of a failure. Details is free-form and
 * may be empty; a client must not depend on its members.
 */
export interface APIError {
  code: ErrorCode;
  message: string;
  details?: Record<string, unknown>;
}

/**
 * ErrorResponse is the envelope of every non-2xx body that has one.
 */
export interface ErrorResponse {
  error: APIError;
}

/**
 * SnapshotFrame is always the first frame on a new connection. It carries the
 * derived session state so a client can render before the first event arrives.
 */
export interface SnapshotFrame {
  kind: "snapshot";
  protocol: number;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  lastSeq: number;
  session: SessionDetail;
}

/**
 * EventFrame is one appended event, verbatim from the log, in strictly
 * increasing seq order with no gaps. A gap is a server bug: the client should
 * reconnect with ?after= and re-read via GET /events.
 */
export interface EventFrame {
  kind: "event";
  protocol: number;
  event: unknown;
}

/**
 * DeltaFrame is streaming assistant text, batched per (turn, step) and flushed
 * at most every 50 ms. It is advisory: the authoritative text is the
 * assistant/message event, so a client that reconnects rebuilds from events
 * alone and never keeps a delta it cannot back with an event.
 */
export interface DeltaFrame {
  kind: "delta";
  protocol: number;
  turn: number;
  step: number;
  /** Values above 2^53 lose precision in JavaScript (Number.MAX_SAFE_INTEGER). */
  seq: number;
  text: string;
}

/**
 * ErrorFrame reports a stream-level problem. Fatal false means the connection
 * stays open and the client may continue.
 */
export interface ErrorFrame {
  kind: "error";
  protocol: number;
  fatal: boolean;
  error: APIError;
}

/**
 * PongFrame answers a client PingFrame. Server pings every 30 s are RFC 6455
 * control frames, not these JSON frames; these exist so a client can probe an
 * otherwise idle stream.
 */
export interface PongFrame {
  kind: "pong";
  protocol: number;
}

/**
 * PingFrame is the only client-to-server frame besides pong. Every mutation goes
 * through HTTP, which keeps the WebSocket a one-way pipe and makes retries a
 * problem the HTTP layer already solved.
 */
export interface PingFrame {
  kind: "ping";
}

/** Every frame the server can push over the event stream. Narrow it with `kind`. */
export type ServerFrame = SnapshotFrame | EventFrame | DeltaFrame | ErrorFrame | PongFrame;

/** Every frame a client can send. */
export type ClientFrame = PingFrame;

/**
 * decodeFrame parses one server frame and returns it narrowed by `kind`.
 *
 * It throws rather than returning null: an unknown kind means this client and
 * the server disagree about the protocol, and a frame dropped in silence would
 * surface later as a session that never finishes.
 */
export function decodeFrame(raw: string): ServerFrame {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error("frame is not valid JSON");
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("frame is not a JSON object");
  }
  const kind = (parsed as { kind?: unknown }).kind;
  if (typeof kind !== "string") {
    throw new Error("frame has no kind discriminator");
  }
  switch (kind) {
    case "snapshot":
      return parsed as SnapshotFrame;
    case "event":
      return parsed as EventFrame;
    case "delta":
      return parsed as DeltaFrame;
    case "error":
      return parsed as ErrorFrame;
    case "pong":
      return parsed as PongFrame;
    default:
      throw new Error(`unknown frame kind: ${kind}`);
  }
}
