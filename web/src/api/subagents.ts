// The subagent tree of one session, as `GET /api/sessions/{id}/subagents` serves it.
//
// Two projections are joined on the server: the parent's `subagent/catalog`
// rows give the shape, and each child's own log gives the timing. The browser
// must not recompute either — it would then be a second owner of the same fact
// and the two would drift.
//
// Everything here is defensive on purpose. The route is newer than the frozen
// protocol module, so an older server answers 404 and a partial one may omit
// members; a panel that raised on either would read as a broken session screen.
// A child the server could not classify keeps its diagnostic and is rendered
// with it rather than dropped: "this child's log is corrupt" is information,
// and hiding the row would make it look like a child that never existed.

export interface SubagentTiming {
  settledMs: number;
  active: boolean;
  lastTurnCompleted?: number;
  turns: number;
}

export interface SubagentDiagnostic {
  kind: string;
  detail: string;
}

export interface SubagentRow {
  sessionId: string;
  name?: string;
  mode: string;
  label?: string;
  path: string;
  depth: number;
  timing?: SubagentTiming;
  diagnostic?: SubagentDiagnostic;
  children: SubagentRow[];
}

export interface SubagentTree {
  session: string;
  count: number;
  subagents: SubagentRow[];
}

const EMPTY_TREE: SubagentTree = { session: '', count: 0, subagents: [] };

function optionalString(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined;
}

function optionalNumber(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function parseTiming(value: unknown): SubagentTiming | undefined {
  if (typeof value !== 'object' || value === null) return undefined;
  const record = value as Record<string, unknown>;
  const settledMs = optionalNumber(record['settledMs']);
  const turns = optionalNumber(record['turns']);
  if (settledMs === undefined || turns === undefined) return undefined;
  const timing: SubagentTiming = {
    settledMs,
    active: record['active'] === true,
    turns,
  };
  const lastTurnCompleted = optionalNumber(record['lastTurnCompleted']);
  if (lastTurnCompleted !== undefined) timing.lastTurnCompleted = lastTurnCompleted;
  return timing;
}

function parseDiagnostic(value: unknown): SubagentDiagnostic | undefined {
  if (typeof value !== 'object' || value === null) return undefined;
  const record = value as Record<string, unknown>;
  const kind = optionalString(record['kind']);
  if (kind === undefined) return undefined;
  return { kind, detail: optionalString(record['detail']) ?? '' };
}

function parseRow(value: unknown): SubagentRow | null {
  if (typeof value !== 'object' || value === null) return null;
  const record = value as Record<string, unknown>;
  const sessionId = optionalString(record['sessionId']);
  if (sessionId === undefined) return null;
  const children: SubagentRow[] = [];
  const raw = record['children'];
  if (Array.isArray(raw)) {
    for (const item of raw) {
      const row = parseRow(item);
      if (row !== null) children.push(row);
    }
  }
  const row: SubagentRow = {
    sessionId,
    mode: optionalString(record['mode']) ?? 'unknown',
    path: optionalString(record['path']) ?? sessionId,
    depth: optionalNumber(record['depth']) ?? 1,
    children,
  };
  const name = optionalString(record['name']);
  if (name !== undefined) row.name = name;
  const label = optionalString(record['label']);
  if (label !== undefined) row.label = label;
  const timing = parseTiming(record['timing']);
  if (timing !== undefined) row.timing = timing;
  const diagnostic = parseDiagnostic(record['diagnostic']);
  if (diagnostic !== undefined) row.diagnostic = diagnostic;
  return row;
}

/** Decode the answer. Anything unrecognisable degrades to an empty tree. */
export function parseTree(value: unknown): SubagentTree {
  if (typeof value !== 'object' || value === null) return { ...EMPTY_TREE };
  const record = value as Record<string, unknown>;
  const subagents: SubagentRow[] = [];
  const raw = record['subagents'];
  if (Array.isArray(raw)) {
    for (const item of raw) {
      const row = parseRow(item);
      if (row !== null) subagents.push(row);
    }
  }
  return {
    session: optionalString(record['session']) ?? '',
    count: optionalNumber(record['count']) ?? countRows(subagents),
    subagents,
  };
}

function countRows(rows: readonly SubagentRow[]): number {
  let total = 0;
  for (const row of rows) total += 1 + countRows(row.children);
  return total;
}

/**
 * Depth-first order, one entry per row. The tree is rendered flat because a
 * nested list buys nothing a left margin does not, and flattening keeps the
 * row keys stable.
 */
export function flatten(rows: readonly SubagentRow[]): SubagentRow[] {
  const out: SubagentRow[] = [];
  for (const row of rows) {
    out.push(row);
    out.push(...flatten(row.children));
  }
  return out;
}

/** `1.5 s` / `420 ms` — how long a child took, from the log, not from a timer. */
export function formatSettled(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${String(Math.round(ms))} ms`;
  const seconds = ms / 1000;
  return seconds >= 10 ? `${String(Math.round(seconds))} s` : `${seconds.toFixed(1)} s`;
}

/** The one-line state of a child: working, finished, or unreadable. */
export function describeState(row: SubagentRow): string {
  if (row.diagnostic !== undefined) {
    return `${row.diagnostic.kind}${row.diagnostic.detail === '' ? '' : `: ${row.diagnostic.detail}`}`;
  }
  const timing = row.timing;
  if (timing === undefined) return 'no log yet';
  if (timing.active) return 'working';
  const turnWord = timing.turns === 1 ? 'turn' : 'turns';
  return `settled in ${formatSettled(timing.settledMs)}, ${String(timing.turns)} ${turnWord}`;
}