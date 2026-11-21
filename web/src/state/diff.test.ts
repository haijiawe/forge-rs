import { describe, expect, it } from 'vitest';

import { parseHunkHeader, sideBySide } from './diff';

const SAMPLE = [
  'diff --git a/src/main.rs b/src/main.rs',
  'index 1111111..2222222 100644',
  '--- a/src/main.rs',
  '+++ b/src/main.rs',
  '@@ -1,3 +1,4 @@',
  ' fn main() {',
  '-    let x = 1;',
  '-    let y = 2;',
  '+    let x = 10;',
  '+    let y = 20;',
  '+    let z = 30;',
  ' }',
  '',
].join('\n');

describe('sideBySide', () => {
  it('把一段里删掉的行与加上的行按顺序配成左右两列', () => {
    const rows = sideBySide(SAMPLE);
    const pairs = rows.filter((row) => row.right !== null);
    // 文件头 4 行 + hunk 头各自成行，配对的只有内容行
    expect(rows[0]?.left.kind).toBe('meta');
    expect(rows[0]?.right).toBeNull();
    expect(rows.find((row) => row.left.kind === 'hunk')?.left.text).toBe(
      '@@ -1,3 +1,4 @@',
    );

    const dels = pairs.filter((row) => row.left.kind === 'del');
    const adds = pairs.filter((row) => row.right?.kind === 'add');
    expect(dels.map((row) => row.left.text)).toEqual(['    let x = 1;', '    let y = 2;']);
    expect(adds.map((row) => row.right?.text)).toEqual([
      '    let x = 10;',
      '    let y = 20;',
      '    let z = 30;',
    ]);
  });

  it('两列的行号与 git diff 的 hunk 头对得上', () => {
    const rows = sideBySide(SAMPLE);
    const first = rows.find((row) => row.left.kind === 'del');
    expect(first?.left.no).toBe(2);
    expect(first?.right?.no).toBe(2);
    // 第三对只有右侧有内容：左边是空白，不编造行号
    const third = rows.filter((row) => row.right?.kind === 'add')[2];
    expect(third?.left.kind).toBe('blank');
    expect(third?.left.no).toBeNull();
    expect(third?.right?.no).toBe(4);
  });

  it('上下文行两侧各一份，行号一起推进', () => {
    const rows = sideBySide(SAMPLE);
    const context = rows.filter((row) => row.left.kind === 'context');
    expect(context.map((row) => [row.left.no, row.right?.no])).toEqual([
      [1, 1],
      [4, 5],
    ]);
  });

  it('多出来的删除行对面是空白，而不是被丢掉', () => {
    const text = ['@@ -1,2 +1,1 @@', '-one', '-two', '+uno', ' tail', ''].join('\n');
    const rows = sideBySide(text).filter((row) => row.right !== null);
    expect(rows.map((row) => [row.left.kind, row.right?.kind])).toEqual([
      ['del', 'add'],
      ['del', 'blank'],
      ['context', 'context'],
    ]);
  });

  it('没有换行的文件末尾标记横跨两列，不参与配对', () => {
    const text = ['@@ -1 +1 @@', '-a', '+b', '\\ No newline at end of file', ''].join('\n');
    const rows = sideBySide(text);
    const marker = rows.find((row) => row.left.text.startsWith('\\ '));
    expect(marker?.right).toBeNull();
    expect(rows.filter((row) => row.right !== null).length).toBe(1);
  });

  it('空文本给出空结果，而不是一行空内容', () => {
    expect(sideBySide('')).toEqual([]);
  });
});

describe('parseHunkHeader', () => {
  it('认得带计数的头', () => {
    expect(parseHunkHeader('@@ -12,3 +12,4 @@ fn f()')).toEqual({ left: 12, right: 12 });
  });

  it('认得省略计数的头', () => {
    expect(parseHunkHeader('@@ -7 +8 @@')).toEqual({ left: 7, right: 8 });
  });

  it('不是 hunk 头就是 null', () => {
    expect(parseHunkHeader('--- a/x')).toBeNull();
    expect(parseHunkHeader('+added')).toBeNull();
  });
});