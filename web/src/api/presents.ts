// `GET /api/presents`: the files the model handed over with the `present` tool,
// as the server read them off `<dataDir>/presented.jsonl`.
//
// The endpoint is newer than the frozen protocol module, so this file parses
// defensively rather than casting: nothing here throws, a row that is not an
// object is dropped, and a missing or wrong-typed member degrades to a default
// rather than reaching a rendered cell as `undefined`. A handoff list is a
// report about somebody else's filesystem; one unreadable row must not take the
// others off the screen.
//
// Two things this module deliberately does **not** do:
//
//   * It does not stat, open or verify a path. The browser cannot see the
//     operator's disk, and the manifest records what the file measured when it
//     was presented — so a path that no longer exists is still a listed handoff,
//     labelled with the size it had at the time. Claiming freshness would be a
//     guess.
//   * It does not turn `size` or `ts` into prose beyond what the manifest holds.
//     `formatBytes` is exact arithmetic on the recorded number; `formatWhen`
//     renders the recorded stamp, and says so when there is none.

/** One handoff, as `presented.jsonl` records it and the route returns it. */
export interface Present {
  /** Unix milliseconds recorded at the handoff; `0` when the line had none. */
  ts: number;
  /** Absolute path at the time of the handoff; never empty for a listed row. */
  path: string;
  /** The size the file measured then, not now; `0` when unrecorded. */
  size: number;
  /** The model's one-line note; `''` when it wrote none. */
  description: string;
}

/**
 * The route's answer.
 *
 * `skipped` and `truncated` are kept rather than folded away: "the list you are
 * reading is not the whole file" is information the operator needs, and a
 * component that cannot say it will render a short list as if it were complete.
 */
export interface PresentReport {
  presents: Present[];
  /** Lines that were not usable JSON objects, or had no path. */
  skipped: number;
  /** True when the entry count or the byte cap cut the list short. */
  truncated: boolean;
  /** Present only when `truncated` alone would not explain the answer. */
  note: string | null;
}

const EMPTY: PresentReport = { presents: [], skipped: 0, truncated: false, note: null };

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function asString(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

function asCount(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : 0;
}

/**
 * One manifest line, or `null` when it names no path.
 *
 * A row without a path is the one thing that cannot be shown: there is nothing
 * for the operator to open. Everything else has a defensible default.
 */
export function parsePresent(value: unknown): Present | null {
  const row = asRecord(value);
  if (row === null) return null;
  const path = asString(row.path);
  if (path === '') return null;
  return {
    ts: asCount(row.ts),
    path,
    size: asCount(row.size),
    description: asString(row.description),
  };
}

/** The whole answer, accepting either the wrapper or a bare list. */
export function parseReport(value: unknown): PresentReport {
  // A bare array is what a caller reaches for when hand-rolling a fixture; the
  // route never sends one, but accepting it keeps the failure honest instead of
  // rendering an empty pane for a shape that clearly had rows.
  if (Array.isArray(value)) {
    return { ...EMPTY, presents: value.map(parsePresent).filter((row): row is Present => row !== null) };
  }
  const body = asRecord(value);
  if (body === null) return EMPTY;
  const rows = Array.isArray(body.presents) ? body.presents : [];
  const note = asString(body.note);
  return {
    presents: rows.map(parsePresent).filter((row): row is Present => row !== null),
    skipped: asCount(body.skipped),
    truncated: body.truncated === true,
    note: note === '' ? null : note,
  };
}

/** Bytes as the manifest recorded them, in the units a person reads. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '大小未知';
  if (bytes === 0) return '0 B';
  if (bytes < 1024) return `${String(bytes)} B`;
  const kib = bytes / 1024;
  if (kib < 1024) return `${kib.toFixed(1)} KB`;
  return `${(kib / 1024).toFixed(1)} MB`;
}

/**
 * The recorded stamp as UTC, or a plain statement that there is none.
 *
 * `0` means the manifest line carried no `ts`, which is a fact about the record
 * and not a time; rendering it as 1970 would invent one.
 */
export function formatWhen(ts: number): string {
  if (!Number.isFinite(ts) || ts <= 0) return '时间未记录';
  const when = new Date(ts);
  if (Number.isNaN(when.getTime())) return '时间未记录';
  const iso = when.toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 19)}Z`;
}

/** The file name in a path, for a heading too narrow for the whole thing. */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  const last = parts[parts.length - 1];
  return last === undefined || last === '' ? path : last;
}