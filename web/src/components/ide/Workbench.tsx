// The three-column workbench: files on the left, one file in the middle, the
// agent on the right.
//
// This component owns layout and nothing else. Every read goes through
// state/workbench.ts, every piece of chat markup arrives already rendered as
// `agent` (from SessionView, which owns the session store), and the editor is
// EditorPane's business — Monaco when the environment can host it, a textarea
// when it cannot. That split is what lets the same session screen degrade to one
// column below 900px without duplicating the transcript.
//
// The width switch lives here, in `useWideLayout`, because the decision is the
// layout's: in a browser without matchMedia (jsdom, an old embedded view) it
// answers false, and the caller keeps the plain single-column screen it always
// had.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { KeyboardEvent, PointerEvent, ReactNode } from 'react';
import { WorkbenchStore } from '../../state/workbench';
import type { FileTab, WorkbenchApi } from '../../state/workbench';
import { DiffList } from './DiffList';
import { EditorPane } from './EditorPane';
import { FileTree } from './FileTree';
import { Tabs } from './Tabs';

/** Below this the three columns stop being usable; SessionView falls back. */
export const WIDE_QUERY = '(min-width: 900px)';

const TREE_WIDTH = 240;
const GRIP_WIDTH = 6;
const MIN_RIGHT = 320;
/** The centre column's floor, used when the window is small but still wide. */
const MIN_CENTRE = 320;

function mediaMatches(query: string): boolean {
  if (typeof window === 'undefined') return false;
  const match = window.matchMedia;
  if (typeof match !== 'function') return false;
  return match.call(window, query).matches;
}

/**
 * True when the viewport is at least `query`. False — not true, not throwing —
 * when the environment has no matchMedia, so a test host keeps the narrow
 * layout instead of silently getting an untested wide one.
 */
export function useWideLayout(query: string = WIDE_QUERY): boolean {
  const [wide, setWide] = useState(() => mediaMatches(query));
  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    const list = window.matchMedia(query);
    setWide(list.matches);
    const sync = (): void => {
      setWide(list.matches);
    };
    list.addEventListener('change', sync);
    return () => {
      list.removeEventListener('change', sync);
    };
  }, [query]);
  return wide;
}

/**
 * The one sentence above the text, in priority order.
 *
 * A binary file and a truncated read both take the editor out of the writing
 * business, and a save failure is the thing the operator has to act on — so it
 * comes first. All three can be true at once; showing one honestly beats
 * showing three lines nobody reads.
 */
function editorNotice(tab: FileTab): string | null {
  if (tab.saveError !== null) {
    // `conflict` is the one the operator can fix, and the fix is not obvious
    // from the word: it means the file moved under the buffer.
    return tab.saveError.startsWith('conflict:')
      ? `${tab.saveError} — 这个标签页读取之后，文件在磁盘上被改动过。关闭该标签页再重新打开，就能看到当前的内容。`
      : tab.saveError;
  }
  if (tab.file === null) return null;
  if (tab.file.binary) return '这个文件不是文本 — 服务端没有为它返回任何字符';
  if (tab.file.truncated) {
    return '服务端截断了这个文件；这里只显示前 1 MiB，保存前必须先把文件重新读取一遍';
  }
  if (tab.saving) return '正在保存…';
  if (tab.draft !== null) return '有未保存的改动 — Ctrl+S 保存（macOS 上是 Cmd+S）';
  return null;
}

export interface WorkbenchProps {
  sessionId: string;
  /** The session header, built by SessionView so both layouts share one node. */
  head: ReactNode;
  /** Transcript, banners, approval, composer, status bar — in that order. */
  agent: ReactNode;
  connecting: boolean;
  needsResync: boolean;
  /** Injectable for tests; production leaves it out. */
  api?: WorkbenchApi;
}

