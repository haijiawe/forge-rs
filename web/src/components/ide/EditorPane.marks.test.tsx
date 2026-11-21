// 兜底编辑器上的改动标记。
//
// 生产里跑 Monaco（整行背景由装饰画），jsdom 只能挂载 textarea 这一条，所以这里
// 验的是那条更弱的承诺：行号栏把改过的行染上色，条子把数字说清楚，撤销按钮把
// 请求往外报。三者都不能自己动盘。
import { describe, expect, it, vi } from 'vitest';
import { interact, mount } from '../../test/render';
import type { ChangedLines } from '../../state/changed';
import { EditorPane } from './EditorPane';

vi.mock('./canvas', () => ({ canvasAvailableOnce: () => false }));

const TEXT = 'one\ntwo\nthree\nfour\nfive';

const CHANGED: ChangedLines = {
  ranges: [{ from: 2, to: 2 }],
  added: 1,
  removed: 0,
  seen: true,
};

function gutterClasses(container: HTMLElement): string[] {
  return Array.from(container.querySelectorAll('.wb-gutter-line')).map(
    (element) => element.className,
  );
}

describe('EditorPane · 改动标记', () => {
  it('states the comparison and the counts above the text', () => {
    const view = mount(
      <EditorPane path="a.ts" text={TEXT} onChange={() => {}} changed={CHANGED} />,
    );
    const bar = view.container.querySelector('.wb-changed');
    expect(bar?.textContent).toContain('与提交版本相比：+1 −0');
    view.unmount();
  });

  it('marks exactly the changed gutter numbers', () => {
    const view = mount(
      <EditorPane path="a.ts" text={TEXT} onChange={() => {}} changed={CHANGED} />,
    );
    const classes = gutterClasses(view.container);
    expect(classes).toHaveLength(5);
    expect(classes[1]).toContain('wb-gutter-changed');
    expect(classes.filter((name) => name.includes('wb-gutter-changed'))).toHaveLength(1);
    view.unmount();
  });

  it('reports the restore request instead of writing the file itself', () => {
    const onRevertFile = vi.fn();
    const view = mount(
      <EditorPane
        path="a.ts"
        text={TEXT}
        onChange={() => {}}
        changed={CHANGED}
        onRevertFile={onRevertFile}
      />,
    );
    const button = view.container.querySelector<HTMLButtonElement>('.wb-changed-revert');
    expect(button?.textContent).toBe('撤销这个文件');
    interact(() => {
      button?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(onRevertFile).toHaveBeenCalledTimes(1);
    view.unmount();
  });

  it('says it is working, and refuses a second press while it is', () => {
    const view = mount(
      <EditorPane
        path="a.ts"
        text={TEXT}
        onChange={() => {}}
        changed={CHANGED}
        onRevertFile={() => {}}
        reverting
      />,
    );
    const button = view.container.querySelector<HTMLButtonElement>('.wb-changed-revert');
    expect(button?.textContent).toBe('还原中…');
    expect(button?.disabled).toBe(true);
    view.unmount();
  });

  it('draws no bar when the file matches the commit', () => {
    const same: ChangedLines = { ranges: [], added: 0, removed: 0, seen: true };
    const view = mount(<EditorPane path="a.ts" text={TEXT} onChange={() => {}} changed={same} />);
    expect(view.container.querySelector('.wb-changed')).toBeNull();
    view.unmount();
  });

  it('draws no bar when the diff could not be read as lines', () => {
    // seen:false 是"这份 diff 读不懂"（二进制、空），不是"没有改动"。
    const binary: ChangedLines = { ranges: [], added: 0, removed: 0, seen: false };
    const view = mount(
      <EditorPane path="a.ts" text={TEXT} onChange={() => {}} changed={binary} />,
    );
    expect(view.container.querySelector('.wb-changed')).toBeNull();
    view.unmount();
  });

  it('says so when the server cut the diff short', () => {
    const view = mount(
      <EditorPane
        path="a.ts"
        text={TEXT}
        onChange={() => {}}
        changed={CHANGED}
        changedTruncated
      />,
    );
    expect(view.container.querySelector('.wb-changed')?.textContent).toContain('截断');
    view.unmount();
  });
});
