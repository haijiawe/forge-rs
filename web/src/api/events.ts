// Event envelope and payload types.
//
// HAND-WRITTEN from docs/contracts/events-v1.md §2 (the envelope) and §4 (the
// payload table). NOT generated: `scripts/gen-protocol` renders
// internal/protocol (the HTTP/WS wire layer) and deliberately does not touch
// internal/session, which owns the on-disk envelope plus its I/O. If this file
// and the Go producer disagree, the Go code and tests/conformance are the
// truth (§11) — fix this file, not the log.
//
// Two rules from the contract that shape every type below:
//
//  * Payloads are DESCRIPTIVE, not enforced (events-v1.md §4, last paragraph):
//    a writer may add a member, a reader must not reject an event whose
//    payload is unexpected. Every payload member is therefore optional, and
//    the renderer must tolerate a missing `text`, a `tool/result` without a
//    `code`, and so on.
//  * Unknown `type` values MUST be accepted and rendered generically (§6), so
//    `Event` has an explicit `UnknownEvent` fallback. Nothing in the UI may
//    throw on, or silently drop, a type it has not heard of.
//
// `seq` is a uint64 on the wire. This client keeps it as a `number` and
// fail-closes above Number.MAX_SAFE_INTEGER (see decodeSeq): silently losing
// precision would corrupt ordering, and bigint plumbing would touch every
// comparison in the UI for a case a local session cannot reach. The check is
// the contract's "must handle it as BigInt above 2^53" honoured as a refusal
// rather than a wrong answer.

/** JSON value as it can appear in a payload. */
export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue };

export interface JsonObject {
  [key: string]: JsonValue;
}

/** Event envelope, events-v1.md §2. */
export interface EventEnvelope {
  /** Envelope version; always 1 for a log this client may read. */
  v: number;
  /** Dense, ascending, 1-based position in the log. */
  seq: number;
  /** Event identity, unique within the log. Opaque: never parsed. */
  id: string;
  /** Unix epoch milliseconds. */
  ts: number;
  /** Session id, identical for every event in one log. */
  session: string;
  /** Turn number; absent (not zero) when the event is not part of a turn. */
  turn?: number;
  /** Step number within the turn. */
  step?: number;
  /** Namespaced discriminant, e.g. "tool/call". */
  type: string;
  /** Type-specific body; shape is descriptive, never validated. */
  payload?: unknown;
}

// --- Payloads (events-v1.md §4) -------------------------------------------
// Every member is optional on purpose: a newer writer may add members, and the
// contract says a reader must render what it understands rather than fail.

export interface SessionStartPayload {
  workspace?: string;
  model?: string;
  profile?: string;
  startedBy?: string;
  /** Only present when a system prompt is configured. */
  system?: string;
  systemBytes?: number;
  systemTruncated?: boolean;
}

export interface SessionEndPayload {
  reason?: 'completed' | 'aborted' | string;
  turns?: number;
}

export interface TurnStartPayload {
  /** 1-based turn number; absent in older logs (fall back to arrival order). */
  number?: number;
}

export interface TurnEndPayload {
  reason?: 'completed' | 'cancelled' | 'error' | string;
  steps?: number;
  cancelledBy?: string;
}

export interface StepStartPayload {
  /** 0-based within the turn. */
  index?: number;
  model?: string;
}

export interface StepEndPayload {
  index?: number;
  reason?: 'end_turn' | 'tool_use' | 'cancelled' | 'max_tokens' | 'error' | string;
}

export interface LLMCallPayload {
  model?: string;
  /** Message count of the request, not the messages themselves. */
  messages?: number;
  tools?: string[];
  temperature?: number;
  maxTokens?: number;
}

/**
 * What a `user/message` event records about an attachment.
 *
 * The body is not in the event: `id` names a file in the data directory's blob
 * store, served by `GET /api/sessions/{id}/attachments/{id}`. A `kind: "file"`
 * attachment names a workspace path and carries no body at all.
 */
export interface AttachmentRef {
  kind?: 'file' | 'image' | 'diff' | string;
  id?: string;
  path?: string;
  mediaType?: string;
  name?: string;
  bytes?: number;
}

export interface UserMessagePayload {
  text?: string;
  attachments?: AttachmentRef[];
}

export interface Usage {
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
}

export interface AssistantMessagePayload {
  text?: string;
  stopReason?: 'end_turn' | 'tool_use' | 'cancelled' | 'max_tokens' | 'error' | string;
  partial?: boolean;
  usage?: Usage;
  durationMs?: number;
}

export interface ToolCallPayload {
  callId?: string;
  tool?: string;
  args?: JsonValue;
}

export interface ToolResultPayload {
  callId?: string;
  tool?: string;
  ok?: boolean;
  /** Capped at 64 KB by the writer; may be truncated (see `truncated`). */
  output?: string;
  /** Machine-readable failure code, when ok is false. */
  code?: string;
  truncated?: boolean;
  /** Free-form tool facts: exitCode, bytes, durationMs, ... */
  meta?: Record<string, JsonValue>;
}

export interface ApprovalRequestPayload {
  approvalId?: string;
  callId?: string;
  tool?: string;
  args?: JsonValue;
  reason?: string;
  timeoutMs?: number;
}

export interface ApprovalDecisionPayload {
  approvalId?: string;
  callId?: string;
  decision?: 'allow' | 'deny' | string;
  decidedBy?: string;
  note?: string;
}

export interface CompactionPayload {
  summary?: string;
  replaces?: { from?: number; to?: number };
}

/**
 * §4.0.3: the records in seq `from..to` inclusive were replaced, for this window
 * only, by `summary`. Every member stays optional here even though the writer
 * checks them: payload schemas are descriptive and not enforced (§4), so a
 * reader renders what it finds instead of rejecting the event.
 */
