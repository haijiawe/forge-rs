// The one boundary to the generated protocol module.
//
// web/src/api/protocol.ts is GENERATED from internal/protocol/protocol.go by
// scripts/gen-protocol (another agent owns it). Nothing under web/src may
// restate a protocol shape: everything imports it through this file, and the
// imports are type-only so a build of the pure modules (virtualizer, stream
// reducer, cards) does not need the generated file at runtime.
//
// If a name below does not match what the generator emits, change this file
// only — never copy the shape somewhere else (that would create a second
// source of truth for a frozen wire contract).
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