// Virtualizer for the transcript: variable row heights, prefix-sum offsets,
// measured-height cache keyed by row key, sticky scroll-to-bottom and scroll
// anchoring across prepends.
//
// The acceptance criterion (docs/05-roadmap.md M3) is "5,000 messages scroll at
// 60 fps" and docs/02-architecture.md §10.1.1 says "render viewport ±1 screen".
// So this renders a window three screens tall at most: one screen below, the
// viewport, one screen above. It never walks all rows to render.
//
// Two invariants the tests pin:
//
//   * `offsetOf(i)` is the sum of the heights of rows [0, i), and
//     `indexAtOffset(offsetOf(i)) === i`. Offsets and heights stay consistent
//     after a measurement, an append and a prepend.
//   * The rendered count is bounded by the viewport and the estimate, not by
//     the row count.

import { OffsetIndex } from './offsetIndex';

export interface VirtualizerOptions {
  /** Height assumed for a row that has not been measured yet. */
  estimateRowHeight?: number;
  /** How many extra screens to render above and below. Default 1 (spec). */
  overscanScreens?: number;
  /** Always render at least this many rows around the viewport. */
  minOverscanRows?: number;
}

export type RowChangeKind = 'none' | 'append' | 'prepend' | 'reset';

export interface RowChange {
  kind: RowChangeKind;
  appended: number;
  prepended: number;
}

/** Where the viewport's top edge sits inside one row, so it can be restored. */
export interface ScrollAnchor {
  key: string;
  /** Pixels from the row's top edge to the viewport's top edge. */
  offsetWithin: number;
}

export interface VisibleRange {
  /** First rendered row index (inclusive). */
  start: number;
  /** One past the last rendered row index. */
  end: number;
  /** Pixels of spacer above the rendered window. */
  offsetTop: number;
  /** Pixels of spacer below the rendered window. */
  offsetBottom: number;
  totalHeight: number;
  renderedCount: number;
}

export interface MeasureResult {
  changed: boolean;
  /** Pixels the scroll position had to move to keep content still. */
  scrollShift: number;
}

const DEFAULT_ESTIMATE = 40;

export class Virtualizer {
  private readonly estimate: number;
  private readonly overscanScreens: number;
  private readonly minOverscanRows: number;

  private keys: string[] = [];
  private indexByKey = new Map<string, number>();
  private index = new OffsetIndex();
  private measured = new Map<string, number>();

  private viewportHeight = 0;
  private scrollTop = 0;
  private stickToBottom = true;

  constructor(options: VirtualizerOptions = {}) {
    this.estimate = Math.max(1, options.estimateRowHeight ?? DEFAULT_ESTIMATE);
    this.overscanScreens = Math.max(0, options.overscanScreens ?? 1);
    this.minOverscanRows = Math.max(0, options.minOverscanRows ?? 2);
  }

  /** The row keys currently known, in order. */
  rowKeys(): readonly string[] {
    return this.keys;
  }

  rowCount(): number {
    return this.keys.length;
  }

  /** Total content height in pixels. */
  totalHeight(): number {
    return this.index.totalHeight();
  }

  /** Height of rows [0, index). */
  offsetOf(index: number): number {
    return this.index.offsetOf(index);
  }

  /** Row index of the row containing `offset`. */
  indexAtOffset(offset: number): number {
    return this.index.indexAtOffset(offset);
  }

  keyAt(index: number): string | undefined {
    return this.keys[index];
  }

  indexOfKey(key: string): number | undefined {
    return this.indexByKey.get(key);
  }

  /** Height currently used for a row (measured, else the estimate). */
  heightOf(index: number): number {
    const key = this.keys[index];
    if (key === undefined) return 0;
    return this.measured.get(key) ?? this.estimate;
  }

  viewport(): number {
    return this.viewportHeight;
  }

  top(): number {
    return this.scrollTop;
  }

  /** True while the viewport is pinned to the newest content (streaming). */
  isStuckToBottom(): boolean {
    return this.stickToBottom;
  }

  setViewport(height: number): void {
    const next = Math.max(0, height);
    if (next === this.viewportHeight) return;
    this.viewportHeight = next;
    if (this.stickToBottom) this.scrollTop = this.bottomScrollTop();
  }

  /**
   * Record the user's scroll position. Reaching the bottom re-enables sticky
   * mode; leaving it disables it, which is what lets a user scroll up while
   * tokens are still streaming.
   */
  setScrollTop(top: number): void {
    const clamped = Math.max(0, Math.min(top, this.maxScrollTop()));
    this.scrollTop = clamped;
    this.stickToBottom = clamped >= this.maxScrollTop() - 1;
  }

  /** Scroll offset that pins the viewport to the bottom. */
  bottomScrollTop(): number {
    return this.maxScrollTop();
  }

  maxScrollTop(): number {
    return Math.max(0, this.index.totalHeight() - this.viewportHeight);
  }

  /**
   * Where the scroll position should be after the current state is rendered.
   * Sticky mode keeps the viewport at the bottom as rows stream in.
   */
  desiredScrollTop(): number {
    return this.stickToBottom ? this.bottomScrollTop() : this.scrollTop;
  }

