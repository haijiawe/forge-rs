import { describe, expect, it } from 'vitest';
import { countLines, parseChanged, rangesOf, touches } from './changed';

function hunk(header: string, body: string[]): string {
  return [header, ...body].join('\n');
}

describe('parseChanged', () => {
  it('reads one hunk and marks only the added lines', () => {
    const body = [
      'diff --git a/src/main.rs b/src/main.rs',
      'index 1111111..2222222 100644',
      '--- a/src/main.rs',
      '+++ b/src/main.rs',
      hunk('@@ -3,3 +3,4 @@ fn main() {', [
        ' let a = 1;',
        ' let b = 2;',
        '+let c = 3;',
        ' let d = 4;',
      ]),
      '',
    ].join('\n');
    const answer = parseChanged(body);
    expect(answer.seen).toBe(true);
    expect(answer.added).toBe(1);
    expect(answer.removed).toBe(0);
    expect(answer.ranges).toEqual([{ from: 5, to: 5 }]);
  });

  it('keeps two hunks as two ranges', () => {
    const body = [
      hunk('@@ -1,2 +1,2 @@', [' one', '-two', '+TWO']),
      hunk('@@ -20,2 +20,4 @@', [' twenty', '+twenty one', '+twenty two', ' twenty three']),
    ].join('\n');
    const answer = parseChanged(body);
    expect(answer.ranges).toEqual([
      { from: 2, to: 2 },
      { from: 21, to: 22 },
    ]);
    expect(answer.added).toBe(3);
    expect(answer.removed).toBe(1);
  });

  it('a replaced line is one added line at the same number', () => {
    const body = hunk('@@ -7,1 +7,1 @@', ['-old', '+new']);
    const answer = parseChanged(body);
    expect(answer.ranges).toEqual([{ from: 7, to: 7 }]);
    expect(answer.added).toBe(1);
    expect(answer.removed).toBe(1);
  });

  it('a deletion-only hunk records the removal and marks nothing', () => {
    const body = hunk('@@ -4,3 +4,1 @@', [' keep', '-gone', '-also gone', ' keep too']);
    const answer = parseChanged(body);
    expect(answer.seen).toBe(true);
    expect(answer.added).toBe(0);
    expect(answer.removed).toBe(2);
    expect(answer.ranges).toEqual([]);
  });

  it('counts a hunk header that leaves the count out', () => {
    const body = hunk('@@ -1 +1 @@', ['-a', '+b']);
    expect(parseChanged(body).ranges).toEqual([{ from: 1, to: 1 }]);
  });

  it('reads a new file from the zero-width old side', () => {
    const body = hunk('@@ -0,0 +1,3 @@', ['+one', '+two', '+three']);
    const answer = parseChanged(body);
    expect(answer.ranges).toEqual([{ from: 1, to: 3 }]);
    expect(answer.added).toBe(3);
  });

  it('does not let the no-newline note shift the numbering', () => {
    const body = hunk('@@ -1,2 +1,2 @@', [' one', '-two', '+TWO', '\\ No newline at end of file']);
    expect(parseChanged(body).ranges).toEqual([{ from: 2, to: 2 }]);
  });

  it('a deleted line that looks like a file header stays inside the hunk', () => {
    // The line was `-- a banner` in the old file, so the diff writes `--- a banner`.
    // Ending the hunk there would drop the addition that follows it.
    const body = hunk('@@ -2,2 +2,2 @@', ['-- a banner', '+// a banner', ' keep']);
    const answer = parseChanged(body);
    expect(answer.seen).toBe(true);
    expect(answer.removed).toBe(1);
    expect(answer.ranges).toEqual([{ from: 2, to: 2 }]);
  });

  it('says nothing rather than "no changes" when the body is not a line diff', () => {
    expect(parseChanged('').seen).toBe(false);
    expect(parseChanged('Binary files a/x and b/x differ\n').seen).toBe(false);
  });

  it('stops at meta lines after a hunk instead of counting them as content', () => {
    const body = [
      hunk('@@ -1,1 +1,2 @@', [' one', '+two']),
      'diff --git a/b b/b',
      'index 3333333..4444444 100644',
    ].join('\n');
    const answer = parseChanged(body);
    expect(answer.added).toBe(1);
    expect(answer.ranges).toEqual([{ from: 2, to: 2 }]);
  });
});

describe('rangesOf', () => {
  it('merges consecutive numbers and splits the rest', () => {
    expect(rangesOf([3, 4, 5, 9, 11, 12])).toEqual([
      { from: 3, to: 5 },
      { from: 9, to: 9 },
      { from: 11, to: 12 },
    ]);
  });

  it('an empty list is no ranges', () => {
    expect(rangesOf([])).toEqual([]);
  });
});

describe('countLines and touches', () => {
  const ranges = [
    { from: 2, to: 4 },
    { from: 10, to: 10 },
  ];

  it('counts the lines the ranges cover, not the ranges', () => {
    expect(countLines(ranges)).toBe(4);
  });

  it('answers whether a line is in any range', () => {
    expect(touches(ranges, 1)).toBe(false);
    expect(touches(ranges, 2)).toBe(true);
    expect(touches(ranges, 4)).toBe(true);
    expect(touches(ranges, 5)).toBe(false);
    expect(touches(ranges, 10)).toBe(true);
  });
});
