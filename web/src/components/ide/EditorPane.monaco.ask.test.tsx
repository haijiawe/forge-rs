// Monaco 那条路上的选区：真机上发现的缺陷是「按钮说第 5–7 行，发出去的却是
// 从落点开始的半截（`t line5 = 5;`）」，因为选区带着列号。这里用一份会记事
// 的假模型钉住：无论鼠标落在行的哪个位置，交给输入框的都是那几整行。
//
// jsdom 没有 canvas，所以 "./canvas" 答 yes，"./monacoSetup" 是假的——和
// EditorPane.test.tsx 里那套一样，只是这里的模型不再是 null。
import { describe, expect, it, vi } from 'vitest';
import { interact, mount, waitFor } from '../../test/render';
import type { SelectionAsk } from '../../state/ask';
import { EditorPane } from './EditorPane';

const fake = vi.hoisted(() => {
  const TEXT = 'let a = 1;\nlet b = 2;\nlet c = 3;';
  const LINES = TEXT.split('\n');
  const state = {
    selection: null as null | {
      startLineNumber: number;
      startColumn: number;
      endLineNumber: number;
      endColumn: number;
    },
    listener: null as null | (() => void),
    ranges: [] as unknown[],
  };

  const model = {
    getLineMaxColumn: (line: number) => (LINES[line - 1] ?? '').length + 1,
    getValueInRange: (range: {
      startLineNumber: number;
      startColumn: number;
      endLineNumber: number;
      endColumn: number;
    }) => {
      state.ranges.push(range);
      const out: string[] = [];
      for (let line = range.startLineNumber; line <= range.endLineNumber; line += 1) {
        const content = LINES[line - 1] ?? '';
        const from = line === range.startLineNumber ? range.startColumn - 1 : 0;
        const to =
          line === range.endLineNumber ? Math.min(range.endColumn - 1, content.length) : content.length;
        out.push(content.slice(from, to));
      }
      return out.join('\n');
    },
  };

  const instance = {
    onDidChangeModelContent: () => ({ dispose: () => {} }),
    addCommand: () => {},
    setValue: () => {},
    getValue: () => TEXT,
    getModel: () => model,
    updateOptions: () => {},
    revealLineInCenter: () => {},
    setPosition: () => {},
    createDecorationsCollection: () => ({ clear: () => {} }),
    onDidChangeCursorSelection: (listener: () => void) => {
      state.listener = listener;
      return { dispose: () => {} };
    },
    onDidScrollChange: () => ({ dispose: () => {} }),
    getSelection: () => state.selection,
    // 第 n 行在视口里的位置：20px 一行，够浮层算坐标了。
    getScrolledVisiblePosition: (position: { lineNumber: number }) => ({
      top: (position.lineNumber - 1) * 20,
      left: 0,
      height: 20,
    }),
    getLayoutInfo: () => ({ width: 800, contentLeft: 60 }),
    dispose: () => {},
  };

  const monaco = {
    editor: { create: () => instance, setModelLanguage: () => {} },
    KeyMod: { CtrlCmd: 1 },
    KeyCode: { KeyS: 2 },
  };

  return { TEXT, state, monaco };
});

vi.mock('./canvas', () => ({ canvasAvailableOnce: () => true }));
vi.mock('./monacoSetup', () => ({
  configureMonaco: () => {},
  applyTheme: () => {},
  monaco: fake.monaco,
  THEME: 'fake',
  languageFor: () => 'plaintext',
}));

describe('EditorPane · Monaco 上的选区', () => {
  it('sends whole lines even when the drag starts and ends mid-line', async () => {
    const asks: SelectionAsk[] = [];
    const view = mount(
      <EditorPane path="src/main.rs" text={fake.TEXT} onChange={() => {}} onAskSelection={(ask) => asks.push(ask)} />,
    );
    await waitFor(() => fake.state.listener !== null);

    interact(() => {
      // 第 1 行第 5 列拖到第 2 行第 4 列：两头都在行内。
      fake.state.selection = {
        startLineNumber: 1,
        startColumn: 5,
        endLineNumber: 2,
        endColumn: 4,
      };
      fake.state.listener?.();
    });

    const chip = view.container.querySelector('button.wb-ask-chip');
    expect(chip?.textContent).toContain('第 1–2 行');
    interact(() => {
      chip?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });

    expect(asks).toEqual([{ path: 'src/main.rs', fromLine: 1, toLine: 2, text: 'let a = 1;\nlet b = 2;' }]);
    // 要的是整行：起点列是 1，终点列是那行的最大列 + 1。
    expect(fake.state.ranges).toEqual([
      { startLineNumber: 1, startColumn: 1, endLineNumber: 2, endColumn: 11 },
    ]);
  });
});
