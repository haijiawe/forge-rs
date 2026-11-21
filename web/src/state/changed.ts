// 从 git 的统一 diff 里读出「新文件哪些行是新的」。
//
// 只用第一字符和 hunk 头：diff 的其余部分（重命名元数据、二进制提示、
// 模式变更）不描述行，硬解析它等于把 git 的所有边角情况都接过来。
//
// 这里说的是「磁盘与提交版本不同」，不是「agent 改的」。日志里 file/write 记的是
// 摘要不是内容，所以谁能写哪几行在服务端就合并了；一个把人的编辑算成 agent
// 改动的界面，比一个不标更糟。
export interface ChangedRange {
  /** 1-based, inclusive. */
  from: number;
  /** 1-based, inclusive. */
  to: number;
}

export interface ChangedLines {
  /** Lines that exist on disk and not in the commit, as ascending ranges. */
  ranges: readonly ChangedRange[];
  added: number;
  removed: number;
  /**
   * True once a hunk header has been read. False means "this is not a line diff
   * this reader understands" — an empty change, a binary file, or a body it
   * could not follow — and the caller must not draw it as "no changes".
   */
  seen: boolean;
}

const EMPTY: ChangedLines = { ranges: [], added: 0, removed: 0, seen: false };

function hunkHeader(line: string): { newStart: number } | null {
  // @@ -1,3 +1,4 @@ optional section heading
  const match = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@/.exec(line);
  if (match === null) return null;
  const start = Number(match[1]);
  return Number.isFinite(start) ? { newStart: start } : null;
}

/**
 * Read one file's unified diff.
 *
 * A hunk's body is walked with a moving new-file line number: `+` consumes one and
 * is recorded, `-` consumes none, and context consumes one. That is the whole
 * grammar this needs; anything else (the `\ No newline at end of file` note) is
 * skipped rather than guessed at.
 */
export function parseChanged(text: string): ChangedLines {
  if (text.trim() === '') return EMPTY;
  const lines = text.split('\n');
  const numbers: number[] = [];
  let added = 0;
  let removed = 0;
  let seen = false;
  let newLine = 0;
  let inHunk = false;
  for (const line of lines) {
    if (!inHunk) {
      // Headers are only headers outside a hunk. Inside one, a deleted line whose
      // text begins with `-- ` renders as `--- `, and treating that as a file
      // header would silently drop the rest of the hunk.
      if (line.startsWith('@@')) {
        const header = hunkHeader(line);
        if (header === null) continue;
        seen = true;
        inHunk = true;
        newLine = header.newStart;
      }
      continue;
    }
    if (line.startsWith('@@')) {
      const header = hunkHeader(line);
      if (header === null) {
        inHunk = false;
        continue;
      }
      newLine = header.newStart;
      continue;
    }
    if (line.startsWith('+')) {
      numbers.push(newLine);
      added += 1;
      newLine += 1;
      continue;
    }
    if (line.startsWith('-')) {
      removed += 1;
      continue;
    }
    if (line.startsWith('\\')) continue;
    if (line === '' || line.startsWith(' ')) {
      // Context consumes a line of the new file but is not a change.
      newLine += 1;
      continue;
    }
    // Anything else (`diff --git`, `index `, a trailing trailer) ends the hunk.
    inHunk = false;
  }
  return { ranges: rangesOf(numbers), added, removed, seen };
}

/** Consecutive numbers become one range, so the editor draws one decoration each. */
export function rangesOf(numbers: readonly number[]): ChangedRange[] {
  const ranges: ChangedRange[] = [];
  let from = 0;
  let to = 0;
  for (const number of numbers) {
    if (from === 0) {
      from = number;
      to = number;
      continue;
    }
    if (number === to + 1) {
      to = number;
      continue;
    }
    ranges.push({ from, to });
    from = number;
    to = number;
  }
  if (from !== 0) ranges.push({ from, to });
  return ranges;
}

/** How many lines the ranges cover, for a sentence like "有 12 行不同". */
export function countLines(ranges: readonly ChangedRange[]): number {
  return ranges.reduce((total, range) => total + (range.to - range.from + 1), 0);
}

/** True when `line` falls inside one of the ranges. */
export function touches(ranges: readonly ChangedRange[], line: number): boolean {
  return ranges.some((range) => line >= range.from && line <= range.to);
}
