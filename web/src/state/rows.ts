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
//
// A `context/compressed` block is a note about the transcript, not a turn: its
// row names the seq range and the summary, and the rows inside that range stay
// on screen marked `foldedBy` — the operator has to be able to see what the
// model was not sent, so a block never deletes or reorders a row.

import type { ApprovalDecision } from '../api/types';
import type { Event, EventEnvelope } from '../api/events';
import type { PendingDelta } from '../api/stream';

export interface RowBase {
  /** Stable identity across appends and prepends: the virtualizer keys on it. */
  key: string;
  seq: number;
  ts: number;
  /**
   * blockId of the compressed block that hid this row's seq from the model, set
   * by {@link RowAccumulator.setBlocks}. Absent means no block covers the row.
   */
  foldedBy?: string;
}

export interface UserRow extends RowBase {
  kind: 'user';
  text: string;
  attachments: number;
  /** What the operator attached, as the log recorded it: a name and a size. */
  attachmentLabels: string[];
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

/**
 * A `context/compressed` note: for this window only, the model was sent
 * `summary` instead of the records in seq `from..to` inclusive (events-v1.md
 * §4.0.3). The hidden rows are still on screen, marked `foldedBy`.
 */
export interface CompressedRow extends RowBase {
  kind: 'compressed';
  /** 32 hex chars when the writer followed the contract; `''` when it did not. */
  blockId: string;
  /** First hidden seq, inclusive. Absent when the payload did not carry one. */
  from?: number;
  /** Last hidden seq, inclusive. Absent when the payload did not carry one. */
  to?: number;
  /** The replacement text: what the model reads instead of the range. */
  summary: string;
  /** How many projected messages fell in the range, when the writer said. */
  messages?: number;
  /** True when the payload cannot describe a complete block: shown, never dropped. */
  incomplete: boolean;
  /** What is wrong with the payload, for the operator. Absent when complete. */
  problem?: string;
}

/**
 * A compressed block as the transcript needs it: a closed, usable seq range.
 * Built by {@link compressedBlocks} from the log, which is the only source.
 */
export interface CompressedBlock {
  blockId: string;
  /** First hidden seq, inclusive. */
  from: number;
  /** Last hidden seq, inclusive. */
  to: number;
  /** The seq of the event that wrote the block. */
  seq: number;
  /**
   * True when `to` is past the highest seq this reader has seen. The range is
   * then still open here: the writer covered records this window has not been
   * sent yet, so rows that arrive later inside the range get marked too (see
   * `compressedBlocks`). Not an error, and not worth a banner.
   */
  toBeyondHead: boolean;
  messages?: number;
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
  | CompressedRow
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

/** A seq in the shape the log writes it: a safe integer of at least 1. */
function seqNumber(value: unknown): number | undefined {
  const parsed = num(value);
  if (parsed === undefined || !Number.isSafeInteger(parsed) || parsed < 1) {
    return undefined;
  }
  return parsed;
}

/** `name (12 KB)`, or `image (12 KB)` when the client sent no name. */
function attachmentLabel(entry: unknown): string {
  if (typeof entry !== 'object' || entry === null) {
    return 'attachment';
  }
  const record = entry as Record<string, unknown>;
  const name = str(record['name']) ?? str(record['path']) ?? str(record['kind']) ?? 'attachment';
  const bytes = num(record['bytes']);
  return bytes === undefined ? name : `${name} (${formatBytes(bytes)})`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${String(bytes)} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
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
    case 'user/message': {
      // The log records metadata, not bodies: `id` names a file in the data
      // directory's blob store. A row therefore shows what was attached, never
      // the bytes themselves — those would be a base64 copy of every upload in
      // every reader's memory.
      const refs = Array.isArray(payload['attachments']) ? (payload['attachments'] as unknown[]) : [];
      const labels = refs.map((entry) => attachmentLabel(entry));
      return {
        ...base,
        kind: 'user',
        text: str(payload['text']) ?? '',
        attachments: refs.length,
        attachmentLabels: labels,
      };
    }
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
    case 'context/compressed': {
      // §4.0.3: blockId, from, to (inclusive seq), summary, optional messages.
      // Payload schemas are descriptive, not enforced (§4), and this window may
      // also be reading a log written by a newer harness, so every field is
      // optional here. A block that cannot describe itself still becomes a row:
      // it prints what it could not read and says `incomplete`, because a
      // silently dropped block would hide the fact that rows are hidden.
      const blockId = str(payload['blockId']) ?? '';
      const summary = str(payload['summary']);
      const from = seqNumber(payload['from']);
      const to = seqNumber(payload['to']);
      const messages = num(payload['messages']);
      const problems: string[] = [];
      if (blockId === '') problems.push('no blockId');
      if (from === undefined) problems.push('no from seq');
      if (to === undefined) problems.push('no to seq');
      if (from !== undefined && to !== undefined && from > to) {
        problems.push('from is after to');
      }
      if (summary === undefined || summary === '') problems.push('no summary');
      return {
        ...base,
        kind: 'compressed',
        blockId,
        ...(from === undefined ? {} : { from }),
        ...(to === undefined ? {} : { to }),
        summary: summary ?? '',
        ...(messages === undefined ? {} : { messages }),
        incomplete: problems.length > 0,
        ...(problems.length === 0 ? {} : { problem: problems.join(', ') }),
      };
    }
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
 * The compressed blocks written in these events, sorted by `from`.
 *
 * O(events): a single walk for the highest seq and a single walk for the
 * blocks, never a rescan per block, so the store can call this on every flush
 * (the same cost as `latestToolNames`).
 *
 * A block whose payload does not describe a usable range (`from`/`to` missing,
 * inverted, non-integer) is left out: a note that cannot say what it hides must
 * not hide anything. Its row still renders, marked `incomplete`.
 *
 * `headSeq` is the highest seq this reader has been sent — the store passes the
 * stream's `lastSeq`, which knows about seqs no event has described yet.
 */
export function compressedBlocks(
  events: readonly Event[],
  headSeq?: number,
): CompressedBlock[] {
  return blocksFromScan(collectBlocks(emptyBlockScan(), events, headSeq));
}

/**
 * What a transcript reader has already walked for compressed blocks.
 *
 * The store publishes once per animation frame, and the log only ever grows, so
 * re-walking it every frame is work that grows with the session and never
 * shrinks: at 100k events that was 100k row decodes per frame for a handful of
 * blocks. This keeps the walk's result and how far it got, so a flush costs the
 * events that arrived since the last one.
 */
export interface BlockScan {
  /** Ranges found so far, sorted by `from`. */
  ranges: BlockRange[];
  /** How many of the reader's events have been walked. */
  scanned: number;
  /** Highest seq seen, including the stream's own head. */
  head: number;
}

/** A block without the reader-relative `toBeyondHead` judgement. */
export type BlockRange = Omit<CompressedBlock, 'toBeyondHead'>;

export function emptyBlockScan(): BlockScan {
  return { ranges: [], scanned: 0, head: 0 };
}

/**
 * Extend a scan with the events it has not walked yet.
 *
 * `scan.scanned` indexes into the reader's event array, which only grows — the
 * stream machine pushes onto it and never reassigns. If that ever changes, an
 * `events` shorter than `scan.scanned` starts the walk again instead of keeping
 * a stale tail.
 */
export function collectBlocks(
  scan: BlockScan,
  events: readonly Event[],
  headSeq?: number,
): BlockScan {
  const reset = events.length < scan.scanned;
  const ranges = reset ? [] : scan.ranges;
  let head = reset ? 0 : scan.head;
  if (typeof headSeq === 'number' && Number.isFinite(headSeq) && headSeq > head) {
    head = headSeq;
  }
  let added = false;
  for (let index = reset ? 0 : scan.scanned; index < events.length; index += 1) {
    const event = events[index];
    if (event === undefined) continue;
    const seq = seqOf(event);
    if (seq > head) head = seq;
    if (event.type !== 'context/compressed') continue;
    const row = rowFromEvent(event);
    if (row === null || row.kind !== 'compressed') continue;
    const { from, to } = row;
    if (from === undefined || to === undefined || from > to) continue;
    ranges.push({
      blockId: row.blockId,
      from,
      to,
      seq: row.seq,
      ...(row.messages === undefined ? {} : { messages: row.messages }),
    });
    added = true;
  }
  if (added) ranges.sort((left, right) => left.from - right.from || left.to - right.to);
  return { ranges, scanned: events.length, head };
}

/**
 * The blocks a scan describes, with `toBeyondHead` judged against its own head.
 *
 * Kept separate from {@link collectBlocks} because the head grows after a block
 * was found: a range that ran past the reader's events stops doing so once those
 * events arrive, and a cached `true` would then be stale forever.
 */
export function blocksFromScan(scan: BlockScan): CompressedBlock[] {
  return scan.ranges.map((range) => ({
    ...range,
    toBeyondHead: range.to > scan.head,
  }));
}

/**
 * The block hiding `seq`, if any. `blocks` comes from {@link compressedBlocks}:
 * sorted by `from`, and non-overlapping per §4.0.3 invariant 2, so one binary
 * search finds the only candidate — O(log blocks), and the accumulator only
 * asks when its block list changes, not per row per frame.
 *
 * A log that breaks the no-overlap invariant falls back to a scan, so a bad
 * writer cannot make a hidden row look as if the model was sent it.
 */
export function blockHiding(
  seq: number,
  blocks: readonly CompressedBlock[],
): CompressedBlock | undefined {
  let low = 0;
  let high = blocks.length - 1;
  let candidate: CompressedBlock | undefined;
  while (low <= high) {
    const middle = (low + high) >> 1;
    const block = blocks[middle];
    if (block === undefined) break;
    if (block.from <= seq) {
      candidate = block;
      low = middle + 1;
    } else {
      high = middle - 1;
    }
  }
  if (candidate !== undefined && seq <= candidate.to) return candidate;
  return blocks.find((block) => block.from <= seq && seq <= block.to);
}

/** A copy of `row` carrying `blockId` as its fold mark, or no mark at all. */
function foldRow<T extends RowBase>(row: T, blockId: string | undefined): T {
  const marked: T = { ...row };
  if (blockId === undefined) {
    delete marked.foldedBy;
  } else {
    marked.foldedBy = blockId;
  }
  return marked;
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
  accumulator.setBlocks(compressedBlocks(events));
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
  private blocks: readonly CompressedBlock[] = [];
  private blockSignature = '';

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
    this.blockSignature = '';
    for (const event of events) this.push(event);
    this.setBlocks(this.blocks);
  }

  /**
   * Mark the rows the model was not sent, from the block list the store rebuilt
   * for this flush.
   *
   * The whole pass is O(rows) and only runs when the block set moved; a row is
   * cloned only when its mark actually changes, so a memoized row view keeps its
   * identity while nothing about the fold changed. Rows are never removed: the
   * mark says "the model did not get this", it does not hide it from the
   * operator.
   */
  setBlocks(blocks: readonly CompressedBlock[]): void {
    const signature = blocks
      .map((block) => `${block.blockId}:${String(block.from)}:${String(block.to)}`)
      .join(';');
    if (signature === this.blockSignature) return;
    this.blockSignature = signature;
    this.blocks = blocks;
    for (let at = 0; at < this.eventRows.length; at += 1) {
      const row = this.eventRows[at];
      if (row === undefined) continue;
      // A compressed row is a note about the log, not a record the model was
      // ever sent, so a range that happens to contain it does not mark it: a
      // note that hides itself reads as a bug, not as information.
      if (row.kind === 'compressed') continue;
      const desired = blockHiding(row.seq, blocks)?.blockId;
      if (row.foldedBy !== desired) this.eventRows[at] = foldRow(row, desired);
    }
    this.composed = null;
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
        this.markRow(at);
        return at;
      }
      this.position.set(identity, this.eventRows.length);
    }
    this.eventRows.push(row);
    const inserted = this.eventRows.length - 1;
    this.markRow(inserted);
    return inserted;
  }

