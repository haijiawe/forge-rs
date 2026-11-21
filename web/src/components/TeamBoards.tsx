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
    label: '已完成',
    hint: '对话记录里有 worker/end 行，且其 ok 标志为 true',
  },
  failed: {
    label: '失败',
    hint: '对话记录里有 worker/end 行，且其 ok 标志为 false',
  },
  unknown: {
    label: '未知',
    hint: '没有 worker/end 行：本构建无法判断它是否已完成',
  },
};

/** A class-name-safe slug for a status this build may never have seen. */
function statusClass(status: string): string {
  const slug = status.replace(/[^A-Za-z0-9_-]+/g, '-').replace(/^-+|-+$/g, '');
  return slug === '' ? 'none' : slug.toLowerCase();
}

function statusLabel(status: string): string {
  return status === '' ? '状态未记录' : status;
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
        这块任务板上没有任务 — 其 <code>board.json</code> 没有列出任何任务，或读取失败；
        诊断信息会说明是哪一种。
      </p>
    );
  }
  return (
    <p className="team-board-tasks">
      任务 <span className="team-board-total">{String(total)}</span>
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
      工作者 <span className="team-board-total">{String(workers.length)}</span>
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
    return <p className="muted team-board-note">没有可显示的任务行</p>;
  }
  return (
    <table className="team-board-table">
      <thead>
        <tr>
          <th scope="col">标识</th>
          <th scope="col">任务</th>
          <th scope="col">状态</th>
          <th scope="col">负责人</th>
        </tr>
      </thead>
      <tbody>
        {tasks.map((task, index) => (
          <tr key={`${task.id}:${String(index)}`}>
            <td className="mono">{task.id === '' ? '—' : task.id}</td>
            <td>{task.subject === '' ? <span className="muted">无标题</span> : task.subject}</td>
            <td
              className={`team-task-status team-task-status-${statusClass(task.status)}`}
              title={statusLabel(task.status)}
            >
              {statusLabel(task.status)}
            </td>
            <td className="team-task-owner">
              {task.owner === null ? (
                <span className="muted">未分配</span>
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
    return <p className="muted team-board-note">没有可显示的工作者记录</p>;
  }
  return (
    <table className="team-board-table">
      <thead>
        <tr>
          <th scope="col">工作者</th>
          <th scope="col">状态</th>
          <th scope="col">事件数</th>
          <th scope="col">备注</th>
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
                  <span className="muted">未报告</span>
                ) : (
                  String(worker.events)
                )}
              </td>
              <td className="team-worker-note">
                {worker.note === '' ? (
                  <span className="muted">
                    {state === 'unknown' ? '未写入 worker/end 行' : '无备注'}
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
      <span className="team-board-parallel team-board-parallel-unknown" title="目前还没有地方持久化并行度">
        未知
      </span>
    ) : (
      <span className="team-board-parallel mono">{String(board.parallel)}</span>
    );

  return (
    <li className="team-board">
      <div className="team-board-summary">
        <h3 className="team-board-title">
          <span className="mono team-board-team">
            {board.team === '' ? '团队未记录' : board.team}
          </span>{' '}
          <span className="muted team-board-id">{board.board}</span>
        </h3>
        <p className="team-board-meta">
          运行于{' '}
          {board.createdAt === null ? (
            <span className="muted team-board-when">时间未知</span>
          ) : (
            // The server stamps UTC and says so; the raw string is shown rather
            // than a localised one, because a board's clock is the server's.
            <time className="mono team-board-when" dateTime={board.createdAt}>
              {board.createdAt}
            </time>
          )}
          <span className="muted"> · </span>
          并行度 {parallel}
        </p>
        <TaskCounts counts={counts} total={board.tasks.length} />
        <StateCounts workers={board.workers} />
        {board.diagnostics.length > 0 && (
          <p className="team-board-diagnostic-badge">
            <span className="pill pill-warn" title="这些字符串是给人看的；面板只显示它们，并不解析它们">
              {plural(board.diagnostics.length, '条诊断', '条诊断')}
            </span>
          </p>
        )}
        <button
          type="button"
          className="button team-board-toggle"
          aria-expanded={open}
          aria-controls={detailId}
          aria-label={`${open ? '隐藏' : '显示'} ${board.board} 的详情`}
          onClick={() => {
            onToggle(board.board);
          }}
        >
          {open ? '隐藏详情' : '显示详情'}
        </button>
      </div>
      {open && (
        <div className="team-board-detail" id={detailId}>
          {board.diagnostics.length > 0 && (
            <>
              <h4 className="team-board-detail-head">诊断</h4>
              <ul className="team-board-diagnostics">
                {board.diagnostics.map((line, index) => (
                  <li key={`${String(index)}:${line}`}>{line}</li>
                ))}
              </ul>
            </>
          )}
          <h4 className="team-board-detail-head">任务</h4>
          <TaskTable tasks={board.tasks} />
          <h4 className="team-board-detail-head">工作者</h4>
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
    <section className="teams" aria-label="团队任务板">
      <div className="teams-head">
        <h2 className="teams-title">团队任务板</h2>
        <button
          type="button"
          className="button teams-retry"
          onClick={() => {
            setAttempt((previous) => previous + 1);
          }}
          disabled={reading}
        >
          {reading && report === null ? '正在读取…' : '重新读取'}
        </button>
      </div>
      <p className="muted teams-note">
        本机上的任务板，来自 <code>hearth team run</code>；按服务端排列的顺序，最新的在前。
      </p>

      {reading && <p className="muted teams-note team-reading">正在读取团队任务板…</p>}

      {error !== null && (
        <p className="banner banner-error" role="alert">
          无法读取团队任务板：<code>{error}</code>。这里没有显示任何任务板，因此不能据此判断
          是否有团队运行过。命令行仍可回答：{' '}
          <code>hearth team list</code>。
        </p>
      )}

      {!reading && error === null && boards.length === 0 && (
        <p className="empty teams-empty">
          这个数据目录里还没有团队运行过，所以没有任务板可显示。这不是故障：该路由不会创建
          这个目录，而 <code>hearth team list</code>{' '}
          读到的也是同一个答案。
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