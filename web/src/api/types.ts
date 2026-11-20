// The one boundary to the protocol module.
//
// web/src/api/protocol.ts mirrors crates/protocol/src/protocol.rs. It is no
// longer generated — the Go generator behind it did not come across — so the two
// sides change together, in one commit. Nothing under web/src may restate a
// protocol shape: everything imports it through this file, and the imports are
// type-only so a build of the pure modules (virtualizer, stream reducer, cards)
// does not need the protocol file at runtime.
//
// If a name below does not match the shapes, change this file and the protocol
// file — never copy the shape somewhere else (that would create a second source
// of truth for a frozen wire contract).
export type {
  Health,
  SessionStatus,
  ApprovalDecision,
  ErrorCode,
  FrameKind,
  AttachmentKind,
  SessionSummary,
  SessionDetail,
  PendingApproval,
  SessionList,
  CreateSessionRequest,
  CreateSessionResponse,
  EventsPage,
  Attachment,
  PostMessageRequest,
  PostMessageResponse,
  SteerRequest,
  SteerResponse,
  CancelResponse,
  ApprovalDecisionRequest,
  ApprovalDecisionResponse,
  APIError,
  ErrorResponse,
  SnapshotFrame,
  EventFrame,
  DeltaFrame,
  ErrorFrame,
  PongFrame,
  PingFrame,
  ServerFrame,
  ClientFrame,
} from './protocol';