  /**
   * Replace the row keys. The change is classified so the caller can decide
   * whether to restore an anchor (prepend) or stay pinned (append).
   */
  setRows(next: readonly string[]): RowChange {
    const old = this.keys;
    let prefix = 0;
    while (prefix < old.length && prefix < next.length && old[prefix] === next[prefix]) {
      prefix += 1;
    }
    let suffix = 0;
    while (
      suffix < old.length - prefix &&
      suffix < next.length - prefix &&
      old[old.length - 1 - suffix] === next[next.length - 1 - suffix]
    ) {
      suffix += 1;
    }

    let kind: RowChangeKind = 'none';
    if (prefix === old.length && prefix === next.length) {
      kind = 'none';
    } else if (prefix === old.length && next.length >= old.length) {
      kind = 'append';
    } else if (suffix === old.length && next.length >= old.length) {
      kind = 'prepend';
    } else {
      kind = 'reset';
    }

    const appended = kind === 'append' ? next.length - old.length : 0;
    const prepended = kind === 'prepend' ? next.length - old.length : 0;

    if (kind === 'none') return { kind, appended: 0, prepended: 0 };

    if (kind === 'append') {
      this.keys = [...old];
      for (let i = old.length; i < next.length; i += 1) {
        const key = next[i] ?? '';
        this.keys.push(key);
        this.indexByKey.set(key, i);
        this.index.append(this.measured.get(key) ?? this.estimate);
      }
    } else {
      // Prepend or reset: rebuild the offset index from the cached heights.
      // This is O(n) and happens once per page of older events, not per frame.
      this.keys = [...next];
      this.indexByKey = new Map<string, number>();
      const heights = new Array<number>(next.length);
      for (let i = 0; i < next.length; i += 1) {
        const key = next[i] ?? '';
        this.indexByKey.set(key, i);
        heights[i] = this.measured.get(key) ?? this.estimate;
      }
      this.index.rebuild(heights);
    }

    if (this.stickToBottom) this.scrollTop = this.bottomScrollTop();
    return { kind, appended, prepended };
  }

  /**
   * Cache a measured height for a row (by key). A row above the viewport that
   * changes height shifts content, so the scroll position is compensated when
   * the user is not pinned to the bottom.
   */
  measure(key: string, height: number): MeasureResult {
    const rowIndex = this.indexByKey.get(key);
    if (rowIndex === undefined) return { changed: false, scrollShift: 0 };
    if (!Number.isFinite(height) || height < 0) return { changed: false, scrollShift: 0 };
    const previous = this.measured.get(key) ?? this.estimate;
    if (Math.abs(previous - height) < 0.5) return { changed: false, scrollShift: 0 };
    this.measured.set(key, height);
    const delta = this.index.setHeight(rowIndex, height);
    if (delta === 0) return { changed: false, scrollShift: 0 };
    if (!this.stickToBottom) {
      const firstVisible = this.index.indexAtOffset(this.scrollTop);
      if (rowIndex < firstVisible) {
        this.scrollTop = Math.max(0, Math.min(this.scrollTop + delta, this.maxScrollTop()));
        return { changed: true, scrollShift: delta };
      }
    } else {
      this.scrollTop = this.bottomScrollTop();
    }
    return { changed: true, scrollShift: 0 };
  }

  /** Capture the viewport top as (row key, offset within row). */
  anchor(): ScrollAnchor | null {
    if (this.keys.length === 0) return null;
    const rowIndex = this.index.indexAtOffset(this.scrollTop);
    const key = this.keys[rowIndex];
    if (key === undefined) return null;
    return { key, offsetWithin: this.scrollTop - this.index.offsetOf(rowIndex) };
  }

  /** Scroll offset that puts `anchor` back where it was before a prepend. */
  scrollTopForAnchor(anchor: ScrollAnchor): number {
    const rowIndex = this.indexByKey.get(anchor.key);
    if (rowIndex === undefined) return this.scrollTop;
    return Math.max(
      0,
      Math.min(this.index.offsetOf(rowIndex) + anchor.offsetWithin, this.maxScrollTop()),
    );
  }

  /** The window of rows to render, plus the spacer heights around it. */
  range(): VisibleRange {
    const totalHeight = this.index.totalHeight();
    if (this.keys.length === 0) {
      return {
        start: 0,
        end: 0,
        offsetTop: 0,
        offsetBottom: 0,
        totalHeight,
        renderedCount: 0,
      };
    }
    const viewport = this.viewportHeight > 0 ? this.viewportHeight : this.estimate * 10;
    const overscanPx = viewport * this.overscanScreens;
    const firstOffset = Math.max(0, this.scrollTop - overscanPx);
    const lastOffset = Math.min(
      Math.max(0, totalHeight - 1),
      this.scrollTop + viewport + overscanPx,
    );

    let start = this.index.indexAtOffset(firstOffset);
    let end = this.index.indexAtOffset(lastOffset) + 1;

    // Honour the minimum overscan even when rows are taller than estimated.
    const minRows = this.minOverscanRows;
    if (end - start < minRows) {
      const grow = minRows - (end - start);
      start = Math.max(0, start - grow);
      end = Math.min(this.keys.length, start + minRows);
      start = Math.max(0, end - minRows);
    }

    const offsetTop = this.index.offsetOf(start);
    const offsetBottom = Math.max(0, totalHeight - this.index.offsetOf(end));
    return {
      start,
      end,
      offsetTop,
      offsetBottom,
      totalHeight,
      renderedCount: end - start,
    };
  }
}