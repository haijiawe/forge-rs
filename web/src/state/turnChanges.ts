// What each turn of the conversation actually changed, folded out of the log.
//
// The log is the only witness. A turn's writes are in it as `tool/result`
// events, and since the file tools now report the line ranges they touched
// (`meta.lines`), a reader can walk from "round three touched these two files"
// back to the lines themselves. Nothing is snapshotted: the answer is computed
// from events every time it is asked for, so a log written by an older build
// simply has no turn-change rows — an empty list, not a wrong one.
//
// Two honesty rules:
//
//  * Only writes that named their lines count. The shell tool runs commands and
//    reports no ranges, so a turn that edited files through a script shows
//    nothing here. The panel says so rather than implying the turn was clean.
//  * A path written twice in one turn is listed once. The ranges describe the
//    last write, because they are line numbers in the text that exists now; the
//    write count is kept so the row does not look like a single edit.

import { eventAs } from '../api/events';
import type { Event, JsonValue } from '../api/events';

/** One run of changed lines in the file as it stands: 1-based, closed. */
export interface TurnChangeRange {
  from: number;
  to: number;
}

/** One file a turn wrote, with the lines it touched on the last write. */
export interface TurnChangeFile {
  path: string;
  /** Lines added by the last write to this path in this turn. */
  added: number;
  /** Lines removed by the last write. */
  removed: number;
  /** The changed runs, coalesced by the writer. Empty for a deletion-only write. */
  ranges: TurnChangeRange[];
  /** True when the file was too big to align and `ranges` is one coarse block. */
  coarse: boolean;
  /** True when the writer dropped ranges past its cap; the marks are incomplete. */
  cut: boolean;
  /** How many writes touched this path in this turn. Never zero. */
  writes: number;
}

/** One turn that wrote at least one file with line information. */
export interface TurnChangeTurn {
  turn: number;
  files: TurnChangeFile[];
  added: number;
  removed: number;
}

function numberAt(source: Record<string, JsonValue>, key: string): number | null {
  const value = source[key];
  if (typeof value !== 'number' || !Number.isFinite(value)) return null;
  return value;
}

function objectAt(source: Record<string, JsonValue>, key: string): Record<string, JsonValue> | null {
  const value = source[key];
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
  return value as Record<string, JsonValue>;
}

function rangesAt(source: Record<string, JsonValue>): TurnChangeRange[] {
  const raw = source['ranges'];
  if (!Array.isArray(raw)) return [];
  const out: TurnChangeRange[] = [];
  for (const entry of raw) {
    if (entry === null || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const from = numberAt(entry as Record<string, JsonValue>, 'from');
    const to = numberAt(entry as Record<string, JsonValue>, 'to');
    if (from === null || to === null) continue;
    if (!Number.isInteger(from) || !Number.isInteger(to)) continue;
    if (from < 1 || to < from) continue;
    out.push({ from, to });
  }
  return out;
}

/**
 * Read one `tool/result` as a file change, or null when it is not one.
 *
 * The tool name is deliberately not checked: any tool that reports `path` and
 * `lines` has earned a row, and hard-coding the two names this build ships
 * would hide a plugin that follows the same convention.
 */
function fileChangeOf(event: Event): TurnChangeFile | null {
  const typed = eventAs(event, 'tool/result');
  if (typed === null) return null;
  const meta = typed.payload?.meta;
  if (meta === undefined || meta === null) return null;
  const record = meta as Record<string, JsonValue>;
  const path = record['path'];
  if (typeof path !== 'string' || path === '') return null;
  const lines = objectAt(record, 'lines');
  if (lines === null) return null;
  const added = numberAt(lines, 'added');
  const removed = numberAt(lines, 'removed');
  if (added === null || removed === null) return null;
  return {
    path,
    added,
    removed,
    ranges: rangesAt(lines),
    coarse: lines['coarse'] === true,
    cut: lines['cut'] === true,
    writes: 1,
  };
}

/**
 * Every turn in the log that wrote a file with line information, oldest first.
 *
 * Events without a turn number are skipped: the contract says a record outside
 * a turn belongs to no round, and folding it into one would invent an owner.
 * The list is ordered by turn, and within a turn by the order the paths were
 * first written, so a re-render does not shuffle rows under the reader's hand.
 */
export function turnChangesFrom(events: readonly Event[]): TurnChangeTurn[] {
  const turns = new Map<number, { order: string[]; files: Map<string, TurnChangeFile> }>();
  for (const event of events) {
    const turn = event.turn;
    if (turn === undefined || turn <= 0) continue;
    const change = fileChangeOf(event);
    if (change === null) continue;
    let bucket = turns.get(turn);
    if (bucket === undefined) {
      bucket = { order: [], files: new Map() };
      turns.set(turn, bucket);
    }
    const previous = bucket.files.get(change.path);
    if (previous === undefined) bucket.order.push(change.path);
    bucket.files.set(change.path, {
      ...change,
      writes: (previous?.writes ?? 0) + 1,
    });
  }
  const out: TurnChangeTurn[] = [];
  for (const [turn, bucket] of [...turns.entries()].sort((a, b) => a[0] - b[0])) {
    const files = bucket.order.map((path) => bucket.files.get(path) as TurnChangeFile);
    out.push({
      turn,
      files,
      added: files.reduce((sum, file) => sum + file.added, 0),
      removed: files.reduce((sum, file) => sum + file.removed, 0),
    });
  }
  return out;
}

/** "第 3 行" / "第 3–7 行" / "第 3 行起 4 行" — one label per run. */
export function rangeLabel(range: TurnChangeRange): string {
  if (range.to <= range.from) return `第 ${String(range.from)} 行`;
  return `第 ${String(range.from)}–${String(range.to)} 行`;
}
