// The diff list: the centre pane's second mode, switched on from the toolbar.
//
// The contract makes `files: []` mean two different things — not a git
// repository, or a clean tree — and it does not say which. So the empty state
// names both possibilities instead of picking one: a sentence that claims
// "no changes" when there is no repository at all would be a lie the reader
// cannot check.
//
// A row is two targets, not one. Clicking the path opens the file — "I want to
// edit this" — and clicking the caret expands the lines below it, in place, so a
// reader can judge a change without losing the list. Both gestures are on the
// row because a reviewer who has to leave the list to see a hunk stops reviewing
// the list.
import { useState } from 'react';
import type { ReactElement } from 'react';
import type { DiffEntry, FileDiff } from '../../api/workbench';
import type { FileDiffState } from '../../state/workbench';
import { sideBySide } from '../../state/diff';

export interface DiffListProps {
  files: readonly DiffEntry[] | null;
  loading: boolean;
  error: string | null;
  /** The row whose lines are showing, if any. */
  fileDiff: FileDiffState | null;
  onOpenFile: (path: string) => void;
  onRetry: () => void;
  onToggleLines: (path: string) => void;
  onHideLines: () => void;
  onRetryLines: (path: string) => void;
  /**
   * Put this file back to its committed bytes.
   *
   * It lives inside the expanded rows rather than on the row itself: a button
   * that writes to the disk must not sit next to the button that opens a file
   * for editing, or the two get clicked by the same gesture at the same speed.
   */
  onRevert: (path: string) => void;
  /** The row whose restore is in flight, if any. */
  revertingPath: string | null;
  /** Why the last restore was refused, shown under the row it was about. */
  revertError: string | null;
}

/**
 * Colour one line of a unified diff by its first character.
 *
 * The text is git's own and is not parsed into hunks: the first character is the
 * only part of the format this renderer needs, and everything else — line
 * numbers, hunk headers, rename metadata — is shown as git wrote it. Rebuilding
 * the format would mean owning its awkward cases instead of reading them.
 */
function lineClass(line: string): string {
  if (line.startsWith('+++') || line.startsWith('---')) return 'wb-diff-line wb-diff-meta';
  if (line.startsWith('@@')) return 'wb-diff-line wb-diff-hunk';
  if (line.startsWith('+')) return 'wb-diff-line wb-diff-add';
  if (line.startsWith('-')) return 'wb-diff-line wb-diff-del';
  if (line.startsWith('diff ') || line.startsWith('index ') || line.startsWith('Binary ')) {
    return 'wb-diff-line wb-diff-meta';
  }
  return 'wb-diff-line';
}

function DiffText({ body }: { body: FileDiff }): ReactElement {
  // A trailing newline is not an empty last line; splitting without this drops
  // nothing but would add one.
  const lines = body.text === '' ? [] : body.text.replace(/\n$/, '').split('\n');
  if (lines.length === 0) {
    return (
      <p className="wb-empty">
        {body.status === 'unchanged'
          ? '这个文件与提交的版本逐字节相同，没有可看的行。'
          : '服务端对这个文件返回了空的改动。'}
      </p>
    );
  }
  return (
    <div className="wb-diff-body">
      <pre className="wb-diff-text">
        {lines.map((line, index) => (
          // The index is the key: two identical lines are normal in a diff and a
          // content key would collapse them into one row.
          <span key={index} className={lineClass(line)}>
            {line === '' ? '\u00a0' : line}
            {'\n'}
          </span>
        ))}
      </pre>
      {body.truncated && (
        <p className="wb-muted wb-diff-note">服务端截断了这份改动；这里只显示前 1 MiB。</p>
      )}
    </div>
  );
}

/**
 * 同一段改动摆成左右两列。
 *
 * 对齐由 `state/diff.ts` 决定，这里只负责画：左列是磁盘上现在的样子，右列是
 * 提交里的样子。留白的格子是真的没有对应行——一行被拆成两行时，并排视图本来
 * 就没有正确答案，假装有比承认没有更糟。
 */
