import { describe, expect, it } from 'vitest';
import { OffsetIndex } from './offsetIndex';
import { Virtualizer } from './virtualizer';

function keys(from: number, count: number, prefix = 'row'): string[] {
  const out = new Array<string>(count);
  for (let i = 0; i < count; i += 1) out[i] = `${prefix}-${from + i}`;
  return out;
}

describe('OffsetIndex', () => {
  it('keeps offsets and heights consistent after point measurements', () => {
    const index = new OffsetIndex([10, 20, 30]);
    expect(index.totalHeight()).toBe(60);
    expect(index.offsetOf(0)).toBe(0);
    expect(index.offsetOf(1)).toBe(10);
    expect(index.offsetOf(2)).toBe(30);
    expect(index.offsetOf(3)).toBe(60);
    expect(index.indexAtOffset(0)).toBe(0);
    expect(index.indexAtOffset(9)).toBe(0);
    expect(index.indexAtOffset(10)).toBe(1);
    expect(index.indexAtOffset(29)).toBe(1);
    expect(index.indexAtOffset(30)).toBe(2);
    expect(index.indexAtOffset(59)).toBe(2);

    index.setHeight(1, 50);
    expect(index.totalHeight()).toBe(90);
    expect(index.offsetOf(2)).toBe(60);
    expect(index.indexAtOffset(59)).toBe(1);
    expect(index.indexAtOffset(60)).toBe(2);
  });

  it('appends without invalidating earlier offsets', () => {
    const index = new OffsetIndex(keys(0, 100, 'h').map(() => 40));
    const before = index.offsetOf(100);
    index.append(7);
    expect(index.totalHeight()).toBe(before + 7);
    expect(index.offsetOf(100)).toBe(before);
    expect(index.indexAtOffset(before + 6)).toBe(100);
  });
});

describe('Virtualizer', () => {
  it('renders a bounded window for 5,000 rows', () => {
    const virtualizer = new Virtualizer({ estimateRowHeight: 40, overscanScreens: 1 });
    virtualizer.setViewport(600);
    virtualizer.setRows(keys(0, 5000));

    expect(virtualizer.totalHeight()).toBe(5000 * 40);
    // New rows default to sticky-bottom (the transcript opens at the newest
    // event); scrolling to the top is what the user does to read history.
    virtualizer.setScrollTop(0);
    const top = virtualizer.range();
    expect(top.start).toBe(0);
    // Viewport plus one screen of overscan on each side at 40px/row.
    expect(top.renderedCount).toBeGreaterThan(0);
    expect(top.renderedCount).toBeLessThan(100);
    expect(top.offsetTop).toBe(0);

    virtualizer.setScrollTop(5000 * 40 * 0.5);
    const middle = virtualizer.range();
    expect(middle.start).toBeGreaterThan(1000);
    expect(middle.end).toBeLessThan(4000);
    expect(middle.renderedCount).toBeLessThan(100);
    // Spacers plus rendered rows add up to the content height exactly.
    expect(middle.offsetTop + middle.offsetBottom).toBeLessThanOrEqual(virtualizer.totalHeight());
  });

  it('keeps offsets consistent after measurement', () => {
    const virtualizer = new Virtualizer({ estimateRowHeight: 40 });
    virtualizer.setViewport(600);
    virtualizer.setRows(keys(0, 5000));

    // Measure the first 10 rows much taller than the estimate.
    for (let i = 0; i < 10; i += 1) {
      const key = virtualizer.keyAt(i);
      expect(key).toBeDefined();
      virtualizer.measure(key ?? '', 100);
    }

    expect(virtualizer.offsetOf(10)).toBe(10 * 100);
    expect(virtualizer.totalHeight()).toBe(10 * 100 + 4990 * 40);
    // Every sampled offset maps back to the row that starts there.
    for (const i of [0, 1, 5, 9, 10, 11, 499, 2500, 4999]) {
      expect(virtualizer.indexAtOffset(virtualizer.offsetOf(i))).toBe(i);
    }
    // And the rows after the measured prefix keep their estimate.
    expect(virtualizer.heightOf(4000)).toBe(40);
    expect(virtualizer.heightOf(3)).toBe(100);
  });

  it('restores the scroll anchor when older rows are prepended', () => {
    const virtualizer = new Virtualizer({ estimateRowHeight: 40 });
    virtualizer.setViewport(600);
    virtualizer.setRows(keys(100, 500, 'row'));

    virtualizer.setScrollTop(40 * 20 + 13);
    const anchor = virtualizer.anchor();
    expect(anchor).not.toBeNull();
    expect(anchor?.key).toBe('row-120');

    // Two older rows arrive, both taller than the estimate.
    const prepended = ['old-0', 'old-1'];
    const change = virtualizer.setRows([...prepended, ...keys(100, 500, 'row')]);
    expect(change.kind).toBe('prepend');
    expect(change.prepended).toBe(2);

    virtualizer.measure('old-0', 120);
    virtualizer.measure('old-1', 30);

    const restored = virtualizer.scrollTopForAnchor(anchor ?? { key: '', offsetWithin: 0 });
    // 20 unchanged rows sit above row-120: 150px of prepended content plus
    // 20 * 40px, plus the same 13px the viewport was into row-120.
    expect(restored).toBe(150 + 40 * 20 + 13);
    // Offsets stay consistent: a later row sits after the prepended content.
    expect(virtualizer.offsetOf(2)).toBe(150);
    expect(virtualizer.totalHeight()).toBe(150 + 40 * 500);
    for (const i of [0, 1, 2, 20, 21, 501]) {
      expect(virtualizer.indexAtOffset(virtualizer.offsetOf(i))).toBe(i);
    }
  });

  it('stays pinned to the bottom while streaming and lets go when scrolled up', () => {
    const virtualizer = new Virtualizer({ estimateRowHeight: 40 });
    virtualizer.setViewport(600);
    virtualizer.setRows(keys(0, 100));
    expect(virtualizer.isStuckToBottom()).toBe(true);
    expect(virtualizer.desiredScrollTop()).toBe(100 * 40 - 600);

    // Streaming appends keep the viewport at the bottom.
    virtualizer.setRows(keys(0, 120));
    expect(virtualizer.isStuckToBottom()).toBe(true);
    expect(virtualizer.desiredScrollTop()).toBe(120 * 40 - 600);

    // A user scrolling up is not yanked back by the next append.
    virtualizer.setScrollTop(400);
    expect(virtualizer.isStuckToBottom()).toBe(false);
    virtualizer.setRows(keys(0, 140));
    expect(virtualizer.desiredScrollTop()).toBe(400);
    expect(virtualizer.range().start).toBeLessThan(20);

    // Returning to the bottom re-arms sticky mode.
    virtualizer.setScrollTop(140 * 40 - 600);
    expect(virtualizer.isStuckToBottom()).toBe(true);
  });

  it('compensates scroll when a row above the viewport is measured', () => {
    const virtualizer = new Virtualizer({ estimateRowHeight: 40 });
    virtualizer.setViewport(600);
    virtualizer.setRows(keys(0, 200));
    virtualizer.setScrollTop(40 * 100);
    const before = virtualizer.top();

    const first = virtualizer.keyAt(3) ?? '';
    const result = virtualizer.measure(first, 80);
    expect(result.changed).toBe(true);
    expect(result.scrollShift).toBe(40);
    expect(virtualizer.top()).toBe(before + 40);
  });
});