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
  return cause instanceof Error ? cause.message : 'the response could not be read';
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
        <span className="muted present-size" title="the size the file had when it was presented">
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
    <section className="presents" aria-label="presented files">
      <div className="presents-head">
        <h2 className="presents-title">presented files</h2>
        <button
          type="button"
          className="button presents-retry"
          onClick={() => {
            setAttempt((previous) => previous + 1);
          }}
          disabled={reading}
        >
          {reading && report === null ? 'reading…' : 'read again'}
        </button>
      </div>
      <p className="muted presents-note">
        files the model handed over with <code>present</code>, newest first. a handoff is a
        pointer: the file is on disk, not in this page.
      </p>

      {reading && <p className="muted presents-note presents-reading">reading the manifest…</p>}

      {error !== null && (
        <p className="banner banner-error" role="alert">
          the manifest could not be read: <code>{error}</code>. nothing is shown, so this says
          nothing about whether a file was presented; the file itself is unaffected.
        </p>
      )}

      {!reading && error === null && handoffs.length === 0 && (
        <p className="empty presents-empty">
          nothing has been presented in this data directory. that is not a failure: the route
          does not create the manifest, and a turn that hands over a file adds the first line.
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
              {report.skipped > 0 && <>{plural(report.skipped, 'line', 'lines')} could not be read. </>}
              {report.truncated && <>the list is cut short; the manifest holds more. </>}
              {report.note !== null && <>{report.note} </>}
              the file names above are still what the model handed over.
            </p>
          )}
        </>
      )}
    </section>
  );
}