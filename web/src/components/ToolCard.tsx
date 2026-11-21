// A tool call and its result as one card.
//
// Two rules from the docs drive this component:
//   * Tool output is capped by the server at 64 KiB (AGENTS.md anti-pattern 3),
//     and the result event says so with `truncated: true`. The card must show
//     that marker: silent truncation is a correctness bug the user cannot see.
//   * Long output is collapsed to the first and last 20 lines
//     (docs/02-architecture.md §10.1.5), so one `cat` of a large file cannot
//     push everything else out of the transcript.
//
// The card opens closed, as one line: a tool call is an action, and a transcript
// with forty of them is a log to scan, not forty forms to read. Clicking the line
// opens the body. A failure opens itself — an error nobody clicks is an error
// nobody sees, and that is the one case where the compact form would actively
// hide something.
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
    text: `${first.join('\n')}\n… 已隐藏 ${String(omitted)} 行 …\n${last.join('\n')}`,
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

/**
 * 一行说清这次调用在动什么。
 *
 * 只写工具名（`shell`、`fs_write`）等于没写：四十个 `shell` 排在一起，读者还是
 * 得逐个展开才知道哪个是 `cargo test`。所以取参数里第一个字符串值当前缀，它几乎
 * 总是那条命令或那个路径。取不到就返回空串，卡片照样能认（工具名 + 状态仍在）。
 */
export function argHint(args: unknown): string {
  if (args === null || typeof args !== 'object') return '';
  const first = Object.values(args as Record<string, unknown>).find(
    (value): value is string => typeof value === 'string' && value !== '',
  );
  if (first === undefined) return '';
  const flat = first.replace(/\s+/g, ' ').trim();
  return flat.length > 60 ? `${flat.slice(0, 60)}…` : flat;
}

export const ToolCard = memo(function ToolCard({ row }: { row: ToolRow }) {
  // null 表示「还没被读者动过」：默认状态跟着失败走（失败自己展开），但读者点过
  // 一次之后就以读者的选择为准——包括把一个失败的卡片收起来。
  const [chosen, setChosen] = useState<boolean | null>(null);
  const [expanded, setExpanded] = useState(false);
  const args = useMemo(() => formatArgs(row.args), [row.args]);
  const hint = useMemo(() => argHint(row.args), [row.args]);
  const output = useMemo(() => (row.output === undefined ? null : collapseOutput(row.output)), [row.output]);
  const failed = row.ok === false || row.errorCode !== undefined;
  const open = chosen ?? failed;

  return (
    <article className={`card tool-card${failed ? ' tool-card-failed' : ''}${open ? ' tool-card-open' : ''}`}>
      <header className="tool-card-head">
        <button
          type="button"
          className="tool-card-toggle"
          aria-expanded={open}
          title={open ? '收起这次调用' : '展开这次调用的参数与输出'}
          onClick={() => {
            setChosen(!open);
          }}
        >
          <span className="tool-caret" aria-hidden="true">{open ? '▾' : '▸'}</span>
          <span className="tool-name">{row.tool}</span>
          {hint !== '' && <span className="tool-hint" title={hint}>{hint}</span>}
          <span className="tool-call-id" title={row.callId}>
            {row.callId === '' ? '调用' : row.callId}
          </span>
          <span className={`pill ${row.done ? (failed ? 'pill-bad' : 'pill-good') : 'pill-muted'}`}>
            {row.done ? (failed ? '失败' : '成功') : '运行中'}
          </span>
          {row.errorCode !== undefined && (
            <span className="error-code" title="工具错误码">
              {row.errorCode}
            </span>
          )}
          {row.truncated && (
            <span className="truncated-marker" title="服务端已把这段输出截断到 64 KiB">
              已在 64 KiB 处截断
            </span>
          )}
        </button>
      </header>

      {open && (
        <div className="tool-card-body">
          {args !== '' && (
            <details className="tool-section">
              <summary>参数</summary>
              <pre className="mono tool-args">{args}</pre>
            </details>
          )}

          {output !== null && (
            <div className="tool-section">
              <div className="tool-output-head">
                <span>输出</span>
                {output.collapsed && (
                  <button type="button" className="link-button" onClick={() => setExpanded((v) => !v)}>
                    {expanded ? '收起' : `显示全部 ${String(output.lines)} 行`}
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
              <summary>元信息</summary>
              <pre className="mono tool-args">{formatArgs(row.meta)}</pre>
            </details>
          )}
        </div>
      )}
    </article>
  );
});
