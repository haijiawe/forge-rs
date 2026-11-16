// WebSocket stream state machine, docs/contracts/api.md §4.8.
//
// It is a class, not an Immer-style reducer, on purpose: a session can hold
// 10k+ events and React re-renders off a version counter, so appending to the
// array in place avoids an O(n²) copy per event during a long stream. The
// machine is the only writer of its state; components read it through
// useSyncExternalStore.
//
// The rules, in the order the contract states them:
//
//  1. Ordering: `snapshot`, then `event`s, then `delta`s. A delta is advisory
//     and always belongs to an `assistant/message` event with the same `seq`.
//  2. Dedup by seq: an event at or below `lastSeq` is never applied twice
//     (this is what makes reconnect-with-`after=lastSeq` safe).
//  3. A gap (seq > lastSeq + 1) is a server bug: the machine stops applying,
//     flags `needsResync` and asks the caller to refetch GET /events.
//  4. A delta is dropped when the event that would back it cannot be an
//     `assistant/message` at that seq, or when one arrives — the event's text
//     always wins over streamed text.
//  5. Unknown event types are kept and rendered generically, never dropped.

import { decodeEvent, type Event, type EventEnvelope, type ApprovalRequestPayload, type ApprovalDecisionPayload } from './events';
import type {
  DeltaFrame,
  ErrorFrame,
  EventsPage,
  EventFrame,
  PendingApproval,
  SessionDetail,
  SnapshotFrame,
} from './types';

/** One streaming step's buffered text, awaiting the event that backs it. */
export interface PendingDelta {
  /** `${turn}:${step}` — one buffer per step, as the server batches. */
  key: string;
  turn: number;
  step: number;
  /** Seq of the assistant/message event this text belongs to. */
  seq: number;
  text: string;
}

export type StreamStatus =
  | 'idle'
  | 'running'
  | 'awaiting_approval'
  | 'ended'
  | 'aborted'
  | string;

export interface StreamState {
  readonly sessionId: string;
  /** The protocol version this client speaks; frames must carry it. */
  readonly protocolVersion: number;
  session: SessionDetail | null;
  status: StreamStatus;
  /** Last event seq applied. The reconnect cursor. */
  lastSeq: number;
  /** Last seq the server claimed exists (snapshot.lastSeq / page.lastSeq). */
  headSeq: number;
  events: Event[];
  /** Insertion-ordered pending deltas (one per turn/step). */
  deltas: PendingDelta[];
  /** Set when a gap or a protocol mismatch makes the stream untrustworthy. */
  needsResync: boolean;
  /** Non-fatal stream errors worth showing in the status bar. */
  streamError: string | null;
}

export type StreamInput =
  | { kind: 'snapshot'; frame: SnapshotFrame }
  | { kind: 'event'; frame: EventFrame }
  | { kind: 'delta'; frame: DeltaFrame }
  | { kind: 'page'; page: EventsPage }
  | { kind: 'stream-error'; frame: ErrorFrame }
  | { kind: 'reset' };

export type StreamEffect =
  | { kind: 'resync'; after: number; reason: 'gap' | 'protocol' }
  | { kind: 'fatal'; message: string };

export function createStreamState(sessionId: string, protocolVersion: number): StreamState {
  return {
    sessionId,
    protocolVersion,
    session: null,
    status: 'idle',
    lastSeq: 0,
    headSeq: 0,
    events: [],
    deltas: [],
    needsResync: false,
    streamError: null,
  };
}

function deltaKey(turn: number, step: number): string {
  return `${turn}:${step}`;
}

/** Status after one event, per api.md §4.2's derivation list. */
function deriveStatus(event: EventEnvelope, current: StreamStatus): StreamStatus {
  switch (event.type) {
    case 'session/start':
      return 'idle';
    case 'session/end':
      return 'ended';
    case 'turn/start':
      return 'running';
    case 'turn/end':
      return 'idle';
    case 'approval/request':
      return 'awaiting_approval';
    case 'approval/decision':
      return 'running';
    default:
      return current;
  }
}