  /**
   * Fold one event row, if the current block list covers it. Cheap enough for
   * the push path (O(log blocks)) and the reason `setBlocks` can skip its whole
   * pass when nothing about the blocks moved: a row that arrives inside a range
   * that is still open is marked here, without re-walking the transcript.
   */
  private markRow(at: number): void {
    if (this.blocks.length === 0) return;
    const row = this.eventRows[at];
    if (row === undefined) return;
    // A compressed row is a note about the log, not a record the model was ever
    // sent, so a range that happens to contain it does not mark it.
    if (row.kind === 'compressed') return;
    const desired = blockHiding(row.seq, this.blocks)?.blockId;
    if (row.foldedBy !== desired) this.eventRows[at] = foldRow(row, desired);
  }

  /**
   * Replace the advisory delta rows. A delta that is already backed by an event
   * is not in this list: StreamMachine drops it when the event lands.
   */
  setDeltas(deltas: readonly PendingDelta[]): void {
    this.composed = null;
    this.composedKeys = null;
    this.deltaRows = deltas.map((delta) => {
      const row: AssistantRow = {
        kind: 'assistant',
        key: `delta:${String(delta.turn)}:${String(delta.step)}`,
        seq: delta.seq,
        ts: 0,
        text: delta.text,
        streaming: true,
      };
      // A delta normally sits past every block, but a block whose `to` runs past
      // the head covers seqs this window has not been sent yet — including the
      // streaming text. Marking it keeps the deltas as honest as the rows below.
      const block = blockHiding(delta.seq, this.blocks);
      return block === undefined ? row : foldRow(row, block.blockId);
    });
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