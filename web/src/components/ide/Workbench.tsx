// The three-column workbench: files on the left, one file in the middle, the
// agent on the right.
//
// This component owns layout and nothing else. Every read goes through
// state/workbench.ts, every piece of chat markup arrives already rendered as
// `agent` (from SessionView, which owns the session store), and the editor is a
// placeholder behind EditorPane. That split is what lets the same session
// screen degrade to one column below 900px without duplicating the transcript.
//
// The width switch lives here, in `useWideLayout`, because the decision is the
// layout's: in a browser without matchMedia (jsdom, an old embedded view) it
// answers false, and the caller keeps the plain single-column screen it always
// had.
import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { KeyboardEvent, PointerEvent, ReactNode } from 'react';
import { WorkbenchStore } from '../../state/workbench';
import type { WorkbenchApi } from '../../state/workbench';
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
            title={treeOpen ? 'hide the file tree' : 'show the file tree'}
            onClick={() => {
              setTreeOpen((open) => !open);
            }}
          >
            {treeOpen ? '◀ files' : '▶ files'}
          </button>
          <button
            type="button"
            className={state.showDiff ? 'button button-primary' : 'button button-ghost'}
            aria-pressed={state.showDiff}
            title="show the working tree instead of the editor"
            onClick={() => {
              store.toggleDiff();
            }}
          >
            diff
          </button>
          <span className="wb-link-state" title="the session event stream">
            {connecting ? 'connecting' : 'live'}
          </span>
          {needsResync && <span className="wb-link-state wb-warn">resyncing</span>}
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

        <section className="wb-pane wb-centre" aria-label="editor">
          {state.showDiff ? (
            <DiffList
              files={state.diff}
              loading={state.diffLoading}
              error={state.diffError}
              onOpenFile={(path) => {
                store.openFromDiff(path);
              }}
              onRetry={() => {
                store.refreshDiff();
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
                {tab === null && (
                  <p className="wb-empty">
                    no file is open. Pick one in the files pane: the centre column shows a single
                    file at a time, read-only for now. The diff button above swaps it for the list
                    of changed files.
                  </p>
                )}
                {tab !== null && tab.loading && <p className="wb-empty">reading {tab.path}…</p>}
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
                      try again
                    </button>
                  </div>
                )}
                {tab !== null && tab.file !== null && (
                  <EditorPane
                    path={tab.file.path}
                    text={tab.file.text}
                    readOnly
                    onChange={() => {
                      // Read-only for this pass: Monaco arrives behind the same
                      // props, and until then a discarded edit would be a lie.
                    }}
                    notice={
                      tab.file.binary
                        ? 'this file is not text — the server sent no characters for it'
                        : tab.file.truncated
                          ? 'the server truncated this file; only its first 1 MiB is shown'
                          : null
                    }
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
          aria-label="resize the agent pane"
          tabIndex={0}
          onPointerDown={grab}
          onKeyDown={nudge}
        />

        <aside className="wb-pane wb-pane-agent">{agent}</aside>
      </div>
    </div>
  );
}