function pendingApprovalFromEvent(event: EventEnvelope): PendingApproval | null {
  if (event.type !== 'approval/request') return null;
  const payload = (event.payload ?? {}) as ApprovalRequestPayload;
  const approvalId = payload.approvalId ?? '';
  if (approvalId === '') return null;
  return {
    approvalId,
    callId: payload.callId ?? '',
    tool: payload.tool ?? '',
    ...(payload.args === undefined ? {} : { args: payload.args }),
    ...(payload.reason === undefined ? {} : { reason: payload.reason }),
    requestedAt: event.ts,
    ...(payload.timeoutMs === undefined ? {} : { timeoutMs: payload.timeoutMs }),
  };
}

function decidedApprovalId(event: EventEnvelope): string | null {
  if (event.type !== 'approval/decision') return null;
  const payload = (event.payload ?? {}) as ApprovalDecisionPayload;
  return payload.approvalId ?? null;
}

export class StreamMachine {
  state: StreamState;

  constructor(sessionId: string, protocolVersion: number) {
    this.state = createStreamState(sessionId, protocolVersion);
  }

  /** Apply one input; returns the side effects the transport must perform. */
  apply(input: StreamInput): StreamEffect[] {
    switch (input.kind) {
      case 'snapshot':
        return this.applySnapshot(input.frame);
      case 'event':
        return this.applyEventFrame(input.frame);
      case 'delta':
        return this.applyDelta(input.frame);
      case 'page':
        return this.applyPage(input.page);
      case 'stream-error':
        return this.applyStreamError(input.frame);
      case 'reset': {
        const { sessionId, protocolVersion } = this.state;
        const status = this.state.status;
        this.state = createStreamState(sessionId, protocolVersion);
        this.state.status = status;
        return [];
      }
    }
  }

  private protocolMismatch(what: string, got: number): StreamEffect[] {
    const message = `${what} frame carries protocol ${String(got)}, this client speaks ${String(
      this.state.protocolVersion,
    )}`;
    this.state.needsResync = true;
    this.state.streamError = message;
    return [{ kind: 'resync', after: this.state.lastSeq, reason: 'protocol' }];
  }

  private applySnapshot(frame: SnapshotFrame): StreamEffect[] {
    if (frame.protocol !== this.state.protocolVersion) {
      return this.protocolMismatch('snapshot', frame.protocol);
    }
    const lastSeq = typeof frame.lastSeq === 'number' ? frame.lastSeq : this.state.headSeq;
    this.state.headSeq = Math.max(this.state.headSeq, lastSeq);
    if (frame.session) {
      this.state.session = frame.session;
      this.state.status = frame.session.status;
    }
    return [];
  }

  private applyEventFrame(frame: EventFrame): StreamEffect[] {
    if (frame.protocol !== this.state.protocolVersion) {
      return this.protocolMismatch('event', frame.protocol);
    }
    let event: Event;
    try {
      event = decodeEvent(frame.event);
    } catch (error) {
      this.state.needsResync = true;
      this.state.streamError = error instanceof Error ? error.message : String(error);
      return [{ kind: 'resync', after: this.state.lastSeq, reason: 'protocol' }];
    }
    return this.appendEvent(event);
  }

  /**
   * Apply one event with dedup and gap detection. A gap stops the stream: the
   * caller refetches GET /events from lastSeq.
   */
  private appendEvent(event: Event): StreamEffect[] {
    const { lastSeq } = this.state;
    if (event.seq <= lastSeq) return []; // duplicate: reconnect replay
    if (event.seq !== lastSeq + 1) {
      this.state.needsResync = true;
      return [{ kind: 'resync', after: lastSeq, reason: 'gap' }];
    }
    this.applyAcceptedEvent(event);
    return [];
  }

