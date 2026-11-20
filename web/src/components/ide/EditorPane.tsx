// The centre pane's editor.
//
// Two implementations behind one contract. Monaco is the real one: syntax
// colouring, find, bracket matching, undo. The `<textarea>` is what the pane
// falls back to when the environment cannot host a canvas — jsdom in the test
// suite is the case that exists today. Which one is drawn is a property of the
// environment (`canRunMonaco`), not of a test flag, so the fallback is exercised
// by the same code path a browser without canvas would take.
//
// The contract is the props: `path`, `text`, `onChange`, `onSave`, `readOnly`,
// `notice`. Nothing outside this file may assume Monaco — no selection APIs, no
// scroll position, no `getModel`. That is what lets the fallback be a textarea
// rather than a stub.

import { useEffect, useRef } from 'react';
import type { ChangeEvent, KeyboardEvent as ReactKeyboardEvent, UIEvent } from 'react';
import type { editor as MonacoEditorApi } from 'monaco-editor';
import { canvasAvailableOnce } from './canvas';

export interface EditorPaneProps {
  path: string;
  text: string;
  onChange: (text: string) => void;
  /**
   * Ctrl/Cmd+S. The pane does not save itself: whether a buffer may be written
   * is the store's decision (read-only server, no draft, already saving), and a
   * second place deciding it is a second place to disagree.
   */
  onSave?: () => void;
  /** Defaults to false; the server refuses writes when it was started read-only. */
  readOnly?: boolean;
  /** One sentence above the text, e.g. that the file is truncated or binary. */
  notice?: string | null;
}

/** Numbers are capped: a 1 MiB file can be 30k lines, and 30k spans cost more than the text. */
const MAX_GUTTER_LINES = 5000;

function lineNumbers(text: string): { numbers: number[]; more: number } {
  const total = text === '' ? 1 : text.split('\n').length;
  const shown = Math.min(total, MAX_GUTTER_LINES);
  const numbers: number[] = [];
  for (let index = 1; index <= shown; index += 1) numbers.push(index);
  return { numbers, more: total - shown };
}

/**
 * The `<textarea>` fallback: a gutter and the text, nothing else.
 *
 * Kept because the test suite mounts this component and because a browser
 * without canvas would otherwise get a blank pane. It is honest about being
 * plain — no fake highlighting, no pretending to be Monaco.
 */
function PlainEditor({ path, text, onChange, onSave, readOnly, notice }: EditorPaneProps) {
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

  function keyed(event: ReactKeyboardEvent<HTMLTextAreaElement>): void {
    // The same shortcut Monaco gets, so the fallback is not a different editor
    // with different rules. `preventDefault` matters: the browser's own
    // "save page" dialog would otherwise open over the workspace.
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
      event.preventDefault();
      onSave?.();
    }
  }

  return (
    <div className="wb-editor">
      <div className="wb-editor-path" title={path}>
        {path}
      </div>
      {notice !== null && notice !== undefined && <p className="wb-editor-notice">{notice}</p>}
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
          readOnly={readOnly === true}
          spellCheck={false}
          wrap="off"
          aria-label={`contents of ${path}`}
          onChange={edited}
          onScroll={follow}
          onKeyDown={keyed}
        />
      </div>
    </div>
  );
}

/**
 * Monaco, mounted once per pane and kept across file switches.
 *
 * The editor instance survives a tab change; a new `path` replaces the model
 * and a new `text` from outside (a reload) replaces the value. Rebuilding the
 * editor per file would throw away undo history a reader expects to still be
 * there, and would pay Monaco's construction cost on every click in the tree.
 */
function MonacoEditor({ path, text, onChange, onSave, readOnly, notice }: EditorPaneProps) {
  const host = useRef<HTMLDivElement | null>(null);
  const editor = useRef<MonacoEditorApi.IStandaloneCodeEditor | null>(null);
  // The callbacks live in refs so the editor is built once: re-creating it when
  // a parent re-renders would lose the cursor on every keystroke.
  const change = useRef(onChange);
  const save = useRef(onSave);
  change.current = onChange;
  save.current = onSave;

  useEffect(() => {
    let cancelled = false;
    // Loaded here rather than at module scope so the fallback path never pays
    // for several megabytes of editor it will not use.
    void import('./monacoSetup').then((setup) => {
      if (cancelled || host.current === null) return;
      setup.configureMonaco();
      setup.applyTheme();
      const instance = setup.monaco.editor.create(host.current, {
        value: text,
        language: setup.languageFor(path),
        theme: setup.THEME,
        readOnly: readOnly === true,
        automaticLayout: true,
        minimap: { enabled: false },
        fontSize: 13,
        lineHeight: 20,
        scrollBeyondLastLine: false,
        renderWhitespace: 'selection',
        tabSize: 2,
        wordWrap: 'off',
      });
      editor.current = instance;
      instance.onDidChangeModelContent(() => {
        change.current(instance.getValue());
      });
      instance.addCommand(
        // Ctrl on Windows/Linux, Cmd on macOS. Taken from Monaco's own enums
        // rather than as the packed integer, so a change on their side is a
        // compile error here rather than a shortcut that silently stops working.
        setup.monaco.KeyMod.CtrlCmd | setup.monaco.KeyCode.KeyS,
        () => {
          save.current?.();
        },
      );
    });
    return () => {
      cancelled = true;
      editor.current?.dispose();
      editor.current = null;
    };
    // Built once. Path, text and readOnly are applied by the effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A file switch replaces the model, so undo history and language follow.
  useEffect(() => {
    const instance = editor.current;
    if (instance === null) return;
    instance.setValue(text);
    void import('./monacoSetup').then((setup) => {
      const model = instance.getModel();
      if (model === null) return;
      setup.monaco.editor.setModelLanguage(model, setup.languageFor(path));
    });
    // `text` deliberately absent: an external text change is handled below, and
    // re-setting the value on every keystroke would fight the editor's own undo
    // stack (the value already equals what the user typed).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path]);

  // An external change (a reload after a conflict) replaces the buffer.
  useEffect(() => {
    const instance = editor.current;
    if (instance === null) return;
    if (instance.getValue() === text) return;
    instance.setValue(text);
  }, [text]);

  useEffect(() => {
    editor.current?.updateOptions({ readOnly: readOnly === true });
  }, [readOnly]);

  return (
    <div className="wb-editor">
      <div className="wb-editor-path" title={path}>
        {path}
      </div>
      {notice !== null && notice !== undefined && <p className="wb-editor-notice">{notice}</p>}
      <div className="wb-editor-monaco" ref={host} aria-label={`contents of ${path}`} />
    </div>
  );
}

export function EditorPane(props: EditorPaneProps) {
  // Resolved once per mount: the probe allocates a canvas, and the answer cannot
  // change while a pane is open.
  const monaco = useRef<boolean | null>(null);
  if (monaco.current === null) monaco.current = canvasAvailableOnce();
  return monaco.current ? <MonacoEditor {...props} /> : <PlainEditor {...props} />;
}