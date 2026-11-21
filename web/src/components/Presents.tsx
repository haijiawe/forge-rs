// The handoff pane: the files the model gave the operator with `present`.
//
// A handoff is a pointer, not a transfer -- the tool copies nothing, puts
// nothing in the conversation, and appends one line to
// `<dataDir>/presented.jsonl`. This pane is the reader that line never had.
//
// What it refuses to pretend:
//
//   * It cannot open the file. The browser has no access to the operator's disk,
//     so the path is shown as selectable text with the CLI command that opens
//     it, not as a button that would do nothing.
//   * It cannot tell whether the file still exists. The manifest records the
//     size the file had when it was presented, and the pane labels it that way.
//   * It does not hide the fact that the list is partial. `skipped` and
//     `truncated` are shown as their own sentence, because a short list that
//     looks complete is worse than a short list that says it is short.
import { useEffect, useRef, useState } from 'react';
import {
  baseName,
  formatBytes,
  formatWhen,
  parseReport,
  type Present,
  type PresentReport,
} from '../api/presents';

export interface PresentsProps {
  /**
   * Reads `GET /api/presents` from the client. The component owns the request
   * so the failure state is always the contract's: the error code, plus the
   * command that still answers. May resolve to the report or a raw body --
   * `parseReport` accepts either.
   *
   * Pass a stable function (`useCallback`) or a module-level one; a fresh arrow
   * each render is tolerated (it is held in a ref) but will not re-read.
   */
  load: () => Promise<unknown>;
}

function describeError(cause: unknown): string {
  return cause instanceof Error ? cause.message : '无法读取响应';
}

function plural(count: number, one: string, many: string): string {
  return `${String(count)} ${count === 1 ? one : many}`;
}

function HandoffRow({ handoff }: { handoff: Present }) {
  return (
    <li className="present-row">
      <div className="present-head">
        <span className="mono present-name" title={handoff.path}>
          {baseName(handoff.path)}
        </span>
        <span className="muted present-size" title="这个文件在交付时的大小">
          {formatBytes(handoff.size)}
        </span>
        <time className="muted mono present-when" dateTime={handoff.ts > 0 ? new Date(handoff.ts).toISOString() : undefined}>
          {formatWhen(handoff.ts)}
        </time>
      </div>
      {handoff.description !== '' && <p className="present-description">{handoff.description}</p>}
      {/* Selectable rather than a link: a `file:` link is blocked by the browser
          and a `#` link would be a lie about doing something. */}
      <code className="present-path" tabIndex={0}>
        {handoff.path}
      </code>
    </li>
  );
}

export function Presents({ load }: PresentsProps) {
  const [report, setReport] = useState<PresentReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reading, setReading] = useState(true);
  const [attempt, setAttempt] = useState(0);

  // Held in a ref so a parent passing a fresh arrow each render does not become
  // a fetch loop; the effect re-runs only on an explicit retry.
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
        setReport(parseReport(value));
      })
      .catch((cause: unknown) => {
        if (!live) return;
        // Cleared, not kept: stale rows beside an error read as "these are
        // current, and that one failed".
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

  const handoffs = report?.presents ?? [];

  return (
    <section className="presents" aria-label="已交付文件">
      <div className="presents-head">
        <h2 className="presents-title">已交付文件</h2>
        <button
          type="button"
          className="button presents-retry"
          onClick={() => {
            setAttempt((previous) => previous + 1);
          }}
          disabled={reading}
        >
          {reading && report === null ? '正在读取…' : '重新读取'}
        </button>
      </div>
      <p className="muted presents-note">
        模型用 <code>present</code> 交接的文件，最新的在前。交接只是一个指针：
        文件在磁盘上，并不在这个页面里。
      </p>

      {reading && <p className="muted presents-note presents-reading">正在读取清单…</p>}

      {error !== null && (
        <p className="banner banner-error" role="alert">
          无法读取清单：<code>{error}</code>。这里没有显示任何内容，因此不能说明是否有文件被
          交付过；文件本身不受影响。
        </p>
      )}

      {!reading && error === null && handoffs.length === 0 && (
        <p className="empty presents-empty">
          这个数据目录里还没有交付过任何文件。这不是故障：该路由不会创建清单，
          而某个回合交接文件时会写下第一行。
        </p>
      )}

      {handoffs.length > 0 && report !== null && (
        <>
          <ul className="presents-list">
            {handoffs.map((handoff) => (
              <HandoffRow key={`${String(handoff.ts)}:${handoff.path}`} handoff={handoff} />
            ))}
          </ul>
          {(report.skipped > 0 || report.truncated || report.note !== null) && (
            <p className="muted presents-partial">
              {report.skipped > 0 && <>{plural(report.skipped, '行', '行')}无法读取。</>}
              {report.truncated && <>列表被截断了，清单里还有更多内容。</>}
              {report.note !== null && <>{report.note} </>}
              上面这些文件名仍然是模型交接过来的内容。
            </p>
          )}
        </>
      )}
    </section>
  );
}