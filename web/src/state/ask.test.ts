// 选区变成草稿文本的规则，以及那条把两侧接起来的线。
//
// 这里测的是格式：文件与行号必须在围栏外面（否则模型只能猜代码属于谁），
// 截断必须写出来（「少了 300 行」会改变对方怎么回答），一行的选区不该说成
// 区间。发射器本身只测「谁订阅谁收到、退订之后不再收到」。
import { describe, expect, it } from 'vitest';
import {
  askAboutSelection,
  MAX_SNIPPET_LINES,
  onAskRequested,
  rangeLabel,
  snippetFor,
  type SelectionAsk,
} from './ask';

const ask: SelectionAsk = {
  path: 'src/main.rs',
  fromLine: 12,
  toLine: 14,
  text: 'let a = 1;\nlet b = 2;\nlet c = 3;',
};

describe('rangeLabel', () => {
  it('names one line without an interval', () => {
    expect(rangeLabel(12, 12)).toBe('第 12 行');
  });

  it('names a range with the two ends', () => {
    expect(rangeLabel(12, 28)).toBe('第 12–28 行');
  });

  it('never says line zero', () => {
    expect(rangeLabel(0, 0)).toBe('第 1 行');
    expect(rangeLabel(-5, 3)).toBe('第 1–3 行');
  });
});

describe('snippetFor', () => {
  it('puts the file and the lines outside the fence and the code inside', () => {
    expect(snippetFor(ask)).toBe(
      '文件 src/main.rs，第 12–14 行：\n```\nlet a = 1;\nlet b = 2;\nlet c = 3;\n```\n\n',
    );
  });

  it('notices a selection that was cut down to the line cap', () => {
    const long = Array.from({ length: MAX_SNIPPET_LINES + 100 }, (_, index) => `line ${String(index)}`);
    const text = snippetFor({ ...ask, fromLine: 1, toLine: long.length, text: long.join('\n') });
    expect(text).toContain('（选区还有 100 行没有附上）');
    expect(text.split('\n').length).toBeLessThan(long.length);
  });

  it('notices a single line that was cut down to the character cap', () => {
    const text = snippetFor({ ...ask, text: 'x'.repeat(40_000) });
    // 一行的选区切掉字符不会少行，所以那句话必须是另一种说法，而不是报 0 行。
    expect(text).toContain('（选区太长，只附上了前一部分）');
    expect(text).not.toContain('0 行没有附上');
  });

  it('leaves a short selection exactly as it was', () => {
    const text = snippetFor({ ...ask, text: '  indented  ' });
    expect(text).toContain('  indented  ');
    expect(text).not.toContain('没有附上');
  });
});

describe('the ask channel', () => {
  it('delivers to every listener and stops after unsubscribing', () => {
    const seen: SelectionAsk[] = [];
    const stop = onAskRequested((value) => seen.push(value));
    askAboutSelection(ask);
    stop();
    askAboutSelection({ ...ask, path: 'other.ts' });
    expect(seen).toEqual([ask]);
  });
});
