// What the agent changed, grouped by turn.
//
// The job of this pane is the question an operator actually asks — "what did it
// do just now, and where do I look?" — so the newest turn is at the top, the
// files of that turn are listed under it, and every row opens the file at the
// first line it touched.
//
// It shows the log, not a guess: turns exist only where a write reported line
// information. A turn that changed files through the shell tool is absent here,
// and the note at the top says so rather than pretending the turn was clean.

import type { TurnChangeFile, TurnChangeTurn } from '../../state/turnChanges';
import { rangeLabel } from '../../state/turnChanges';

export interface TurnChangesProps {
  /** Oldest first, as the log is read; the pane draws it newest first. */
  turns: readonly TurnChangeTurn[];
  onOpen: (path: string, line: number) => void;
}

const MAX_CHIPS = 4;

function changedText(file: TurnChangeFile): string {
  if (file.ranges.length === 0) return `−${String(file.removed)} 行`;
  return `+${String(file.added)} −${String(file.removed)}`;
}

export function TurnChanges({ turns, onOpen }: TurnChangesProps) {
  if (turns.length === 0) {
    return (
      <div className="wb-turn-pane">
        <p className="wb-empty">
          还没有任何可跳转的改动。只有报告了行号的文件写入（`fs_write`、`fs_edit`）才会出现在这里；
          通过在终端或 shell 里跑命令改出来的文件没有行号，这一栏不会替它们编一个。
        </p>
      </div>
    );
  }
  const newestFirst = [...turns].reverse();
  return (
    <div className="wb-turn-pane">
      <p className="wb-turn-note">
        每一轮写下这些文件的哪几行。点一条打开文件并跳到那一行。只统计带行信息的写入，shell 命令改的文件不在其中。
      </p>
      {newestFirst.map((turn) => (
        <section key={turn.turn} className="wb-turn" aria-label={`第 ${String(turn.turn)} 轮`}>
          <header className="wb-turn-head">
            <span className="wb-turn-number">第 {turn.turn} 轮</span>
            <span className="wb-turn-count">
              {turn.files.length} 个文件 · +{turn.added} −{turn.removed}
            </span>
          </header>
          <ul className="wb-turn-files">
            {turn.files.map((file) => (
              <li key={file.path}>
                <button
                  type="button"
                  className="wb-turn-file"
                  title={`打开 ${file.path}`}
                  onClick={() => {
                    onOpen(file.path, file.ranges[0]?.from ?? 1);
                  }}
                >
                  <span className="mono wb-turn-path">{file.path}</span>
                  <span className="wb-turn-change">{changedText(file)}</span>
                  {file.writes > 1 && <span className="wb-turn-writes">写入 {file.writes} 次</span>}
                </button>
                {file.ranges.length > 0 && (
                  <div className="wb-turn-ranges">
                    {file.ranges.slice(0, MAX_CHIPS).map((range) => (
                      <button
                        key={`${String(range.from)}-${String(range.to)}`}
                        type="button"
                        className="wb-turn-range"
                        title={`跳到 ${file.path} ${rangeLabel(range)}`}
                        onClick={() => {
                          onOpen(file.path, range.from);
                        }}
                      >
                        {rangeLabel(range)}
                      </button>
                    ))}
                    {file.ranges.length > MAX_CHIPS && (
                      <span className="wb-turn-more">还有 {file.ranges.length - MAX_CHIPS} 处</span>
                    )}
                    {file.coarse && <span className="wb-turn-more">文件太大，只给了一段大致范围</span>}
                    {file.cut && <span className="wb-turn-more">范围被截断，标记不完整</span>}
                  </div>
                )}
              </li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}
