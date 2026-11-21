// 统一 diff 文本 → 并排两列。
//
// 服务端发的是 `git diff` 写出来的统一格式，这里**不重新判断改动**，只是把
// 同一段 hunk 里的 `-` 行与 `+` 行按出现顺序对齐成左右两列。两个理由：
//
//   * 解析器不该有第二套语义。谁改了哪一行由 git 决定，这里只决定「摆在哪」。
//   * 对齐是呈现，不是判定。配对失败的剩余行照样出现在自己那一列，留白比
//     「假装它们是一对」诚实——一行被拆成两行时，并排视图本来就没有正确答案。
//
// 行号取自 hunk 头 `@@ -12,3 +12,4 @@`，所以两列显示的号码与 `git diff` 一致。

export type SideKind = 'context' | 'del' | 'add' | 'blank' | 'meta' | 'hunk';

export interface SideCell {
  kind: SideKind;
  /** 文件里的行号；meta/hunk/blank 没有 */
  no: number | null;
  text: string;
}

export interface SideRow {
  /** meta 与 hunk 行横跨两列，此时 left 有值、right 为 null */
  left: SideCell;
  right: SideCell | null;
}

const HUNK = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/** 把一个 hunk 头解析成左右起始行号；不是 hunk 头则返回 null。 */
export function parseHunkHeader(line: string): { left: number; right: number } | null {
  const match = HUNK.exec(line);
  if (match === null) return null;
  const left = Number(match[1]);
  const right = Number(match[3]);
  if (!Number.isFinite(left) || !Number.isFinite(right)) return null;
  return { left, right };
}

function cell(kind: SideKind, no: number | null, text: string): SideCell {
  return { kind, no, text };
}

/** 一行是否为「让两列都清空」的行：文件头、index 行、二进制标记等。 */
function isMeta(line: string): boolean {
  return (
    line.startsWith('diff ') ||
    line.startsWith('index ') ||
    line.startsWith('--- ') ||
    line.startsWith('+++ ') ||
    line.startsWith('new file') ||
    line.startsWith('deleted file') ||
    line.startsWith('old mode') ||
    line.startsWith('new mode') ||
    line.startsWith('similarity index') ||
    line.startsWith('rename ') ||
    line.startsWith('Binary ') ||
    line.startsWith('\\ ')
  );
}

/**
 * 把统一 diff 摆成两列。
 *
 * `-` 与 `+` 的连续段按顺序配对；多出来的行落在自己那一列，对面是空单元格。
 * 上下文行两列各一份。hunk 头与文件头横跨两列。
 */
export function sideBySide(text: string): SideRow[] {
  const lines = text.replace(/\n$/, '').split('\n');
  if (text === '') return [];
  const rows: SideRow[] = [];
  let leftNo = 0;
  let rightNo = 0;

  // 一个连续段的缓冲：先把 `-` 攒起来，遇到 `+` 或别的行再结算。
  let removed: string[] = [];
  let added: string[] = [];

  const flush = (): void => {
    const pairs = Math.max(removed.length, added.length);
    for (let i = 0; i < pairs; i += 1) {
      const gone = removed[i];
      const came = added[i];
      rows.push({
        left:
          gone === undefined
            ? cell('blank', null, '')
            : cell('del', leftNo++, gone),
        right:
          came === undefined
            ? cell('blank', null, '')
            : cell('add', rightNo++, came),
      });
    }
    removed = [];
    added = [];
  };

  for (const line of lines) {
    if (isMeta(line)) {
      flush();
      rows.push({ left: cell('meta', null, line), right: null });
      continue;
    }
    const hunk = parseHunkHeader(line);
    if (hunk !== null) {
      flush();
      leftNo = hunk.left;
      rightNo = hunk.right;
      rows.push({ left: cell('hunk', null, line), right: null });
      continue;
    }
    if (line.startsWith('-')) {
      removed.push(line.slice(1));
      continue;
    }
    if (line.startsWith('+')) {
      added.push(line.slice(1));
      continue;
    }
    // 上下文行（以空格开头）或空串：两侧各有同一行。
    flush();
    const body = line.startsWith(' ') ? line.slice(1) : line;
    rows.push({
      left: cell('context', leftNo++, body),
      right: cell('context', rightNo++, body),
    });
  }
  flush();
  return rows;
}