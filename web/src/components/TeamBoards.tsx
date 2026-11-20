// The team-board view: what `hearth team run` left on this machine, as one list.
//
// Three things this pane refuses to do, because the API refuses to know them:
//
//   * It does not call an unfinished worker a failure. `ok: null` is its own
//     state ("unknown") with its own class, because a board read mid-run must not
//     make every worker look broken.
//   * It does not print a parallelism number. `parallel` is always `null` in this
//     build — nothing persists it — so the cell says unknown rather than a
//     plausible default.
//   * It does not derive a percentage, a progress bar or a "time ago" from data
//     that is not there. Counts come from the rows the server sent; nothing else
//     is computed.
//
// It is also read-only and cheap: `load` is one GET, the detail of a board is the
// data already in hand, and expanding a board starts no request.
import { Fragment, useEffect, useId, useRef, useState } from 'react';
import { ApiError } from '../api/client';
import {
  parseBoards,
  taskStatusCounts,
  workerState,
  workerStateCounts,
  type TaskStatusCount,
  type TeamBoard,
  type TeamBoardReport,
  type TeamTask,
  type TeamWorker,
  type WorkerState,
} from '../api/teams';

export interface TeamBoardsProps {
  /**
   * Reads `GET /api/teams` (or the unwrapped `GET /api/teams/{board}`) straight
   * from the client. The component owns the request so the failure state is
   * always the one the contract describes: the error code, plus where the CLI
   * can still answer. It may return the decoded report or the raw body —
   * `parseBoards` accepts either.
   *
   * Pass a stable function (`useCallback`) or a module-level one; a new
   * identity each render is tolerated (it is held in a ref), but the pane then
   * will not re-read when you meant it to.
   */
  load: () => Promise<unknown>;
}

/** The three states, with the evidence each one stands on. */
const STATE_TEXT: Record<WorkerState, { label: string; hint: string }> = {
  finished: {
    label: 'finished',
    hint: 'the transcript has a worker/end line whose ok flag is true',
  },
  failed: {
    label: 'failed',
    hint: 'the transcript has a worker/end line whose ok flag is false',
  },
  unknown: {
    label: 'unknown',
    hint: 'no worker/end line: this build cannot say whether it finished',
  },
};