  private applyAcceptedEvent(event: Event): void {
    const state = this.state;
    // An event at seq N means no delta can still claim seq <= N: its seq is
    // taken, and a delta is only ever backed by an assistant/message event.
    state.deltas = state.deltas.filter((delta) => delta.seq > event.seq);
    state.events.push(event);
    state.lastSeq = event.seq;
    state.headSeq = Math.max(state.headSeq, event.seq);
    state.status = deriveStatus(event, state.status);

    const pending = pendingApprovalFromEvent(event);
    if (pending !== null && state.session !== null) {
      state.session = { ...state.session, pendingApproval: pending };
      state.status = 'awaiting_approval';
    }
    const decided = decidedApprovalId(event);
    if (decided !== null && state.session?.pendingApproval?.approvalId === decided) {
      state.session = { ...state.session, pendingApproval: undefined };
      state.status = 'running';
    }
    if (event.type === 'turn/end') {
      const turn = event.turn;
      if (turn !== undefined) state.deltas = state.deltas.filter((delta) => delta.turn !== turn);
    }
  }

  private applyDelta(frame: DeltaFrame): StreamEffect[] {
    if (frame.protocol !== this.state.protocolVersion) {
      return this.protocolMismatch('delta', frame.protocol);
    }
    const state = this.state;
    if (state.needsResync) return []; // untrusted until the refetch lands
    const seq = typeof frame.seq === 'number' ? frame.seq : 0;
    const text = typeof frame.text === 'string' ? frame.text : '';
    if (text === '') return [];
    // The authoritative event already landed: drop advisory text for it.
    if (seq <= state.lastSeq) return [];
    const key = deltaKey(frame.turn, frame.step);
    const existing = state.deltas.find((delta) => delta.key === key);
    if (existing === undefined) {
      state.deltas.push({ key, turn: frame.turn, step: frame.step, seq, text });
      return [];
    }
    if (seq < existing.seq) return []; // stale frame, out of order
    if (seq === existing.seq) {
      existing.text += text;
    } else {
      // A new step reusing the key: the old text has no event to back it.
      existing.seq = seq;
      existing.text = text;
    }
    return [];
  }

  /**
   * Apply a GET /events page (the resync path). Events below lastSeq are
   * duplicates; the page must still be in ascending seq order. If the page
   * does not reach headSeq the caller keeps resyncing.
   */
  private applyPage(page: EventsPage): StreamEffect[] {
    const rawEvents: readonly unknown[] = page.events ?? [];
    let firstProblem: string | null = null;
    for (const raw of rawEvents) {
      let event: Event;
      try {
        event = decodeEvent(raw);
      } catch (error) {
        firstProblem = error instanceof Error ? error.message : String(error);
        break;
      }
      if (event.seq <= this.state.lastSeq) continue;
      if (event.seq !== this.state.lastSeq + 1) {
        firstProblem = `event page jumps from seq ${this.state.lastSeq} to ${event.seq}`;
        break;
      }
      this.applyAcceptedEvent(event);
    }
    const lastSeq = typeof page.lastSeq === 'number' ? page.lastSeq : this.state.lastSeq;
    this.state.headSeq = Math.max(this.state.headSeq, lastSeq);
    this.state.needsResync = firstProblem !== null || this.state.lastSeq < this.state.headSeq;
    this.state.streamError = firstProblem;
    // Deltas surviving a resync cannot be trusted: the transcript was rebuilt
    // from events and any in-flight step will resend its text.
    this.state.deltas = [];
    if (this.state.needsResync && this.state.lastSeq < this.state.headSeq) {
      return [{ kind: 'resync', after: this.state.lastSeq, reason: 'gap' }];
    }
    return [];
  }

  private applyStreamError(frame: ErrorFrame): StreamEffect[] {
    const message = frame.error?.message ?? 'stream error';
    this.state.streamError = message;
    if (frame.fatal === true) {
      this.state.needsResync = true;
      return [{ kind: 'fatal', message }];
    }
    return [];
  }
}