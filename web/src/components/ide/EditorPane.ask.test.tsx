// 兜底编辑器上的选区入口。生产里跑的是 Monaco，但两条路对父组件的说法必须是
// 同一个（path + 行区间 + 原文），而 jsdom 只能挂载这一条，所以这里测的是那份
// 约定本身：行号怎么从光标位置数出来、空白选区不给入口、点下去报告的是什么。
import { describe, expect, it, vi } from 'vitest';
import { interact, mount } from '../../test/render';
import type { SelectionAsk } from '../../state/ask';
import { EditorPane } from './EditorPane';

vi.mock('./canvas', () => ({ canvasAvailableOnce: () => false }));

const TEXT = 'a\nb\nc\nd\ne';

function areaOf(container: HTMLElement): HTMLTextAreaElement {
  const element = container.querySelector('textarea');
  if (element === null) throw new Error('no textarea');
  return element;
}

function chipOf(container: HTMLElement): HTMLButtonElement | null {
  return container.querySelector('button.wb-ask-chip');
}

describe('EditorPane · 把选区交给 agent', () => {
  it('reports the file, the lines and the exact text that was selected', () => {
    const asks: SelectionAsk[] = [];
    const view = mount(
      <EditorPane
        path="src/main.rs"
        text={TEXT}
        onChange={() => {}}
        onAskSelection={(ask) => asks.push(ask)}
      />,
    );
    const area = areaOf(view.container);
    // 0:a 1:\n 2:b 3:\n 4:c 5:\n 6:d 7:\n 8:e — 选中 "b\nc"。
    interact(() => {
      area.setSelectionRange(2, 5);
      // 真实的选区是拖出来的：select 之后紧跟一次 mouseup。React 对 select 的
      // 合成在 jsdom 里不触发，鼠标抬起这一条是同一手势里真实存在的事件。
      area.dispatchEvent(new Event('select', { bubbles: true }));
      area.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    });
    const chip = chipOf(view.container);
    expect(chip?.textContent).toContain('第 2–3 行');

    interact(() => {
      chip?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(asks).toEqual([{ path: 'src/main.rs', fromLine: 2, toLine: 3, text: 'b\nc' }]);
  });

  it('offers nothing when the selection is empty', () => {
    const view = mount(
      <EditorPane path="a.ts" text={TEXT} onChange={() => {}} onAskSelection={() => {}} />,
    );
    const area = areaOf(view.container);
    interact(() => {
      area.setSelectionRange(4, 4);
      area.dispatchEvent(new Event('select', { bubbles: true }));
      area.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    });
    expect(chipOf(view.container)).toBeNull();
  });

  it('sends whole lines when the drag starts and ends mid-line', () => {
    const asks: SelectionAsk[] = [];
    const view = mount(
      <EditorPane path="src/main.rs" text={'let a = 1;\nlet b = 2;'} onChange={() => {}} onAskSelection={(ask) => asks.push(ask)} />,
    );
    const area = areaOf(view.container);
    // 5 = 第 1 行 "let a" 之后，17 = 第 2 行中间：两头都在行内，不是整行。
    interact(() => {
      area.setSelectionRange(5, 17);
      area.dispatchEvent(new Event('select', { bubbles: true }));
      area.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    });
    expect(chipOf(view.container)?.textContent).toContain('第 1–2 行');
    interact(() => {
      chipOf(view.container)?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    // 按钮上写的是「第 1–2 行」，发出去的就必须是那两整行。
    expect(asks[0]).toEqual({ path: 'src/main.rs', fromLine: 1, toLine: 2, text: 'let a = 1;\nlet b = 2;' });
  });

  it('does not take focus away when the chip is pressed', () => {
    const view = mount(<EditorPane path="a.ts" text={TEXT} onChange={() => {}} onAskSelection={() => {}} />);
    const area = areaOf(view.container);
    interact(() => {
      area.setSelectionRange(0, 1);
      area.dispatchEvent(new Event('select', { bubbles: true }));
      area.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    });
    const chip = chipOf(view.container);
    if (chip === null) throw new Error('no chip');
    const press = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
    interact(() => {
      chip.dispatchEvent(press);
    });
    // 真机上这一下原本把焦点从输入框抢到按钮上，人接着敲的字全丢。
    expect(press.defaultPrevented).toBe(true);
  });

  it('counts the first line as line one, not line zero', () => {
    const asks: SelectionAsk[] = [];
    const view = mount(
      <EditorPane path="a.ts" text={TEXT} onChange={() => {}} onAskSelection={(ask) => asks.push(ask)} />,
    );
    const area = areaOf(view.container);
    interact(() => {
      area.setSelectionRange(0, 1);
      area.dispatchEvent(new Event('select', { bubbles: true }));
      area.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    });
    expect(chipOf(view.container)?.textContent).toContain('第 1 行');
    interact(() => {
      chipOf(view.container)?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(asks[0]).toEqual({ path: 'a.ts', fromLine: 1, toLine: 1, text: 'a' });
  });
});