function SideText({ body }: { body: FileDiff }): ReactElement {
  const rows = sideBySide(body.text);
  if (rows.length === 0) {
    return (
      <p className="wb-empty">
        {body.status === 'unchanged'
          ? '这个文件与提交的版本逐字节相同，没有可看的行。'
          : '服务端对这个文件返回了空的改动。'}
      </p>
    );
  }
  return (
    <div className="wb-diff-body">
      <table className="wb-side">
        <tbody>
          {rows.map((row, index) => {
            if (row.right === null) {
              return (
                <tr key={index} className="wb-side-wide">
                  <td colSpan={4} className={`wb-side-${row.left.kind}`}>
                    {row.left.text === '' ? '\u00a0' : row.left.text}
                  </td>
                </tr>
              );
            }
            return (
              <tr key={index}>
                <td className="wb-side-no">{row.left.no ?? ''}</td>
                <td className={`wb-side-${row.left.kind}`}>
                  {row.left.kind === 'blank' ? '\u00a0' : row.left.text === '' ? '\u00a0' : row.left.text}
                </td>
                <td className="wb-side-no">{row.right.no ?? ''}</td>
                <td className={`wb-side-${row.right.kind}`}>
                  {row.right.kind === 'blank' ? '\u00a0' : row.right.text === '' ? '\u00a0' : row.right.text}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {body.truncated && (
        <p className="wb-muted wb-diff-note">服务端截断了这份改动；这里只显示前 1 MiB。</p>
      )}
    </div>
  );
}

export function DiffList({
  files,
  loading,
  error,
  fileDiff,
  onOpenFile,
  onRetry,
  onToggleLines,
  onHideLines,
  onRetryLines,
  onRevert,
  revertingPath,
  revertError,
}: DiffListProps) {
  // 视图形态是**读者**的偏好，不是会话的状态：它不该进事件日志，也不该在刷新后
  // 假装自己记得。放在组件里，切换一次文件树重建就回到统一格式，这是刻意的。
  const [side, setSide] = useState(false);
  return (
    <div className="wb-diff">
      <div className="wb-diff-head">
        <span>工作区</span>
        {files !== null && <span className="wb-muted">{files.length} 个文件</span>}
      </div>
      {loading && <p className="wb-empty">正在读取改动…</p>}
      {error !== null && (
        <div className="wb-empty wb-error">
          <p>{error}</p>
          <button type="button" className="button button-ghost" onClick={onRetry}>
            重试
          </button>
        </div>
      )}
      {!loading && error === null && files === null && (
        <p className="wb-empty">还没有读取改动。</p>
      )}
      {!loading && error === null && files !== null && files.length === 0 && (
        <p className="wb-empty">
          该会话报告没有改动的文件。这个答案意味着：要么工作区不是 git
          仓库，要么它的工作树是干净的 — 该端点不区分这两种情况，所以这个列表也不区分。
        </p>
      )}
      {!loading && error === null && files !== null && files.length > 0 && (
        <ul className="wb-diff-list">
          {files.map((entry) => {
            const expanded = fileDiff !== null && fileDiff.path === entry.path;
            return (
              <li key={entry.path} className={expanded ? 'wb-diff-item wb-diff-item-open' : 'wb-diff-item'}>
                <div className="wb-diff-row-line">
                  <button
                    type="button"
                    className="wb-diff-toggle"
                    aria-expanded={expanded}
                    aria-label={expanded ? `收起 ${entry.path} 的改动行` : `展开 ${entry.path} 的改动行`}
                    title={expanded ? '收起改动行' : '展开改动行'}
                    onClick={() => {
                      onToggleLines(entry.path);
                    }}
                  >
                    {expanded ? '▾' : '▸'}
                  </button>
                  <button
                    type="button"
                    className="wb-diff-row"
                    title={`${entry.path} — 点击打开这个文件`}
                    onClick={() => {
                      onOpenFile(entry.path);
                    }}
                  >
                    <span className="wb-diff-status">{entry.status}</span>
                    <span className="wb-diff-path">{entry.path}</span>
                    <span className="wb-diff-counts">
                      <span className="wb-good">+{entry.additions}</span>{' '}
                      <span className="wb-bad">-{entry.deletions}</span>
                    </span>
                  </button>
                </div>
                {expanded && fileDiff !== null && (
                  <div className="wb-diff-expand">
                    {fileDiff.loading && <p className="wb-empty">正在读取 {entry.path} 的改动行…</p>}
                    {!fileDiff.loading && fileDiff.error !== null && (
                      <div className="wb-empty wb-error">
                        <p>{fileDiff.error}</p>
                        <button
                          type="button"
                          className="button button-ghost"
                          onClick={() => {
                            onRetryLines(entry.path);
                          }}
                        >
                          重试
                        </button>
                        <button type="button" className="button button-ghost" onClick={onHideLines}>
                          收起
                        </button>
                      </div>
                    )}
                    {!fileDiff.loading && fileDiff.error === null && fileDiff.body !== null && (
                      <>
                        {side ? <SideText body={fileDiff.body} /> : <DiffText body={fileDiff.body} />}
                        <div className="wb-diff-actions">
                          <button
                            type="button"
                            className="button button-ghost"
                            aria-pressed={side}
                            title="把这段改动摆成左右两列（左是磁盘，右是提交）"
                            onClick={() => {
                              setSide((on) => !on);
                            }}
                          >
                            {side ? '统一格式' : '并排显示'}
                          </button>
                          <button
                            type="button"
                            className="button button-ghost"
                            disabled={revertingPath === entry.path}
                            title="把这一行改动的文件恢复到上次提交的版本"
                            onClick={() => {
                              onRevert(entry.path);
                            }}
                          >
                            {revertingPath === entry.path ? '正在还原…' : '还原到上次提交'}
                          </button>
                          {entry.status === 'untracked' && (
                            <span className="wb-muted">
                              这个文件还没有被提交过，还原会删掉它，所以服务端会拒绝。
                            </span>
                          )}
                        </div>
                        {revertError !== null && revertingPath === null && (
                          <p className="wb-error wb-diff-note">{revertError}</p>
                        )}
                      </>
                    )}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}