// Block renderer. Each block is its own memoized component, so a streaming row
// re-renders only the tail block that changed, not the whole message.
import { memo, useMemo, type ReactNode } from 'react';
import { parseBlocksCached, type Block } from '../markdown/parse';

const SAFE_URL = /^(https?:|mailto:|\/|#)/i;

/** Split one line into text, `code`, **bold**, *italic* and [links](url). */
export function inlineNodes(text: string, keyPrefix = 'i'): ReactNode[] {
  const nodes: ReactNode[] = [];
  const pattern = /(`[^`]*`)|(\*\*[^*]+\*\*)|(__[^_]+__)|(\*[^*\n]+\*)|(_[^_\n]+_)|(\[[^\]]+\]\([^)\s]+\))/g;
  let cursor = 0;
  let match: RegExpExecArray | null;
  let at = 0;
  while ((match = pattern.exec(text)) !== null) {
    if (match.index > cursor) nodes.push(text.slice(cursor, match.index));
    const token = match[0];
    const key = `${keyPrefix}-${String(at)}`;
    at += 1;
    if (token.startsWith('`')) {
      nodes.push(
        <code key={key} className="md-code">
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith('**') || token.startsWith('__')) {
      nodes.push(<strong key={key}>{token.slice(2, -2)}</strong>);
    } else if (token.startsWith('[')) {
      const split = token.indexOf('](');
      const label = token.slice(1, split);
      const href = token.slice(split + 2, -1);
      nodes.push(
        SAFE_URL.test(href) ? (
          <a key={key} href={href} target="_blank" rel="noreferrer noopener">
            {label}
          </a>
        ) : (
          <span key={key}>{label}</span>
        ),
      );
    } else {
      nodes.push(<em key={key}>{token.slice(1, -1)}</em>);
    }
    cursor = match.index + token.length;
  }
  if (cursor < text.length) nodes.push(text.slice(cursor));
  return nodes;
}

/** Everything after the first newline of a paragraph keeps its line breaks. */
function textWithBreaks(text: string, keyPrefix: string): ReactNode[] {
  const out: ReactNode[] = [];
  text.split('\n').forEach((line, index) => {
    if (index > 0) out.push(<br key={`${keyPrefix}-br-${String(index)}`} />);
    out.push(...inlineNodes(line, `${keyPrefix}-${String(index)}`));
  });
  return out;
}

const BlockView = memo(function BlockView({ block }: { block: Block }) {
  switch (block.kind) {
    case 'heading': {
      const Tag = `h${String(block.level)}` as 'h1' | 'h2' | 'h3' | 'h4' | 'h5' | 'h6';
      return <Tag className="md-heading">{inlineNodes(block.text)}</Tag>;
    }
    case 'paragraph':
      return <p className="md-paragraph">{textWithBreaks(block.text, 'p')}</p>;
    case 'code':
      return (
        <pre className={`md-pre${block.open ? ' md-pre-open' : ''}`}>
          <code className="md-codeblock">{block.code}</code>
        </pre>
      );
    case 'list': {
      const items = block.items.map((item, index) => (
        <li key={`li-${String(index)}`}>{inlineNodes(item, `li-${String(index)}`)}</li>
      ));
      return block.ordered ? <ol className="md-list">{items}</ol> : <ul className="md-list">{items}</ul>;
    }
    case 'quote':
      return <blockquote className="md-quote">{textWithBreaks(block.text, 'q')}</blockquote>;
    case 'rule':
      return <hr className="md-rule" />;
  }
});

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  // Parsing is cached by exact source, so a re-render of unchanged text is free.
  const blocks = useMemo(() => parseBlocksCached(text), [text]);
  return (
    <div className="markdown">
      {blocks.map((block, index) => (
        <BlockView key={`${String(index)}-${block.kind}`} block={block} />
      ))}
    </div>
  );
});