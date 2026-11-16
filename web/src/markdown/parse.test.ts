import { describe, expect, it } from 'vitest';
import { parseBlocks, parseBlocksCached } from './parse';

describe('parseBlocks', () => {
  it('splits paragraphs, headings and rules', () => {
    const blocks = parseBlocks('# title\n\nsome text\nstill text\n\n---\n\n## second');
    expect(blocks).toEqual([
      { kind: 'heading', level: 1, text: 'title' },
      { kind: 'paragraph', text: 'some text\nstill text' },
      { kind: 'rule' },
      { kind: 'heading', level: 2, text: 'second' },
    ]);
  });

  it('keeps a fenced code block verbatim, including markdown characters', () => {
    const blocks = parseBlocks('before\n```ts\nconst x = "# not a heading";\n```\nafter');
    expect(blocks[0]).toEqual({ kind: 'paragraph', text: 'before' });
    expect(blocks[1]).toEqual({ kind: 'code', lang: 'ts', code: 'const x = "# not a heading";', open: false });
    expect(blocks[2]).toEqual({ kind: 'paragraph', text: 'after' });
  });

  it('marks an unterminated fence as open so streaming code is not re-parsed', () => {
    const blocks = parseBlocks('text\n```go\nfunc main() {');
    const code = blocks[1];
    expect(code).toMatchObject({ kind: 'code', lang: 'go', open: true });
  });

  it('parses ordered and unordered lists and stops at a blank line', () => {
    const blocks = parseBlocks('- one\n- two\n\n1. first\n2. second');
    expect(blocks[0]).toEqual({ kind: 'list', ordered: false, items: ['one', 'two'] });
    expect(blocks[1]).toEqual({ kind: 'list', ordered: true, items: ['first', 'second'] });
  });

  it('parses block quotes', () => {
    const blocks = parseBlocks('> quoted line\n> second line');
    expect(blocks[0]).toEqual({ kind: 'quote', text: 'quoted line\nsecond line' });
  });

  it('returns one paragraph for plain text and nothing for an empty string', () => {
    expect(parseBlocks('hello')).toEqual([{ kind: 'paragraph', text: 'hello' }]);
    expect(parseBlocks('')).toEqual([]);
  });

  it('caches by source so an unchanged block list is not rebuilt', () => {
    const first = parseBlocksCached('## stable');
    const second = parseBlocksCached('## stable');
    expect(second).toBe(first);
    expect(parseBlocksCached('## other')).not.toBe(first);
  });
});