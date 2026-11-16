// A tool call and its result as one card.
//
// Two rules from the docs drive this component:
//   * Tool output is capped by the server at 64 KiB (AGENTS.md anti-pattern 3),
//     and the result event says so with `truncated: true`. The card must show
//     that marker: silent truncation is a correctness bug the user cannot see.
//   * Long output is collapsed to the first and last 20 lines
//     (docs/02-architecture.md §10.1.5), so one `cat` of a large file cannot
//     push everything else out of the transcript.
import { memo, useMemo, useState } from 'react';
import type { ToolRow } from '../state/rows';

export const COLLAPSED_LINES = 20;

export interface CollapsedText {
  text: string;
  collapsed: boolean;
  lines: number;
  omitted: number;
}

/** First and last `COLLAPSED_LINES` lines of a long output. */
export function collapseOutput(output: string, head = COLLAPSED_LINES, tail = COLLAPSED_LINES): CollapsedText {
  const lines = output.split('\n');
  if (lines.length <= head + tail + 1) {
    return { text: output, collapsed: false, lines: lines.length, omitted: 0 };
  }
  const omitted = lines.length - head - tail;
  const first = lines.slice(0, head);
  const last = lines.slice(lines.length - tail);
  return {
    text: `${first.join('\n')}\n… ${String(omitted)} lines hidden …\n${last.join('\n')}`,
    collapsed: true,
    lines: lines.length,
    omitted,
  };
}

function formatArgs(args: unknown): string {
  if (args === undefined) return '';
  try {
    return JSON.stringify(args, null, 2);
  } catch {
    return String(args);
  }
}

export const ToolCard = memo(function ToolCard({ row }: { row: ToolRow }) {
  const [expanded, setExpanded] = useState(false);
  const args = useMemo(() => formatArgs(row.args), [row.args]);
  const output = useMemo(() => (row.output === undefined ? null : collapseOutput(row.output)), [row.output]);
  const failed = row.ok === false || row.errorCode !== undefined;

  return (
    <article className={`card tool-card${failed ? ' tool-card-failed' : ''}`}>
      <header className="tool-card-head">
        <span className="tool-name">{row.tool}</span>
        <span className="tool-call-id" title={row.callId}>
          {row.callId === '' ? 'call' : row.callId}
        </span>
        <span className={`pill ${row.done ? (failed ? 'pill-bad' : 'pill-good') : 'pill-muted'}`}>
          {row.done ? (failed ? 'failed' : 'ok') : 'running'}
        </span>
        {row.errorCode !== undefined && (
          <span className="error-code" title="tool error code">
            {row.errorCode}
          </span>
        )}
      </header>

      {args !== '' && (
        <details className="tool-section">
          <summary>args</summary>
          <pre className="mono tool-args">{args}</pre>
        </details>
      )}

      {output !== null && (
        <div className="tool-section">
          <div className="tool-output-head">
            <span>output</span>
            {row.truncated && (
              <span className="truncated-marker" title="the server capped this output at 64 KiB">
                truncated at 64 KiB
              </span>
            )}
            {output.collapsed && (
              <button type="button" className="link-button" onClick={() => setExpanded((v) => !v)}>
                {expanded ? 'collapse' : `show all ${String(output.lines)} lines`}
              </button>
            )}
          </div>
          <pre className="mono tool-output">
            {output.collapsed && expanded ? (row.output ?? '') : output.text}
          </pre>
        </div>
      )}

      {row.meta !== undefined && (
        <details className="tool-section">
          <summary>meta</summary>
          <pre className="mono tool-args">{formatArgs(row.meta)}</pre>
        </details>
      )}
    </article>
  );
});