export function Workbench({ sessionId, head, agent, connecting, needsResync, api }: WorkbenchProps) {
  const store = useMemo(() => {
    const options = api === undefined ? { sessionId } : { sessionId, api };
    return new WorkbenchStore(options);
  }, [sessionId, api]);

  useEffect(() => {
    store.start();
  }, [store]);

  const state = useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);

  const [treeOpen, setTreeOpen] = useState(true);
  const [rightWidth, setRightWidth] = useState(420);
  const [dragging, setDragging] = useState(false);
  // Restoring is a click, not a state of the session: which row is waiting and
  // what the server said about the last attempt belong to this render.
  const [reverting, setReverting] = useState<string | null>(null);
  const [revertError, setRevertError] = useState<string | null>(null);
  const drag = useRef<{ x: number; width: number } | null>(null);

  // Dragging is tracked on the window, not on the grip, so the pointer may leave
  // the six-pixel handle without ending the gesture.
  useEffect(() => {
    if (!dragging) return;
    const move = (event: globalThis.PointerEvent): void => {
      const start = drag.current;
      if (start === null) return;
      const ceiling = Math.max(MIN_RIGHT, window.innerWidth - TREE_WIDTH - MIN_CENTRE);
      const next = start.width + (start.x - event.clientX);
      setRightWidth(Math.min(ceiling, Math.max(MIN_RIGHT, next)));
    };
    const end = (): void => {
      drag.current = null;
      setDragging(false);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', end);
    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', end);
    };
  }, [dragging]);

  function grab(event: PointerEvent<HTMLDivElement>): void {
    drag.current = { x: event.clientX, width: rightWidth };
    setDragging(true);
  }

  // The same resize from the keyboard: a separator that only answers a mouse
  // drag is unusable with the keyboard, and this is a splitter, not a decoration.
  function nudge(event: KeyboardEvent<HTMLDivElement>): void {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    const step = event.key === 'ArrowLeft' ? 24 : -24;
    setRightWidth((width) => Math.max(MIN_RIGHT, width + step));
  }

  const tab = state.open.find((entry) => entry.path === state.activePath) ?? null;
  // How many tabs have a draft. The button only exists when the answer is not
  // zero: a permanent "save all" next to one clean tab is a button that teaches
  // the operator to ignore buttons.
  const dirtyCount = state.open.filter((entry) => entry.draft !== null).length;
  // The track count has to follow the children: a collapsed tree is not a
  // zero-width column, it is a column that is not rendered, or the grid would
  // place the centre pane in the 0px track.
  const columns = treeOpen
    ? `${String(TREE_WIDTH)}px minmax(0, 1fr) ${String(GRIP_WIDTH)}px ${String(rightWidth)}px`
    : `minmax(0, 1fr) ${String(GRIP_WIDTH)}px ${String(rightWidth)}px`;

  return (
    <div className={dragging ? 'wb-shell wb-dragging' : 'wb-shell'}>
      <div className="wb-toolbar">
        {head}
        <div className="wb-tools">
          <button
            type="button"
            className="button button-ghost"
            aria-expanded={treeOpen}
            title={treeOpen ? '隐藏文件树' : '显示文件树'}
            onClick={() => {
              setTreeOpen((open) => !open);
            }}
          >
            {treeOpen ? '◀ 文件' : '▶ 文件'}
          </button>
          <button
            type="button"
            className={state.showDiff ? 'button button-primary' : 'button button-ghost'}
            aria-pressed={state.showDiff}
            title="显示工作区改动，而不是编辑器"
            onClick={() => {
              store.toggleDiff();
            }}
          >
            改动
          </button>
          {dirtyCount > 0 && (
            <button
              type="button"
              className="button button-ghost"
              title="保存所有有改动的标签，一个接一个"
              onClick={() => {
                void store.saveAll();
              }}
            >
              保存全部（{dirtyCount}）
            </button>
          )}
          <span className="wb-link-state" title="会话事件流">
            {connecting ? '连接中' : '在线'}
          </span>
          {needsResync && <span className="wb-link-state wb-warn">重新同步中</span>}
        </div>
      </div>

      <div className="wb-body" style={{ gridTemplateColumns: columns }}>
        {treeOpen && (
          <aside className="wb-pane wb-pane-tree">
            <FileTree
              dirs={state.dirs}
              expanded={state.expanded}
              activePath={state.activePath}
              onToggleDir={(path) => {
                store.toggleDir(path);
              }}
              onOpenFile={(path) => {
                store.openFile(path);
              }}
            />
          </aside>
        )}

        <section className="wb-pane wb-centre" aria-label="编辑器">
          {state.showDiff ? (
            <DiffList
              files={state.diff}
              loading={state.diffLoading}
              error={state.diffError}
              fileDiff={state.fileDiff}
              onOpenFile={(path) => {
                store.openFromDiff(path);
              }}
              onRetry={() => {
                store.refreshDiff();
              }}
              onToggleLines={(path) => {
                store.toggleFileDiff(path);
              }}
              onHideLines={() => {
                store.hideFileDiff();
              }}
              onRetryLines={(path) => {
                store.retryFileDiff(path);
              }}
              revertingPath={reverting}
              revertError={revertError}
              onRevert={(path) => {
                // The progress and the refusal live here rather than in the
                // store: they are about one click, and the row that asked is
                // often not an open tab, so a message parked on a tab would be
                // a message nobody sees.
                setReverting(path);
                setRevertError(null);
                void store.revertToCommitted(path).then((refusal) => {
                  setReverting(null);
                  setRevertError(refusal);
                });
              }}
            />
          ) : (
            <>
              <Tabs
                open={state.open}
                activePath={state.activePath}
                onSelect={(path) => {
                  store.setActive(path);
                }}
                onClose={(path) => {
                  store.closeFile(path);
                }}
              />
              <div className="wb-editor-slot">
                {state.pendingClose !== null && (
                  <div className="wb-confirm" role="alert">
                    <p>
                      <span className="mono">{state.pendingClose}</span> 有未保存的改动。关掉就会丢掉它们，
                      磁盘上的文件保持原样，日志里也不会留下任何记录。
                    </p>
                    <div className="wb-confirm-actions">
                      <button
                        type="button"
                        className="button button-ghost"
                        onClick={() => {
                          store.cancelClose();
                        }}
                      >
                        继续编辑
                      </button>
                      <button
                        type="button"
                        className="button button-primary"
                        onClick={() => {
                          store.confirmClose();
                        }}
                      >
                        丢弃并关闭
                      </button>
                    </div>
                  </div>
                )}
                {tab === null && (
                  <p className="wb-empty">
                    还没有打开任何文件。在文件面板里挑一个：中间这一栏一次只显示一个
                    文件。改动用 Ctrl+S 保存（macOS 上是 Cmd+S），并会作为 `file/write`
                    单独记录在会话日志里，与代理做的事互不相干。上面的“改动”按钮会把编辑器
                    换成已改动文件的列表。
                  </p>
                )}
                {tab !== null && tab.loading && <p className="wb-empty">正在读取 {tab.path}…</p>}
                {tab !== null && tab.error !== null && (
                  <div className="wb-empty wb-error">
                    <p>{tab.error}</p>
                    <button
                      type="button"
                      className="button button-ghost"
                      onClick={() => {
                        store.openFile(tab.path);
                      }}
                    >
                      重试
                    </button>
                  </div>
                )}
                {tab !== null && tab.file !== null && (
                  <EditorPane
                    path={tab.file.path}
                    text={tab.draft ?? tab.file.text}
                    readOnly={tab.file.binary || tab.file.truncated}
                    onChange={(text) => {
                      store.editFile(tab.path, text);
                    }}
                    onSave={() => {
                      void store.saveFile(tab.path);
                    }}
                    notice={editorNotice(tab)}
                  />
                )}
              </div>
            </>
          )}
        </section>

        <div
          className="wb-grip"
          role="separator"
          aria-orientation="vertical"
          aria-label="调整代理面板宽度"
          tabIndex={0}
          onPointerDown={grab}
          onKeyDown={nudge}
        />

        <aside className="wb-pane wb-pane-agent">{agent}</aside>
      </div>
    </div>
  );
}