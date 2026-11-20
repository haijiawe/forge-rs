// The centre pane's editor: a line-number gutter and a textarea, deliberately
// plain.
//
// This is a placeholder with a contract, not a finished editor. Monaco arrives
// later behind the same three props (`path`, `text`, `onChange`), so nothing
// outside this file may assume a textarea: no selection APIs, no scroll
// position, no syntax highlighting. What is here is only what a reader needs to
// tell one file from another and see its text.
//
// Read-only by default: the first pass is for looking, and an editable box that
// silently discards edits would be worse than one that refuses them.
import { useRef } from 'react';
import type { ChangeEvent, UIEvent } from 'react';

export interface EditorPaneProps {
  path: string;
  text: string;
  onChange: (text: string) => void;
  /** Defaults to true; the wired-but-not-saving first pass. */
  readOnly?: boolean;
  /** One sentence above the text, e.g. that the server truncated the file. */
  notice?: string | null;
}

/**
 * Numbers are capped: a 1 MiB file can be 30k lines, and 30k spans would cost
 * more than the text itself. Past the cap the gutter says so rather than lying
 * about the last line.
 */
const MAX_GUTTER_LINES = 5000;

function lineNumbers(text: string): { numbers: number[]; more: number } {
  const total = text === '' ? 1 : text.split('\n').length;
  const shown = Math.min(total, MAX_GUTTER_LINES);
  const numbers: number[] = [];
  for (let index = 1; index <= shown; index += 1) numbers.push(index);
  return { numbers, more: total - shown };
}

export function EditorPane({ path, text, onChange, readOnly = true, notice = null }: EditorPaneProps) {
  const gutter = useRef<HTMLDivElement | null>(null);
  const { numbers, more } = lineNumbers(text);

  function follow(event: UIEvent<HTMLTextAreaElement>): void {
    // The gutter has no scrollbar of its own, so it is dragged along by the
    // textarea; without this the numbers drift out of line on a long file.
    if (gutter.current !== null) gutter.current.scrollTop = event.currentTarget.scrollTop;
  }

  function edited(event: ChangeEvent<HTMLTextAreaElement>): void {
    onChange(event.currentTarget.value);
  }

  return (
    <div className="wb-editor">
      <div className="wb-editor-path" title={path}>
        {path}
      </div>
      {notice !== null && <p className="wb-editor-notice">{notice}</p>}
      <div className="wb-editor-body">
        <div className="wb-gutter" ref={gutter} aria-hidden="true">
          {numbers.map((number) => (
            <div key={number} className="wb-gutter-line">
              {number}
            </div>
          ))}
          {more > 0 && <div className="wb-gutter-line wb-muted">+{more}</div>}
        </div>
        <textarea
          className="wb-textarea"
          value={text}
          readOnly={readOnly}
          spellCheck={false}
          wrap="off"
          aria-label={`contents of ${path}`}
          onChange={edited}
          onScroll={follow}
        />
      </div>
    </div>
  );
}