// End-to-end check of the thing the milestone is graded on: with 5,000
// transcript rows, the DOM contains a window, not 5,000 nodes.
//
// jsdom has no layout, so clientHeight is stubbed for the scroll container. That
// is the only fake here: the virtualizer math, the row projection and the React
// tree are real.
import { beforeEach, describe, expect, it } from 'vitest';
import type { EventEnvelope } from '../api/events';
import { RowAccumulator, compressedBlocks } from '../state/rows';
import { Transcript } from './Transcript';
import { mount } from '../test/render';

const VIEWPORT = 600;

function userEvent(seq: number): EventEnvelope {
  return {
    v: 1,
    seq,
    id: `evt-${String(seq)}`,
    ts: 1_700_000_000_000 + seq,
    session: 'sess-1',
    type: 'user/message',
    payload: { text: `message number ${String(seq)}` },
  };
}

function rowsOf(count: number): { rows: ReturnType<RowAccumulator['rows']['slice']>; rowKeys: string[] } {
  const accumulator = new RowAccumulator();
  for (let seq = 1; seq <= count; seq += 1) accumulator.push(userEvent(seq));
  return { rows: accumulator.rows, rowKeys: accumulator.rowKeys() };
}

beforeEach(() => {
  // Pretend this element has a viewport; jsdom reports 0 for everything.
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return this.classList.contains('transcript') ? VIEWPORT : 0;
    },
  });
});

describe('Transcript virtualization', () => {
  it('renders a bounded window of 5,000 rows', () => {
    const { rows, rowKeys } = rowsOf(5000);
    const view = mount(<Transcript rows={rows} rowKeys={rowKeys} />);

    const rendered = view.container.querySelectorAll('.row-wrap');
    // 600px viewport / 48px estimate = ~13 rows, plus one screen of overscan
    // above and below: bounded, and far below 5,000.
    expect(rendered.length).toBeGreaterThan(0);
    expect(rendered.length).toBeLessThan(60);

    // The scroll height still describes all 5,000 rows.
    const inner = view.container.querySelector('.transcript-inner') as HTMLElement | null;
    expect(inner).not.toBeNull();
    expect(Number.parseFloat(inner?.style.height ?? '0')).toBe(5000 * 48);
    view.unmount();
  });

  it('keeps the window bounded as the transcript grows', () => {
    const first = rowsOf(500);
    const view = mount(<Transcript rows={first.rows} rowKeys={first.rowKeys} />);
    const initial = view.container.querySelectorAll('.row-wrap').length;

    // Re-project a longer transcript and re-render: still one window.
    const grown = rowsOf(5000);
    view.rerender(<Transcript rows={grown.rows} rowKeys={grown.rowKeys} />);
    const rendered = view.container.querySelectorAll('.row-wrap').length;
    expect(rendered).toBeLessThan(60);
    expect(rendered).toBeGreaterThan(0);
    expect(initial).toBeLessThan(60);
    view.unmount();
  });

  it('shows the empty hint when there is nothing to render', () => {
    const view = mount(<Transcript rows={[]} rowKeys={[]} emptyHint="正在等待第一个事件…" />);
    expect(view.container.textContent).toContain('正在等待第一个事件…');
    view.unmount();
  });
});

describe('Transcript folded rows', () => {
  const BLOCK = 'c'.repeat(32);

  function rowsWithBlock(): ReturnType<typeof rowsOf> {
    const events: EventEnvelope[] = [
      userEvent(1),
      userEvent(2),
      userEvent(3),
      {
        v: 1,
        seq: 4,
        id: 'evt-4',
        ts: 1_700_000_000_004,
        session: 'sess-1',
        type: 'context/compressed',
        payload: { blockId: BLOCK, from: 2, to: 2, summary: 'a summary', messages: 1 },
      },
    ];
    // The same two steps the store runs per flush: push the new events, then
    // hand the accumulator the block list it derives from the whole log.
    const accumulator = new RowAccumulator();
    for (const event of events) accumulator.push(event);
    accumulator.setBlocks(compressedBlocks(events));
    return { rows: accumulator.rows, rowKeys: accumulator.rowKeys() };
  }

  it('marks the rows the model cannot see and names the block on the wrapper', () => {
    const { rows, rowKeys } = rowsWithBlock();
    const view = mount(<Transcript rows={rows} rowKeys={rowKeys} />);

    const folded = [...view.container.querySelectorAll('.row-wrap.row-folded')];
    expect(folded).toHaveLength(1);
    expect(folded[0]?.getAttribute('data-folded-by')).toBe(BLOCK);
    expect(folded[0]?.getAttribute('title')).toBe(`该行被块 ${BLOCK} 从模型中隐藏`);

    // The neighbouring rows, and the note describing the block, are untouched.
    const wrappers = [...view.container.querySelectorAll('.row-wrap')];
    const unfolded = wrappers.filter((node) => !node.classList.contains('row-folded'));
    expect(unfolded.map((node) => node.getAttribute('title'))).toEqual(unfolded.map(() => null));
    expect(view.container.querySelector('.row-compressed')?.closest('.row-wrap')?.classList.contains('row-folded')).toBe(false);
    view.unmount();
  });
});