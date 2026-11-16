// Projection: Event[] (+ pending deltas) → transcript rows.
//
// Rows exist for rendering, not for truth: everything here is derived from the
// log and can be rebuilt at any time (AGENTS.md law 1). The virtualizer needs a
// stable key per row, and a streamed turn must not change the key of rows above
// it, so a tool call and its result share one `tool:<callId>` row, and an
// approval request and decision share one `approval:<approvalId>` row.
//
// The default branch is generic on purpose: an unknown event type becomes an
// `event` row that prints its type, seq and payload (events-v1.md §6 — an old
// reader must survive a new log). A known type that the transcript does not
// need to draw at this size (step/start, llm/call, …) folds into a compact meta
// row unless verbose mode is on.

import type { ApprovalDecision } from '../api/types';
import type { Event, EventEnvelope } from '../api/events';
import type { PendingDelta } from '../api/stream';

export interface RowBase {
  /** Stable identity across appends and prepends: the virtualizer keys on it. */
  key: string;
  seq: number;
  ts: number;
}

export interface UserRow extends RowBase {
  kind: 'user';
  text: string;
  attachments: number;
}

export interface AssistantRow extends RowBase {
  kind: 'assistant';
  text: string;
  /** True while text is still a delta and not yet backed by an event. */
  streaming: boolean;
  stopReason?: string;
  partial?: boolean;
}

export interface ToolRow extends RowBase {
  kind: 'tool';
  callId: string;
  tool: string;
  args: unknown;
  /** Undefined until the result event arrives. */
  output?: string;
  ok?: boolean;
  truncated: boolean;
  /** Machine error code from the result, when the tool failed. */
  errorCode?: string;
  meta?: unknown;
  done: boolean;
}

export interface ApprovalRow extends RowBase {
  kind: 'approval';
  approvalId: string;
  callId: string;
  tool: string;
  args: unknown;
  reason?: string;
  decision?: ApprovalDecision;
  note?: string;
}

export interface SummaryRow extends RowBase {
  kind: 'summary';
  text: string;
  from?: number;
  to?: number;
}

export interface ErrorRow extends RowBase {
  kind: 'error';
  message: string;
  code?: string;
  retryable?: boolean;
}

export interface TurnRow extends RowBase {
  kind: 'turn';
  turn: number;
  phase: 'start' | 'end';
  reason?: string;
}

/** The generic fallback: any type the transcript has no renderer for. */
export interface EventRow extends RowBase {
  kind: 'event';
  type: string;
  payload: unknown;
  /** Known types folded out of the main transcript in compact mode. */
  folded: boolean;
}

export type Row =
  | UserRow
  | AssistantRow
  | ToolRow
  | ApprovalRow
  | SummaryRow
  | ErrorRow
  | TurnRow
  | EventRow;

export interface ProjectionOptions {
  /** Show step/start, step/end, llm/call and session/* rows. Default false. */
  verbose?: boolean;
}

function payloadOf(event: EventEnvelope): Record<string, unknown> {
  const payload: unknown = event.payload;
  return typeof payload === 'object' && payload !== null && !Array.isArray(payload)
    ? (payload as Record<string, unknown>)
    : {};
}