export interface ContextCompressedPayload {
  /** 32 lowercase hex; a key, never a path. */
  blockId?: string;
  /** First hidden seq, inclusive. */
  from?: number;
  /** Last hidden seq, inclusive. */
  to?: number;
  /** What the model reads instead of the range. */
  summary?: string;
  /** How many projected messages fell in the range: a diagnostic, not an address. */
  messages?: number;
}

export interface ErrorPayload {
  message?: string;
  code?: string;
  retryable?: boolean;
}

/** The closed set of event types this build renders specially. */
export interface KnownEventPayloads {
  'session/start': SessionStartPayload;
  'session/end': SessionEndPayload;
  'turn/start': TurnStartPayload;
  'turn/end': TurnEndPayload;
  'step/start': StepStartPayload;
  'step/end': StepEndPayload;
  'llm/call': LLMCallPayload;
  'user/message': UserMessagePayload;
  'assistant/message': AssistantMessagePayload;
  'tool/call': ToolCallPayload;
  'tool/result': ToolResultPayload;
  'approval/request': ApprovalRequestPayload;
  'approval/decision': ApprovalDecisionPayload;
  'compaction/summary': CompactionPayload;
  'context/compressed': ContextCompressedPayload;
  error: ErrorPayload;
}

export type KnownEventType = keyof KnownEventPayloads;

export type EventOf<T extends KnownEventType> = EventEnvelope & {
  type: T;
  payload?: KnownEventPayloads[T];
};

/** One event of a type this build knows how to render. */
export type KnownEvent = {
  [K in KnownEventType]: EventOf<K>;
}[KnownEventType];

/**
 * Fallback member: a type this build has never heard of (events-v1.md §6).
 * It is rendered as a generic row showing `type` and `payload`.
 */
export interface UnknownEvent extends EventEnvelope {
  type: string;
  payload?: unknown;
}

/** Any event the transcript may have to render. */
export type Event = KnownEvent | UnknownEvent;

/** The known types as a runtime list, for type-guard membership tests. */
export const KNOWN_EVENT_TYPES: readonly KnownEventType[] = [
  'session/start',
  'session/end',
  'turn/start',
  'turn/end',
  'step/start',
  'step/end',
  'llm/call',
  'user/message',
  'assistant/message',
  'tool/call',
  'tool/result',
  'approval/request',
  'approval/decision',
  'compaction/summary',
  'context/compressed',
  'error',
] as const;

export function isKnownEventType(type: string): type is KnownEventType {
  return (KNOWN_EVENT_TYPES as readonly string[]).includes(type);
}

export function isKnownEvent(event: EventEnvelope): event is KnownEvent {
  return isKnownEventType(event.type);
}

/**
 * Narrow an envelope to one concrete known type without a cast in the caller.
 * Returns null when the event is a different (or unknown) type.
 */
export function eventAs<T extends KnownEventType>(
  event: EventEnvelope,
  type: T,
): EventOf<T> | null {
  if (event.type !== type) return null;
  return event as EventOf<T>;
}

/**
 * Read a payload as a partial struct. A non-object payload yields `{}`: the
 * contract does not guarantee the payload is an object, and a renderer that
 * crashes on that is the bug (§4).
 */
export function payloadOf<P extends object>(event: EventEnvelope): Partial<P> {
  const raw = event.payload;
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return {};
  return raw as Partial<P>;
}

/** Error thrown when a frame or event does not satisfy the envelope contract. */
export class ProtocolError extends Error {
  override readonly name = 'ProtocolError';
  constructor(message: string) {
    super(message);
  }
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

/**
 * `seq` is a uint64 on the wire; refuse values a JS number cannot hold exactly
 * rather than compare two different seqs as equal (api.md §1).
 */
export function decodeSeq(value: unknown): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) {
    throw new ProtocolError(`seq is not a non-negative integer: ${String(value)}`);
  }
  if (!Number.isSafeInteger(value)) {
    throw new ProtocolError(
      `seq ${value} exceeds Number.MAX_SAFE_INTEGER; this client cannot order it`,
    );
  }
  return value;
}

/**
 * Decode one event from the wire (an `EventsPage.events` element or an
 * `event` frame). Unknown envelope members and unknown payload shapes are
 * preserved; only the invariants a reader must check fail.
 */
export function decodeEvent(raw: unknown): Event {
  const obj = asRecord(raw);
  if (obj === null) throw new ProtocolError('event is not a JSON object');
  const version = obj['v'];
  if (version !== 1) {
    throw new ProtocolError(`unsupported event version ${String(version)} (want 1)`);
  }
  const id = obj['id'];
  const session = obj['session'];
  const type = obj['type'];
  const ts = obj['ts'];
  if (typeof id !== 'string' || id === '') throw new ProtocolError('event has empty id');
  if (typeof session !== 'string' || session === '') {
    throw new ProtocolError('event has empty session');
  }
  if (typeof type !== 'string' || type === '') throw new ProtocolError('event has empty type');
  if (typeof ts !== 'number' || !Number.isFinite(ts) || ts <= 0) {
    throw new ProtocolError('event has empty ts');
  }
  const seq = decodeSeq(obj['seq']);
  const event: EventEnvelope = {
    v: 1,
    seq,
    id,
    ts,
    session,
    type,
    ...(typeof obj['turn'] === 'number' ? { turn: obj['turn'] } : {}),
    ...(typeof obj['step'] === 'number' ? { step: obj['step'] } : {}),
    ...('payload' in obj ? { payload: obj['payload'] } : {}),
  };
  return event;
}