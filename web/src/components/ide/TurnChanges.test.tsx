// 按轮改动面板：给它数据，看它画了什么、点了回调什么。
//
// 折叠逻辑（哪些事件算一次改动、同一路径写两次怎么合）由
// `state/turnChanges.test.ts` 守着；这里只管渲染、顺序与手势。
import { describe, expect, it, vi } from 'vitest';

import { interact, mount } from '../../test/render';
import type { TurnChangeTurn } from '../../state/turnChanges';
import { TurnChanges } from './TurnChanges';

function turn(over: Partial<TurnChangeTurn> = {}): TurnChangeTurn {
  return {
    turn: 1,
    files: [
      {
        path: 'src/main.rs',
        added: 2,
        removed: 1,
        ranges: [{ from: 3, to: 4 }],
        coarse: false,
        cut: false,
        writes: 1,
      },
    ],
    added: 2,
    removed: 1,
    ...over,
  };
}

describe('TurnChanges', () => {
  it('没有改动时说清楚为什么，而不是画一张空表', () => {
    const view = mount(<TurnChanges turns={[]} onOpen={vi.fn()} />);
    expect(view.container.querySelector('.wb-empty')?.textContent).toContain('还没有任何可跳转的改动');
    expect(view.container.querySelectorAll('.wb-turn').length).toBe(0);
  });

  it('最新的轮排在最前面，并给出这一轮的合计', () => {
    const view = mount(
      <TurnChanges
        turns={[turn({ turn: 1 }), turn({ turn: 2, added: 5, removed: 0 })]}
        onOpen={vi.fn()}
      />,
    );
    const heads = [...view.container.querySelectorAll('.wb-turn-head')].map(
      (head) => head.querySelector('.wb-turn-number')?.textContent,
    );
    expect(heads).toEqual(['第 2 轮', '第 1 轮']);
    expect(view.container.querySelector('.wb-turn-count')?.textContent).toContain('+5 −0');
  });

  it('点文件名跳到第一处改动，点范围跳到那一处', () => {
    const onOpen = vi.fn();
    const view = mount(<TurnChanges turns={[turn()]} onOpen={onOpen} />);
    const file = view.container.querySelector('.wb-turn-file');
    expect(file?.querySelector('.wb-turn-path')?.textContent).toBe('src/main.rs');
    file?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(onOpen).toHaveBeenLastCalledWith('src/main.rs', 3);
  });

  it('多次写入同一文件时说出来，行号仍指最终文本', () => {
    const view = mount(
      <TurnChanges
        turns={[
          turn({
            files: [
              {
                path: 'src/main.rs',
                added: 1,
                removed: 0,
                ranges: [{ from: 9, to: 9 }],
                coarse: false,
                cut: false,
                writes: 3,
              },
            ],
          }),
        ]}
        onOpen={vi.fn()}
      />,
    );
    expect(view.container.querySelector('.wb-turn-writes')?.textContent).toBe('写入 3 次');
    expect(view.container.querySelector('.wb-turn-range')?.textContent).toBe('第 9 行');
  });

  it('范围内被截断或粗略时说明，不把不完整的标记当完整', () => {
    const view = mount(
      <TurnChanges
        turns={[
          turn({
            files: [
              {
                path: 'big.rs',
                added: 40,
                removed: 0,
                ranges: [{ from: 1, to: 40 }],
                coarse: true,
                cut: true,
                writes: 1,
              },
            ],
          }),
        ]}
        onOpen={vi.fn()}
      />,
    );
    const more = [...view.container.querySelectorAll('.wb-turn-more')].map((node) => node.textContent);
    expect(more).toContain('文件太大，只给了一段大致范围');
    expect(more).toContain('范围被截断，标记不完整');
  });

  it('范围多于四个时只画前四个，并说还有多少处', async () => {
    const ranges = [
      { from: 1, to: 1 },
      { from: 3, to: 3 },
      { from: 5, to: 5 },
      { from: 7, to: 7 },
      { from: 9, to: 9 },
    ];
    const view = mount(
      <TurnChanges
        turns={[turn({ files: [{ path: 'a', added: 5, removed: 0, ranges, coarse: false, cut: false, writes: 1 }] })]}
        onOpen={vi.fn()}
      />,
    );
    expect(view.container.querySelectorAll('.wb-turn-range').length).toBe(4);
    expect(view.container.querySelector('.wb-turn-more')?.textContent).toBe('还有 1 处');
    // `interact` keeps the act() warning out of the suite when a click is asserted
    // through React state; here it is only to keep parity with the other tests.
    await interact(() => {});
  });
});