function str(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function num(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function seqOf(event: EventEnvelope): number {
  return typeof event.seq === 'number' ? event.seq : 0;
}

function tsOf(event: EventEnvelope): number {
  return typeof event.ts === 'number' ? event.ts : 0;
}

function genericRow(event: EventEnvelope, folded: boolean): EventRow {
  return {
    kind: 'event',
    key: `evt:${String(seqOf(event))}`,
    seq: seqOf(event),
    ts: tsOf(event),
    type: event.type,
    payload: event.payload,
    folded,
  };
}

/** One event → zero or one primary row (mergeable rows are handled by caller). */
export function rowFromEvent(event: EventEnvelope, options: ProjectionOptions = {}): Row | null {
  const payload = payloadOf(event);
  const base = {
    key: `evt:${String(seqOf(event))}`,
    seq: seqOf(event),
    ts: tsOf(event),
  };
  const verbose = options.verbose === true;

  switch (event.type) {
    case 'user/message':
      return {
        ...base,
        kind: 'user',
        text: str(payload['text']) ?? '',
        attachments: Array.isArray(payload['attachments']) ? payload['attachments'].length : 0,
      };
    case 'assistant/message':
      return {
        ...base,
        kind: 'assistant',
        text: str(payload['text']) ?? '',
        streaming: false,
        ...(str(payload['stopReason']) === undefined ? {} : { stopReason: str(payload['stopReason']) }),
        ...(typeof payload['partial'] === 'boolean' ? { partial: payload['partial'] } : {}),
      };
    case 'tool/call':
      return {
        ...base,
        kind: 'tool',
        key: `tool:${str(payload['callId']) ?? base.key}`,
        callId: str(payload['callId']) ?? '',
        tool: str(payload['tool']) ?? 'tool',
        args: payload['args'],
        truncated: false,
        done: false,
      };
    case 'tool/result':
      return {
        ...base,
        kind: 'tool',
        key: `tool:${str(payload['callId']) ?? base.key}`,
        callId: str(payload['callId']) ?? '',
        tool: str(payload['tool']) ?? 'tool',
        args: undefined,
        output: str(payload['output']) ?? '',
        ok: payload['ok'] === true,
        truncated: payload['truncated'] === true,
        ...(str(payload['code']) === undefined ? {} : { errorCode: str(payload['code']) }),
        ...(payload['meta'] === undefined ? {} : { meta: payload['meta'] }),
        done: true,
      };
    case 'approval/request':
      return {
        ...base,
        kind: 'approval',
        key: `approval:${str(payload['approvalId']) ?? base.key}`,
        approvalId: str(payload['approvalId']) ?? '',
        callId: str(payload['callId']) ?? '',
        tool: str(payload['tool']) ?? 'tool',
        args: payload['args'],
        ...(str(payload['reason']) === undefined ? {} : { reason: str(payload['reason']) }),
      };
    case 'approval/decision':
      return {
        ...base,
        kind: 'approval',
        key: `approval:${str(payload['approvalId']) ?? base.key}`,
        approvalId: str(payload['approvalId']) ?? '',
        callId: str(payload['callId']) ?? '',
        tool: str(payload['tool']) ?? 'tool',
        args: undefined,
        ...(payload['decision'] === 'allow' || payload['decision'] === 'deny'
          ? { decision: payload['decision'] }
          : {}),
        ...(str(payload['note']) === undefined ? {} : { note: str(payload['note']) }),
      };
    case 'compaction/summary': {
      const replaces = payload['replaces'];
      const range =
        typeof replaces === 'object' && replaces !== null
          ? (replaces as { from?: unknown; to?: unknown })
          : {};
      const from = num(range.from);
      const to = num(range.to);
      return {
        ...base,
        kind: 'summary',
        text: str(payload['summary']) ?? '',
        ...(from === undefined ? {} : { from }),
        ...(to === undefined ? {} : { to }),
      };
    }
    case 'error':
      return {
        ...base,
        kind: 'error',
        message: str(payload['message']) ?? 'error',
        ...(str(payload['code']) === undefined ? {} : { code: str(payload['code']) }),
        ...(typeof payload['retryable'] === 'boolean' ? { retryable: payload['retryable'] } : {}),
      };
    case 'turn/start':
      return {
        ...base,
        kind: 'turn',
        turn: num(payload['number']) ?? (event.turn ?? 0),
        phase: 'start',
      };
    case 'turn/end':
      return {
        ...base,
        kind: 'turn',
        turn: event.turn ?? 0,
        phase: 'end',
        ...(str(payload['reason']) === undefined ? {} : { reason: str(payload['reason']) }),
      };
    default:
      // step/start, step/end, llm/call, session/start, session/end and every
      // type this build has never heard of land here, and are folded unless
      // verbose mode asks for them.
      return genericRow(event, !verbose);
  }
}

/** True when the two rows describe the same identity (callId / approvalId). */
function mergeKey(row: Row): string | null {
  if (row.kind === 'tool') return `tool:${row.callId}`;
  if (row.kind === 'approval') return `approval:${row.approvalId}`;
  return null;
}

function mergeRows(existing: Row, incoming: Row): Row {
  if (existing.kind === 'tool' && incoming.kind === 'tool') {
    const merged: ToolRow = {
      ...existing,
      // A result row carries the authoritative output; a late call row must
      // not erase it, so only fill the fields the result brought.
      args: existing.args === undefined ? incoming.args : existing.args,
      ...(incoming.output === undefined ? {} : { output: incoming.output }),
      ...(incoming.ok === undefined ? {} : { ok: incoming.ok }),
      truncated: existing.truncated || incoming.truncated,
      ...(incoming.errorCode === undefined ? {} : { errorCode: incoming.errorCode }),
      ...(incoming.meta === undefined ? {} : { meta: incoming.meta }),
      done: existing.done || incoming.done,
    };
    return merged;
  }
  if (existing.kind === 'approval' && incoming.kind === 'approval') {
    const merged: ApprovalRow = {
      ...existing,
      args: existing.args === undefined ? incoming.args : existing.args,
      ...(incoming.decision === undefined ? {} : { decision: incoming.decision }),
      ...(incoming.note === undefined ? {} : { note: incoming.note }),
    };
    return merged;
  }
  return incoming;
}

/**
 * Project a full transcript. Used on first paint, on resync and by the tests;
 * the streaming path appends through RowAccumulator instead.
 */
export function projectRows(
  events: readonly Event[],
  deltas: readonly PendingDelta[] = [],
  options: ProjectionOptions = {},
): Row[] {
  const accumulator = new RowAccumulator(options);
  accumulator.rebuild(events);
  accumulator.setDeltas(deltas);
  return accumulator.rows;
}

/**
 * Incremental projection for the streaming path: one event appended per frame
 * costs O(1) amortized, not a re-projection of the whole transcript.
 *
 * Event rows and delta rows are kept apart so a delta can never end up above an
 * event: a delta describes a seq the server has not committed yet, so it always
 * trails the transcript, and the next event simply appends to the event list.
 */
export class RowAccumulator {
  private eventRows: Row[] = [];
  private deltaRows: AssistantRow[] = [];
  private composed: Row[] | null = null;
  private composedKeys: string[] | null = null;
  private readonly options: ProjectionOptions;
  private position = new Map<string, number>();

  constructor(options: ProjectionOptions = {}) {
    this.options = options;
  }

  /** Event rows followed by the streaming rows, cached until something moves. */
  get rows(): Row[] {
    if (this.composed === null) {
      this.composed = this.deltaRows.length === 0 ? this.eventRows : [...this.eventRows, ...this.deltaRows];
    }
    return this.composed;
  }

  rebuild(events: readonly Event[]): void {
    this.eventRows = [];
    this.position = new Map<string, number>();
    this.composed = null;
    this.composedKeys = null;
    for (const event of events) this.push(event);
  }

  /** Apply one event; returns the event-row index that changed (new or merged). */
  push(event: Event): number {
    const row = rowFromEvent(event, this.options);
    if (row === null) return -1;
    this.composed = null;
    this.composedKeys = null;
    const identity = mergeKey(row);
    if (identity !== null) {
      const at = this.position.get(identity);
      if (at !== undefined) {
        const existing = this.eventRows[at];
        if (existing !== undefined) this.eventRows[at] = mergeRows(existing, row);
        return at;
      }
      this.position.set(identity, this.eventRows.length);
    }
    this.eventRows.push(row);
    return this.eventRows.length - 1;
  }

  /**
   * Replace the advisory delta rows. A delta that is already backed by an event
   * is not in this list: StreamMachine drops it when the event lands.
   */
  setDeltas(deltas: readonly PendingDelta[]): void {
    this.composed = null;
    this.composedKeys = null;
    this.deltaRows = deltas.map((delta) => ({
      kind: 'assistant',
      key: `delta:${String(delta.turn)}:${String(delta.step)}`,
      seq: delta.seq,
      ts: 0,
      text: delta.text,
      streaming: true,
    }));
  }

  /** Row keys in render order, for the virtualizer. Stable while rows are. */
  rowKeys(): string[] {
    if (this.composedKeys !== null) return this.composedKeys;
    const keys = new Array<string>(this.eventRows.length + this.deltaRows.length);
    let at = 0;
    for (const row of this.eventRows) {
      keys[at] = row.key;
      at += 1;
    }
    for (const row of this.deltaRows) {
      keys[at] = row.key;
      at += 1;
    }
    this.composedKeys = keys;
    return keys;
  }
}