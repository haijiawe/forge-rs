// 编辑器里那条「这个文件与提交版本不同」的标记。
//
// 这里能验的三件事，恰好是界面唯一能声称的三件事：
//   * 条子说的是「与提交版本相比」，数字来自 diff，编辑区画的是补充行；
//   * 兜底编辑器的行号栏把改过的行染上色（它画不了整行背景）；
//   * 撤销按钮只报告「按了」，不自己写盘 —— 谁落盘是 store 的决定。
//
// 至于「哪几行是 agent 改的」，这里验不了也断言不了：日志里只存摘要不存内容，
// 服务端没有那份信息。测试不去假装有。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, waitFor } from '../../test/render';
import { EditorPane } from './EditorPane';

const fake = vi.hoisted(() => {
  const calls = {
    /** One entry per collection created, holding its class names. */
    collections: [] as { className: string; from: number; to: number }[][],
    created: 0,
  };

  const instance = {
    onDidChangeModelContent: () => ({ dispose: () => {} }),
    addCommand: () => {},
    setValue: () => {},
    getValue: () => '',
    getModel: () => null,
    updateOptions: () => {},
    revealLineInCenter: () => {},
    setPosition: () => {},
    createDecorationsCollection: (
      items: {
        range?: { startLineNumber: number; endLineNumber: number };
        options?: { className?: string };
      }[],
    ) => {
      calls.created += 1;
      calls.collections.push(
        items.map((item) => ({
          className: item.options?.className ?? '',
          from: item.range?.startLineNumber ?? 0,
          to: item.range?.endLineNumber ?? 0,
        })),
      );
      return { clear: () => {} };
    },
    onDidChangeCursorSelection: () => ({ dispose: () => {} }),
    onDidScrollChange: () => ({ dispose: () => {} }),
    getSelection: () => null,
    getScrolledVisiblePosition: () => null,
    getLayoutInfo: () => ({ width: 800, contentLeft: 60 }),
    dispose: () => {},
  };

  const monaco = {
    editor: {
      create: () => instance,
      setModelLanguage: () => {},
    },
    KeyMod: { CtrlCmd: 1 },
    KeyCode: { KeyS: 2 },
  };

  return { calls, monaco };
});

vi.mock('./canvas', () => ({ canvasAvailableOnce: () => true }));
vi.mock('./monacoSetup', () => ({
  configureMonaco: () => {},
  applyTheme: () => {},
  monaco: fake.monaco,
  THEME: 'fake',
  languageFor: () => 'plaintext',
}));

const TEXT = Array.from({ length: 40 }, (_, index) => `line ${String(index + 1)}`).join('\n');

const CHANGED = {
  ranges: [
    { from: 3, to: 4 },
    { from: 12, to: 12 },
  ],
  added: 3,
  removed: 1,
  seen: true,
};

beforeEach(() => {
  fake.calls.collections.length = 0;
  fake.calls.created = 0;
});

describe('EditorPane — 改动标记', () => {
  it('draws a mark on every changed line, in its own collection', async () => {
    const view = mount(
      <EditorPane path="src/main.ts" text={TEXT} onChange={() => {}} changed={CHANGED} />,
    );

    await waitFor(() => fake.calls.collections.length > 0);

    const marks = fake.calls.collections.at(-1) ?? [];
    expect(marks).toEqual([
      { className: 'wb-line-changed', from: 3, to: 4 },
      { className: 'wb-line-changed', from: 12, to: 12 },
    ]);
    view.unmount();
  });

  it('draws nothing when nobody asked about this file', async () => {
    // 带一个跳行，好让「编辑器已就绪」有个可等的信号：跳行会建自己的集合。
    // 要验的是改动标记不会跟着一起冒出来 —— "没问过"不是"没改动"。
    const view = mount(
      <EditorPane path="src/main.ts" text={TEXT} onChange={() => {}} revealLine={5} />,
    );
    await waitFor(() => fake.calls.collections.length > 0);

    const classes = fake.calls.collections.flat().map((item) => item.className);
    expect(classes).toEqual(['wb-line-revealed']);
    view.unmount();
  });

  it('replaces the marks when the diff is re-read, rather than adding to them', async () => {
    const view = mount(
      <EditorPane path="src/main.ts" text={TEXT} onChange={() => {}} changed={CHANGED} />,
    );
    await waitFor(() => fake.calls.collections.length > 0);

    view.rerender(
      <EditorPane
        path="src/main.ts"
        text={TEXT}
        onChange={() => {}}
        changed={{ ranges: [{ from: 20, to: 20 }], added: 1, removed: 0, seen: true }}
      />,
    );

    const latest = fake.calls.collections.at(-1) ?? [];
    expect(latest).toEqual([{ className: 'wb-line-changed', from: 20, to: 20 }]);
    view.unmount();
  });
});