/** A class-name-safe slug for a status this build may never have seen. */
function statusClass(status: string): string {
  const slug = status.replace(/[^A-Za-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '');
  return slug === '' ? 'none' : slug.toLowerCase();
}

function statusLabel(status: string): string {
  return status === '' ? 'status not recorded' : status;
}

/** `n task(s)`, so a count of one does not read as a plural. */
function plural(count: number, one: string, many: string): string {
  return `${String(count)} ${count === 1 ? one : many}`;
}

function describeError(cause: unknown): string {
  // The envelope's code is the stable half; the message is for the operator.
  if (cause instanceof ApiError) return `${cause.code}: ${cause.message}`;
  return cause instanceof Error ? cause.message : String(cause);
}

function TaskCounts({ counts, total }: { counts: readonly TaskStatusCount[]; total: number }) {
  if (total === 0) {
    return (
      <p className="team-board-tasks team-board-tasks-empty">
        no tasks on this board — its <code>board.json</code> lists none, or could not
        be read; the diagnostics say which.
      </p>
    );
  }
  return (
    <p className="team-board-tasks">
      tasks <span className="team-board-total">{String(total)}</span>
      <span className="muted"> — </span>
      {counts.map((entry, index) => (
        <Fragment key={`${entry.status}:${String(index)}`}>
          {index === 0 ? null : <span className="muted"> · </span>}
          <span className={`team-task-count team-task-count-${statusClass(entry.status)}`}>
            {statusLabel(entry.status)} {String(entry.count)}
          </span>
        </Fragment>
      ))}
    </p>
  );
}

function StateCounts({ workers }: { workers: readonly TeamWorker[] }) {
  const counts = workerStateCounts(workers);
  const states: readonly WorkerState[] = ['finished', 'failed', 'unknown'];
  return (
    <p className="team-board-workers">
      workers <span className="team-board-total">{String(workers.length)}</span>
      <span className="muted"> — </span>
      {states.map((state, index) => (
        <Fragment key={state}>
          {index === 0 ? null : <span className="muted"> · </span>}
          <span className={`team-worker-count team-worker-count-${state}`}>
            {STATE_TEXT[state].label} {String(counts[state])}
          </span>
        </Fragment>
      ))}
    </p>
  );
}

function TaskTable({ tasks }: { tasks: readonly TeamTask[] }) {
  if (tasks.length === 0) {
    return <p className="muted team-board-note">no task rows to show</p>;
  }
  return (
    <table className="team-board-table">
      <thead>
        <tr>
          <th scope="col">id</th>
          <th scope="col">task</th>
          <th scope="col">status</th>
          <th scope="col">owner</th>
        </tr>
      </thead>
      <tbody>
        {tasks.map((task, index) => (
          <tr key={`${task.id}:${String(index)}`}>
            <td className="mono">{task.id === '' ? '—' : task.id}</td>
            <td>{task.subject === '' ? <span className="muted">no subject</span> : task.subject}</td>
            <td
              className={`team-task-status team-task-status-${statusClass(task.status)}`}
              title={statusLabel(task.status)}
            >
              {statusLabel(task.status)}
            </td>
            <td className="team-task-owner">
              {task.owner === null ? (
                <span className="muted">unassigned</span>
              ) : (
                <span className="mono">{task.owner}</span>
              )}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function WorkerTable({ workers }: { workers: readonly TeamWorker[] }) {
  if (workers.length === 0) {
    return <p className="muted team-board-note">no worker transcripts to show</p>;
  }
  return (
    <table className="team-board-table">
      <thead>
        <tr>
          <th scope="col">worker</th>
          <th scope="col">state</th>
          <th scope="col">events</th>
          <th scope="col">note</th>
        </tr>
      </thead>
      <tbody>
        {workers.map((worker) => {
          const state = workerState(worker);
          return (
            <tr key={worker.name}>
              <td className="mono">{worker.name}</td>
              <td>
                {/* Three states, three classes: `unknown` is not a muted
                    failure, it is the absence of an answer, and the pill says
                    why in its title instead of guessing. */}
                <span
                  className={`pill team-worker-state team-worker-state-${state}`}
                  title={STATE_TEXT[state].hint}
                >
                  {STATE_TEXT[state].label}
                </span>
              </td>
              <td className="mono team-worker-events">
                {worker.events === null ? (
                  <span className="muted">not reported</span>
                ) : (
                  String(worker.events)
                )}
              </td>
              <td className="team-worker-note">
                {worker.note === '' ? (
                  <span className="muted">
                    {state === 'unknown' ? 'no worker/end line was written' : 'no note'}
                  </span>
                ) : (
                  worker.note
                )}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function BoardItem({
  board,
  open,
  onToggle,
}: {
  board: TeamBoard;
  open: boolean;
  onToggle: (board: string) => void;
}) {
  // The detail region needs an id `aria-controls` can name. `useId` gives one
  // that is unique per board and safe for a directory name that may contain
  // characters an id could not.
  const detailId = useId();
  const counts = taskStatusCounts(board.tasks);
  const parallel =
    board.parallel === null ? (
      <span className="team-board-parallel team-board-parallel-unknown" title="nothing persists the parallelism yet">
        unknown
      </span>
    ) : (
      <span className="team-board-parallel mono">{String(board.parallel)}</span>
    );

  return (
    <li className="team-board">
      <div className="team-board-summary">
        <h3 className="team-board-title">
          <span className="mono team-board-team">
            {board.team === '' ? 'team not recorded' : board.team}
          </span>{' '}
          <span className="muted team-board-id">{board.board}</span>
        </h3>
        <p className="team-board-meta">
          ran{' '}
          {board.createdAt === null ? (
            <span className="muted team-board-when">when unknown</span>
          ) : (
            // The server stamps UTC and says so; the raw string is shown rather
            // than a localised one, because a board's clock is the server's.
            <time className="mono team-board-when" dateTime={board.createdAt}>
              {board.createdAt}
            </time>
          )}
          <span className="muted"> · </span>
          parallelism {parallel}
        </p>
        <TaskCounts counts={counts} total={board.tasks.length} />
        <StateCounts workers={board.workers} />
        {board.diagnostics.length > 0 && (
          <p className="team-board-diagnostic-badge">
            <span className="pill pill-warn" title="the strings are for humans; the pane shows them, it does not parse them">
              {plural(board.diagnostics.length, 'diagnostic', 'diagnostics')}
            </span>
          </p>
        )}
        <button
          type="button"
          className="button team-board-toggle"
          aria-expanded={open}
          aria-controls={detailId}
          aria-label={`${open ? 'hide' : 'show'} details for ${board.board}`}
          onClick={() => {
            onToggle(board.board);
          }}
        >
          {open ? 'hide details' : 'show details'}
        </button>
      </div>
      {open && (
        <div className="team-board-detail" id={detailId}>
          {board.diagnostics.length > 0 && (
            <>
              <h4 className="team-board-detail-head">diagnostics</h4>
              <ul className="team-board-diagnostics">
                {board.diagnostics.map((line, index) => (
                  <li key={`${String(index)}:${line}`}>{line}</li>
                ))}
              </ul>
            </>
          )}
          <h4 className="team-board-detail-head">tasks</h4>
          <TaskTable tasks={board.tasks} />
          <h4 className="team-board-detail-head">workers</h4>
          <WorkerTable workers={board.workers} />
        </div>
      )}
    </li>
  );
}

export function TeamBoards({ load }: TeamBoardsProps) {
  const [report, setReport] = useState<TeamBoardReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reading, setReading] = useState(true);
  const [attempt, setAttempt] = useState(0);
  const [expanded, setExpanded] = useState<readonly string[]>([]);

  // The loader is kept in a ref so a parent that passes a fresh arrow each
  // render does not turn into a fetch loop; the effect below re-runs only on an
  // explicit retry, which is the one time a second GET is intended.
  const loadRef = useRef(load);
  useEffect(() => {
    loadRef.current = load;
  }, [load]);

  useEffect(() => {
    let live = true;
    setReading(true);
    setError(null);
    loadRef
      .current()
      .then((value: unknown) => {
        if (!live) return;
        setReport(parseBoards(value));
      })
      .catch((cause: unknown) => {
        if (!live) return;
        // The report is cleared: half a page of stale boards beside an error
        // would read as "these are current and that one failed".
        setReport(null);
        setError(describeError(cause));
      })
      .finally(() => {
        if (live) setReading(false);
      });
    return () => {
      live = false;
    };
  }, [attempt]);

  const toggle = (board: string): void => {
    setExpanded((previous) =>
      previous.includes(board)
        ? previous.filter((name) => name !== board)
        : [...previous, board],
    );
  };

  const boards = report?.boards ?? [];

  return (
    <section className="teams" aria-label="team boards">
      <div className="teams-head">
        <h2 className="teams-title">team boards</h2>
        <button
          type="button"
          className="button teams-retry"
          onClick={() => {
            setAttempt((previous) => previous + 1);
          }}
          disabled={reading}
        >
          {reading && report === null ? 'reading…' : 'read again'}
        </button>
      </div>
      <p className="muted teams-note">
        boards on this machine, from <code>hearth team run</code>; newest first, as the server
        ordered them.
      </p>

      {reading && <p className="muted teams-note team-reading">reading the team boards…</p>}

      {error !== null && (
        <p className="banner banner-error" role="alert">
          the team boards could not be read: <code>{error}</code>. no board is shown, so this
          says nothing about whether a team ran. the CLI can still answer:{' '}
          <code>hearth team list</code>.
        </p>
      )}

      {!reading && error === null && boards.length === 0 && (
        <p className="empty teams-empty">
          no team has run in this data directory, so there are no boards to show. that is not a
          failure: the route does not create the directory, and <code>hearth team list</code>{' '}
          reads the same answer.
        </p>
      )}

      {boards.length > 0 && (
        <ul className="teams-list">
          {boards.map((board) => (
            <BoardItem
              key={board.board}
              board={board}
              open={expanded.includes(board.board)}
              onToggle={toggle}
            />
          ))}
        </ul>
      )}
    </section>
  );
}