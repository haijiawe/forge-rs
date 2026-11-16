// The virtualized transcript.
//
// Per docs/02-architecture.md §10.1.1 only the viewport plus one screen of
// overscan is in the DOM, whatever the transcript length. The math lives in
// src/virtual/virtualizer.ts; this component owns the three things a DOM
// virtualizer must do for it:
//
//   1. Feed the height of every rendered row back into the measurement cache
//      after commit, and keep the scroll position still when a row above the
//      viewport changes height.
//   2. Stay pinned to the bottom while the agent streams, but stop doing that
//      the moment the user scrolls up (otherwise the view fights the reader).
//   3. Restore the reading position when older events are prepended (history
//      paging): the anchor is captured before the row set changes.
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { Row } from '../state/rows';
import { Virtualizer } from '../virtual/virtualizer';
import { TranscriptRow } from './TranscriptRow';

export interface TranscriptProps {
  rows: Row[];
  rowKeys: readonly string[];
  /** Shown when the transcript is empty. */
  emptyHint?: string;
}

export function Transcript({ rows, rowKeys, emptyHint }: TranscriptProps) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const nodesRef = useRef(new Map<string, HTMLElement>());
  const virtualizerRef = useRef<Virtualizer | null>(null);
  if (virtualizerRef.current === null) {
    virtualizerRef.current = new Virtualizer({ estimateRowHeight: 48, overscanScreens: 1 });
  }
  const virtualizer = virtualizerRef.current;
  const [, forceRender] = useState(0);
  const [stuck, setStuck] = useState(true);

  // The viewport is the scroll container; a window resize changes it.
  useEffect(() => {
    const element = scrollRef.current;
    if (element === null) return;
    virtualizer.setViewport(element.clientHeight);
    // The first render computed the window against a viewport of 0, so it had
    // no rows to show; re-render once the real height is known.
    forceRender((value) => value + 1);
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(() => {
      virtualizer.setViewport(element.clientHeight);
      forceRender((value) => value + 1);
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  }, [virtualizer]);

  const onScroll = useCallback(() => {
    const element = scrollRef.current;
    if (element === null) return;
    virtualizer.setScrollTop(element.scrollTop);
    setStuck(virtualizer.isStuckToBottom());
  }, [virtualizer]);

  // Feed the row set in, restore the anchor / bottom, then measure what landed.
  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (element === null) return;
    virtualizer.setViewport(element.clientHeight);
    const wasStuck = virtualizer.isStuckToBottom();
    const anchor = virtualizer.anchor();
    const change = virtualizer.setRows(rowKeys);

    if (change.kind === 'prepend' && anchor !== null) {
      element.scrollTop = virtualizer.scrollTopForAnchor(anchor);
      virtualizer.setScrollTop(element.scrollTop);
    } else if (wasStuck) {
      element.scrollTop = virtualizer.bottomScrollTop();
      virtualizer.setScrollTop(element.scrollTop);
    } else {
      element.scrollTop = Math.min(virtualizer.top(), virtualizer.maxScrollTop());
    }

    let changed = false;
    let shift = 0;
    for (const [key, node] of nodesRef.current) {
      if (!node.isConnected) continue;
      const result = virtualizer.measure(key, node.getBoundingClientRect().height);
      changed = changed || result.changed;
      shift += result.scrollShift;
    }
    if (shift !== 0) element.scrollTop = virtualizer.top();
    setStuck(virtualizer.isStuckToBottom());
    if (changed) forceRender((value) => value + 1);
  }, [rows, rowKeys, virtualizer]);

  const range = virtualizer.range();
  const visible = rows.slice(range.start, Math.min(range.end, rows.length));

  return (
    <div className="transcript" ref={scrollRef} onScroll={onScroll}>
      <div className="transcript-inner" style={{ height: `${String(range.totalHeight)}px` }}>
        {range.offsetTop > 0 && <div className="spacer" style={{ height: `${String(range.offsetTop)}px` }} aria-hidden="true" />}
        {visible.map((row) => (
          <div
            key={row.key}
            className="row-wrap"
            ref={(node) => {
              if (node === null) nodesRef.current.delete(row.key);
              else nodesRef.current.set(row.key, node);
            }}
          >
            <TranscriptRow row={row} />
          </div>
        ))}
      </div>

      {rows.length === 0 && <p className="transcript-empty">{emptyHint ?? 'no events yet'}</p>}

      {!stuck && rows.length > 0 && (
        <button
          type="button"
          className="jump-latest"
          onClick={() => {
            const element = scrollRef.current;
            if (element === null) return;
            element.scrollTop = virtualizer.bottomScrollTop();
            virtualizer.setScrollTop(element.scrollTop);
            setStuck(true);
          }}
        >
          ↓ latest
        </button>
      )}
    </div>
  );
}