// `GET /api/teams` and `GET /api/teams/{board}`: the team boards this machine
// has run, as the server read them off `board.json` and `workers/*.jsonl`.
//
// The endpoint is newer than the frozen protocol module, so this file follows
// `capabilities.ts` rather than casting: nothing here throws, every missing or
// wrong-typed member degrades to an empty list or `null`, and no rendered cell
// can ever receive `undefined`. A board view is a report about somebody else's
// files; one half-written board must not take the other nine off the screen.
//
// Two fields carry meaning beyond "value or not", and both are kept intact
// rather than flattened to a boolean:
//
//   * `workers[].ok` is `true`, `false` **or `null`**. `null` means the
//     transcript had no `worker/end` line — the worker was still running, or was
//     killed, when the server read the board. That is not failure, and it is not
//     success. `workerState()` is the one place that three-way split is decided.
//   * `parallel` is `null` because `hearth team run` resolves it and persists it
//     nowhere. A number would be an invention, so the UI shows "unknown".

/** The three states a worker can be in, as the transcript can prove them. */
export type WorkerState = 'finished' | 'failed' | 'unknown';

/** One row of `board.json`'s `tasks`, reduced to what a board view shows. */
export interface TeamTask {
  /** `board.json`'s id; a task without one is still listed, with `''`. */
  id: string;
  /** Empty when the row had no subject; the server adds a diagnostic then. */
  subject: string;
  /** Passed through exactly: `pending`, `in_progress`, `completed`, or `''`. */
  status: string;
  /** `null` for an unassigned task, which is what the server sends. */
  owner: string | null;
}

/** One `workers/<name>.jsonl` transcript, reduced to its end line. */
export interface TeamWorker {
  name: string;
  /** `true`/`false` from the `worker/end` line; `null` when there was none. */
  ok: boolean | null;
  /** The end line's note, empty when the worker never wrote one. */
  note: string;
  /** JSON lines in the transcript; `null` when the server did not report it. */
  events: number | null;
}

/** One run directory under `<dataDir>/teams/`. */
export interface TeamBoard {
  /** The directory name: unique, and the key the detail route takes. */
  board: string;
  /** The slug without the timestamp; `''` when the directory had no stamp. */
  team: string;
  /** Always `null` from this build; a client must render it as unknown. */
  parallel: number | null;
  /** RFC 3339 in UTC from the directory stamp, or `null` when absent. */
  createdAt: string | null;
  tasks: TeamTask[];
  workers: TeamWorker[];
  /** Human strings; absent on a healthy board. Never parsed, only shown. */
  diagnostics: string[];
}

/** The whole `GET /api/teams` answer. */
export interface TeamBoardReport {
  boards: TeamBoard[];
}

/** How many workers are in each of the three states. */
export interface WorkerStateCounts {
  finished: number;
  failed: number;
  unknown: number;
}

/** One status and how many tasks carry it. */
export interface TaskStatusCount {
  status: string;
  count: number;
}

/** Statuses the task crate writes, in the order a board reads best. */
const STATUS_ORDER: readonly string[] = ['pending', 'in_progress', 'completed'];

function text(value: unknown): string {
  return typeof value === 'string' ? value : '';
}

/** A non-negative number, or `null`. Counts are reported, never derived. */
function count(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

/** Parse the objects of one array, dropping entries the parser refuses. */
function rows<T>(value: unknown, parse: (raw: unknown) => T | null): T[] {
  if (!Array.isArray(value)) return [];
  const out: T[] = [];
  for (const raw of value) {
    const parsed = parse(raw);
    // A refused row is skipped, never rendered as a hole: the server always
    // sends the members it addresses, so this is the malformed case only.
    if (parsed !== null) out.push(parsed);
  }
  return out;
}

/** The strings of one array, blanks dropped (an empty diagnostic says nothing). */
function strings(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const out: string[] = [];
  for (const raw of value) {
    const line = text(raw);
    if (line !== '') out.push(line);
  }
  return out;
}

function task(raw: unknown): TeamTask | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const row = raw as { id?: unknown; subject?: unknown; status?: unknown; owner?: unknown };
  // `owner: ''` and `owner: null` mean the same thing on a board — nobody holds
  // the task — so an empty string is normalised to `null` rather than rendered
  // as an empty owner cell.
  const owner = text(row.owner);
  return {
    id: text(row.id),
    subject: text(row.subject),
    status: text(row.status),
    owner: owner === '' ? null : owner,
  };
}

function worker(raw: unknown): TeamWorker | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const row = raw as { name?: unknown; ok?: unknown; note?: unknown; events?: unknown };
  const name = text(row.name);
  // A transcript with no name is not addressable: the file name is the only
  // identity a worker has, and the server skips such a file too.
  if (name === '') return null;
  return {
    name,
    // Only a literal `true` is success and only a literal `false` is failure.
    // Anything else — `null`, missing, a stray string — is "cannot say", which
    // is the honest reading and the direction that does not accuse the worker.
    ok: row.ok === true ? true : row.ok === false ? false : null,
    note: text(row.note),
    events: count(row.events),
  };
}

/**
 * Decode one board (`GET /api/teams/{board}` is this object unwrapped).
 *
 * `null` when the value is not an object or carries no `board` name: a row with
 * no name could not be keyed, addressed or expanded, and inventing a label for
 * it would be worse than leaving it out.
 */
export function parseBoard(value: unknown): TeamBoard | null {
  if (typeof value !== 'object' || value === null) return null;
  const body = value as Record<string, unknown>;
  const board = text(body.board);
  if (board === '') return null;
  const createdAt = text(body.createdAt);
  return {
    board,
    team: text(body.team),
    parallel: count(body.parallel),
    createdAt: createdAt === '' ? null : createdAt,
    tasks: rows(body.tasks, task),
    workers: rows(body.workers, worker),
    diagnostics: strings(body.diagnostics),
  };
}

/**
 * Decode `GET /api/teams`.
 *
 * An answer that is not an object, or has no `boards` array, reads as "no
 * boards": a page about boards must not fail because a proxy answered HTML.
 */
export function parseBoards(value: unknown): TeamBoardReport {
  const body = typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : {};
  return { boards: rows(body.boards, parseBoard) };
}

/** Which of the three states one worker is in, and the only place it is decided. */
export function workerState(worker: TeamWorker): WorkerState {
  if (worker.ok === true) return 'finished';
  if (worker.ok === false) return 'failed';
  return 'unknown';
}

/** Count workers by state, including the states with zero. */
export function workerStateCounts(workers: readonly TeamWorker[]): WorkerStateCounts {
  const counts: WorkerStateCounts = { finished: 0, failed: 0, unknown: 0 };
  for (const worker of workers) counts[workerState(worker)] += 1;
  return counts;
}

/**
 * Count tasks by status.
 *
 * The three statuses the task crate writes come first, in board order; a value
 * this build has not heard of is still counted and listed after them, because
 * the server passes unknown statuses through on purpose. The list is empty for a
 * board with no tasks, which the component says out loud.
 */
export function taskStatusCounts(tasks: readonly TeamTask[]): TaskStatusCount[] {
  const seen = new Map<string, number>();
  for (const task of tasks) seen.set(task.status, (seen.get(task.status) ?? 0) + 1);
  const out: TaskStatusCount[] = [];
  for (const status of STATUS_ORDER) {
    const found = seen.get(status);
    if (found === undefined) continue;
    out.push({ status, count: found });
    seen.delete(status);
  }
  for (const status of [...seen.keys()].sort()) {
    out.push({ status, count: seen.get(status) ?? 0 });
  }
  return out;
}