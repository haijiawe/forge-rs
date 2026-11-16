// A small block-level Markdown parser.
//
// Requirements it serves (docs/02-architecture.md §10.1.3): render markdown per
// block and never re-parse the whole transcript while streaming. So parsing is
// pure and cached by source text, and the caller parses only the blocks of a
// row whose text actually changed.
//
// It is deliberately a subset: headings, paragraphs, fenced code, lists,
// blockquotes and rules. No syntax highlighting in M3, no raw HTML (React
// escapes everything), no tables. An unterminated fence is rendered as code:
// that is what a streamed code block looks like for the half second before its
// closing fence arrives, and re-rendering it as plain text would flicker.

export type Block =
  | { kind: 'paragraph'; text: string }
  | { kind: 'heading'; level: 1 | 2 | 3 | 4 | 5 | 6; text: string }
  | { kind: 'code'; lang: string; code: string; open: boolean }
  | { kind: 'list'; ordered: boolean; items: string[] }
  | { kind: 'quote'; text: string }
  | { kind: 'rule' };

const FENCE = /^\s*(```+|~~~+)\s*([\w+#.-]*)\s*$/;
const HEADING = /^(#{1,6})\s+(.*)$/;
const RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^\s*>\s?(.*)$/;
const LIST_ITEM = /^\s*(?:([-*+])|(\d{1,9})[.)])\s+(.*)$/;

function pushParagraph(blocks: Block[], lines: string[]): void {
  if (lines.length === 0) return;
  blocks.push({ kind: 'paragraph', text: lines.join('\n') });
  lines.length = 0;
}

export function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, '\n').split('\n');
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i] ?? '';

    const fence = FENCE.exec(line);
    if (fence !== null) {
      pushParagraph(blocks, paragraph);
      const marker = fence[1] ?? '```';
      const lang = fence[2] ?? '';
      const code: string[] = [];
      i += 1;
      let closed = false;
      while (i < lines.length) {
        const inner = lines[i] ?? '';
        if (inner.trimStart().startsWith(marker)) {
          closed = true;
          i += 1;
          break;
        }
        code.push(inner);
        i += 1;
      }
      blocks.push({ kind: 'code', lang, code: code.join('\n'), open: !closed });
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading !== null) {
      pushParagraph(blocks, paragraph);
      const level = Math.min(6, Math.max(1, (heading[1] ?? '#').length)) as 1 | 2 | 3 | 4 | 5 | 6;
      blocks.push({ kind: 'heading', level, text: heading[2] ?? '' });
      i += 1;
      continue;
    }

    if (RULE.test(line)) {
      pushParagraph(blocks, paragraph);
      blocks.push({ kind: 'rule' });
      i += 1;
      continue;
    }

    const quote = QUOTE.exec(line);
    if (quote !== null) {
      pushParagraph(blocks, paragraph);
      const quoted: string[] = [quote[1] ?? ''];
      i += 1;
      while (i < lines.length) {
        const next = QUOTE.exec(lines[i] ?? '');
        if (next === null) break;
        quoted.push(next[1] ?? '');
        i += 1;
      }
      blocks.push({ kind: 'quote', text: quoted.join('\n') });
      continue;
    }

    const item = LIST_ITEM.exec(line);
    if (item !== null) {
      pushParagraph(blocks, paragraph);
      const ordered = item[2] !== undefined;
      const items: string[] = [item[3] ?? ''];
      i += 1;
      while (i < lines.length) {
        const next = LIST_ITEM.exec(lines[i] ?? '');
        if (next === null) break;
        if ((next[2] !== undefined) !== ordered) break;
        items.push(next[3] ?? '');
        i += 1;
      }
      blocks.push({ kind: 'list', ordered, items });
      continue;
    }

    if (line.trim() === '') {
      pushParagraph(blocks, paragraph);
      i += 1;
      continue;
    }

    paragraph.push(line);
    i += 1;
  }

  pushParagraph(blocks, paragraph);
  return blocks;
}

// Parsing the same text twice is common (a re-render of an unchanged row), so
// the cache is keyed by the exact source. It is bounded: streaming produces a
// new string per frame and an unbounded cache would be a leak.
const CACHE_LIMIT = 256;
const cache = new Map<string, Block[]>();

export function parseBlocksCached(source: string): Block[] {
  const hit = cache.get(source);
  if (hit !== undefined) return hit;
  const blocks = parseBlocks(source);
  if (cache.size >= CACHE_LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  cache.set(source, blocks);
  return blocks;
}