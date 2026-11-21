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

import { useEffect, useRef, useState } from 'react';
import type {
  ChangeEvent,
  KeyboardEvent as ReactKeyboardEvent,
  ReactElement,
  UIEvent,
} from 'react';
import type { editor as MonacoEditorApi } from 'monaco-editor';
import { canvasAvailableOnce } from './canvas';
import { rangeLabel } from '../../state/ask';
import type { SelectionAsk } from '../../state/ask';
import { touches } from '../../state/changed';
import type { ChangedLines } from '../../state/changed';

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
  /**
   * A line to scroll to and highlight, from a search hit.
   *
   * The parent only passes this when the reveal belongs to the path on screen —
   * a line number carried across a file switch would jump the reader somewhere
   * they never asked to be.
   */
  revealLine?: number | null;
  /** Called once the reveal has been applied, so the parent can forget it. */
  onRevealed?: () => void;
  /**
   * 把选中的一段交给 agent。
   *
   * 面板只负责报告「哪段、在哪」，怎么送到输入框、要不要清空，是父组件的
   * 事：编辑器不该知道会话、输入框、消息这些东西的存在。
   */
  onAskSelection?: (ask: SelectionAsk) => void;
  /**
   * The lines of this file that exist on disk and not in the commit.
   *
   * Null means "nobody asked", which draws nothing: an empty set would be a claim
   * about the file, and the pane has not read a diff when this is null.
   */
  changed?: ChangedLines | null;
  /** True when the server cut the diff short, so the marks may be incomplete. */
  changedTruncated?: boolean;
  /** Put this file back to its committed bytes. */
  onRevertFile?: () => void;
  /** True while that restore is in flight. */
  reverting?: boolean;
}

/** Numbers are capped: a 1 MiB file can be 30k lines, and 30k spans cost more than the text. */
const MAX_GUTTER_LINES = 5000;

/**
 * The sentence above the text that says how this file differs from the commit.
 *
 * It names the comparison, not the author. The log stores digests of what was
 * written, not the text, so the server cannot say which of these lines came from
 * the agent and which from the person typing; a bar that said "the agent changed
 * these" would be guessing about someone else's work. What it can state exactly is
 * what git already knows: this file's bytes and the committed version differ here.
 */
function ChangedBar({
  changed,
  truncated,
  onRevert,
  reverting,
}: {
  changed?: ChangedLines | null;
  truncated?: boolean;
  onRevert?: () => void;
  reverting?: boolean;
}): ReactElement | null {
  if (changed === null || changed === undefined) return null;
  if (!changed.seen) return null;
  if (changed.added === 0 && changed.removed === 0 && !truncated) return null;
  const title =
    '与 git 提交的版本比较：磁盘上新增或改写的行为橙色，' +
    '提交里有而这个文件已经没有的行不在这里（看「改动」面板）。' +
    '日志记的是摘要不是内容，所以这里分不出哪几行是 agent 写的、哪几行是你自己写的。';
  return (
    <div className="wb-changed" title={title}>
      <span className="wb-changed-count">
        与提交版本相比：+{changed.added} −{changed.removed}
      </span>
      {truncated === true && <span className="wb-muted">（服务端截断了这份改动，标记可能不全）</span>}
      {onRevert !== undefined && (
        <button
          type="button"
          className="wb-changed-revert"
          disabled={reverting === true}
          title="把磁盘上的这个文件退回 git 里已提交的字节；只动这一个文件"
          onClick={onRevert}
        >
          {reverting === true ? '还原中…' : '撤销这个文件'}
        </button>
      )}
    </div>
  );
}

/** 一个选区，外加「问 agent」浮层该落在编辑器的哪个位置。 */
interface MonacoPick extends SelectionAsk {
  top: number;
  left: number;
}

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
function PlainEditor({
  path,
  text,
  onChange,
  onSave,
  readOnly,
  notice,
  revealLine,
  onRevealed,
  onAskSelection,
  changed,
  changedTruncated,
  onRevertFile,
  reverting,
}: EditorPaneProps) {
  const gutter = useRef<HTMLDivElement | null>(null);
  const area = useRef<HTMLTextAreaElement | null>(null);
  const { numbers, more } = lineNumbers(text);
  // 选区在这里只有 select 一个事件，行号要自己从光标位置数出来；Monaco 那条
  // 路有 selection 对象，但两条路对上层的说法必须是同一个（见 props）。
  const [pick, setPick] = useState<SelectionAsk | null>(null);

  function selected(area: HTMLTextAreaElement): void {
    const start = area.selectionStart ?? 0;
    const end = area.selectionEnd ?? 0;
    if (end <= start) {
      setPick(null);
      return;
    }
    // 发给 agent 的是整行，不是从鼠标落点开始的半截：按钮上写的是「第 A–B 行」，
    // 内容就必须是那几行。在行中间松手时，光标位置只说明选区从哪个字符起，
    // 拿它当起点会切出 `t line5 = 5;` 这种半个词。
    const lineStart = area.value.lastIndexOf('\n', start - 1) + 1;
    const breakAfter = area.value.indexOf('\n', end);
    const lineEnd = breakAfter === -1 ? area.value.length : breakAfter;
    const picked = area.value.slice(lineStart, lineEnd);
    const fromLine = 1 + (area.value.slice(0, lineStart).match(/\n/g)?.length ?? 0);
    const toLine = fromLine + (picked.match(/\n/g)?.length ?? 0);
    setPick({ path, fromLine, toLine, text: picked });
  }

  useEffect(() => {
    if (revealLine === null || revealLine === undefined) return;
    const target = area.current;
    if (target === null) return;
    // An empty buffer has no line to show. The pane is only drawn once a read
    // has answered, so this is the empty-file case, not a loading one.
    if (text === '') return;
    // The fallback cannot highlight a line (it would mean splitting the value into
    // spans, and a `<textarea>` has no spans). Scrolling to it is the part that
    // matters — the reader lands where the match is.
    const lineHeight = 20;
    target.scrollTop = Math.max(0, (revealLine - 1) * lineHeight - lineHeight * 2);
    if (gutter.current !== null) gutter.current.scrollTop = target.scrollTop;
    onRevealed?.();
  }, [revealLine, onRevealed, text]);

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
      <ChangedBar
        changed={changed ?? null}
        truncated={changedTruncated}
        onRevert={onRevertFile}
        reverting={reverting}
      />
      {notice !== null && notice !== undefined && <p className="wb-editor-notice">{notice}</p>}
      {pick !== null && (
        <button
          type="button"
          className="wb-ask-chip wb-ask-chip-bar"
          title="把选中的代码放进输入框，后面接着写你要它做什么"
          // 按下不抢焦点：抢走之后光标不在输入框里，人接着敲的头几个字会掉进
          // 空处（真机上实测：点完按钮，焦点停在按钮上）。
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => {
            onAskSelection?.(pick);
          }}
        >
          问 agent（{rangeLabel(pick.fromLine, pick.toLine)}）
        </button>
      )}
      <div className="wb-editor-body">
        <div className="wb-gutter" ref={gutter} aria-hidden="true">
          {numbers.map((number) => (
            // 兜底编辑器画不了整行背景，能画的是行号：改过的行号带一个色条，
            // 于是「哪些行是新的」在这条路上仍然看得见，只是更朴素。
            <div
              key={number}
              className={
                changed !== null && changed !== undefined && touches(changed.ranges, number)
                  ? 'wb-gutter-line wb-gutter-changed'
                  : 'wb-gutter-line'
              }
            >
              {number}
            </div>
          ))}
          {more > 0 && <div className="wb-gutter-line wb-muted">+{more}</div>}
        </div>
        <textarea
          className="wb-textarea"
          ref={area}
          value={text}
          readOnly={readOnly === true}
          spellCheck={false}
          wrap="off"
          aria-label={`contents of ${path}`}
          onChange={edited}
          onScroll={follow}
          onSelect={(event) => {
            selected(event.currentTarget);
          }}
          // 拖动和 Shift+方向键也会改选区，而 onSelect 不是每个宿主都会为这两条
          // 路径触发（jsdom 就不会）。同一个处理函数挂三处，代价只有一次重算。
          onMouseUp={(event) => {
            selected(event.currentTarget);
          }}
          onKeyUp={(event) => {
            if (!event.shiftKey) return;
            selected(event.currentTarget);
          }}
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
function MonacoEditor({
  path,
  text,
  onChange,
  onSave,
  readOnly,
  notice,
  revealLine,
  onRevealed,
  onAskSelection,
  changed,
  changedTruncated,
  onRevertFile,
  reverting,
}: EditorPaneProps) {
  const host = useRef<HTMLDivElement | null>(null);
  const editor = useRef<MonacoEditorApi.IStandaloneCodeEditor | null>(null);
  const marked = useRef<MonacoEditorApi.IEditorDecorationsCollection | null>(null);
  // 与 `marked` 分开：一个是「刚才跳到的那一行」，一个是「这个文件里哪些行是新的」。
  // 合成一个集合就意味着跳一次搜索会把改动标记擦掉。
  const touched = useRef<MonacoEditorApi.IEditorDecorationsCollection | null>(null);
  // False until the lazily imported editor exists. It is state, not a ref,
  // because its whole job is to re-run the effects that had nothing to act on
  // during the first render: a ref changes no output and re-renders nothing.
  const [ready, setReady] = useState(false);
  // 选区落在屏幕上的位置由 Monaco 报告，所以这里存的不只是「选了哪几行」，还有
  // 浮层该摆在哪。位置随滚动变化，滚动事件会把它重算。
  const [pick, setPick] = useState<MonacoPick | null>(null);
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
        // Monaco 的默认字体栈没有 CJK 回退，全角字形会落到系统随便挑的字体上；
        // 显式给一个带中文回退的栈：等宽西文优先，中文落到雅黑/苹方。
        fontFamily:
          "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Cascadia Mono', 'Microsoft YaHei', 'PingFang SC', monospace",
        // Monaco 默认把「模糊字符」用方框标出来——它指的是与 ASCII 形近的字符，
        // 而中文文档里每一个全角标点（`；`、`，`、`：`）都符合这个定义。于是一份
        // 中文契约满屏都是框，标记的不是异常而是常态。关掉它，保留真正看不见的
        // 字符（零宽空格、方向控制符）的提示——那些是有人可能藏东西的地方。
        unicodeHighlight: {
          ambiguousCharacters: false,
          invisibleCharacters: true,
          nonBasicASCII: false,
        },
        lineHeight: 20,
        scrollBeyondLastLine: false,
        renderWhitespace: 'selection',
        tabSize: 2,
        wordWrap: 'off',
      });
      editor.current = instance;
      setReady(true);
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

  // 选区：Monaco 只在光标或选区变化时通知，而「问 agent」要跟在选区旁边，所以
  // 还要听滚动——滚出视口的那次必须把按钮收起来，不然它悬在无关的行上。
  useEffect(() => {
    const instance = editor.current;
    if (!ready || instance === null) return;
    const sync = (): void => {
      const model = instance.getModel();
      const selection = instance.getSelection();
      if (model === null || selection === null) {
        setPick(null);
        return;
      }
      if (
        selection.startLineNumber === selection.endLineNumber &&
        selection.startColumn === selection.endColumn
      ) {
        setPick(null);
        return;
      }
      const fromLine = selection.startLineNumber;
      const toLine = selection.endLineNumber;
      // 整行，不是从落点开始的那一段：按钮说的是「第 A–B 行」。Monaco 的选区
      // 记得列号，所以行中间按下时 getValueInRange 只给半截词。
      const text = model.getValueInRange({
        startLineNumber: fromLine,
        startColumn: 1,
        endLineNumber: toLine,
        endColumn: model.getLineMaxColumn(toLine),
      });
      // 只选中空白时不给入口：把一段空白发给 agent，什么都说明不了。
      if (text.trim() === '') {
        setPick(null);
        return;
      }
      const where = instance.getScrolledVisiblePosition({ lineNumber: fromLine, column: 1 });
      if (where === null || where.top < 0) {
        setPick(null);
        return;
      }
      // 浮层是绝对定位的：靠右的选区不能把它顶出面板，靠上的选区不能让它压住
      // 路径栏。宽度是编辑器视口的宽度，不是窗口的。
      const width = instance.getLayoutInfo().width;
      setPick({
        path,
        fromLine,
        toLine,
        text,
        top: Math.max(0, where.top),
        left: Math.max(8, Math.min(where.left, width - 210)),
      });
    };
    const moved = instance.onDidChangeCursorSelection(sync);
    const scrolled = instance.onDidScrollChange(sync);
    sync();
    return () => {
      moved.dispose();
      scrolled.dispose();
    };
  }, [ready, path]);

  // A new file means the old mark describes nothing. Declared before the reveal
  // effect on purpose: effects run in the order they are written, so a mark made
  // for the file just opened cannot be cleared by the bookkeeping of the file
  // that was open before it.
  useEffect(() => {
    marked.current?.clear();
    marked.current = null;
  }, [path]);

  // A search hit: scroll the line into view and mark it, so the reader can see
  // which of the forty lines on screen is the one that matched.
  //
  // `ready` is in the dependency list because Monaco arrives asynchronously: the
  // first run sees `editor.current === null` and returns, and without a signal
  // that the instance now exists there is no second run — opening a hit then
  // scrolled nowhere and marked nothing.
  //
  // The mark outlives the effect on purpose. `onRevealed` clears the pending
  // reveal in the store, which re-runs this effect with `revealLine` null; when
  // the mark was cleared in the cleanup, it appeared and vanished within one
  // render, which is the same as not marking it at all. It is replaced when the
  // next hit arrives, when another file is opened (the effect above), and when
  // the editor goes away — the moments it stops describing the screen.
  useEffect(() => {
    const instance = editor.current;
    if (!ready || instance === null) return;
    if (revealLine === null || revealLine === undefined) return;
    // An empty buffer has no line to show; see the fallback editor's version.
    if (text === '') return;
    const line = Math.max(1, revealLine);
    instance.revealLineInCenter(line);
    instance.setPosition({ lineNumber: line, column: 1 });
    marked.current?.clear();
    marked.current = instance.createDecorationsCollection([
      {
        range: { startLineNumber: line, startColumn: 1, endLineNumber: line, endColumn: 1 },
        options: { isWholeLine: true, className: 'wb-line-revealed' },
      },
    ]);
    onRevealed?.();
  }, [ready, revealLine, onRevealed, text]);

  // The lines this file has and the commit does not: drawn as one whole-line
  // decoration per range, so a hunk of twenty new lines is one decoration rather
  // than twenty.
  useEffect(() => {
    const instance = editor.current;
    if (!ready || instance === null) return;
    touched.current?.clear();
    touched.current = null;
    const ranges = changed?.ranges ?? [];
    if (ranges.length === 0) return;
    touched.current = instance.createDecorationsCollection(
      ranges.map((range) => ({
        range: {
          startLineNumber: range.from,
          startColumn: 1,
          endLineNumber: range.to,
          endColumn: 1,
        },
        options: { isWholeLine: true, className: 'wb-line-changed' },
      })),
    );
    // `changed` 是 store 里的对象：只在重新读过 diff 之后才换身份，所以打字不会
    // 让这里重跑一遍。
  }, [ready, path, changed]);

  // Leaving the editor entirely, too.
  useEffect(
    () => () => {
      marked.current?.clear();
      marked.current = null;
      touched.current?.clear();
      touched.current = null;
    },
    [],
  );

  return (
    <div className="wb-editor">
      <div className="wb-editor-path" title={path}>
        {path}
      </div>
      <ChangedBar
        changed={changed ?? null}
        truncated={changedTruncated}
        onRevert={onRevertFile}
        reverting={reverting}
      />
      {notice !== null && notice !== undefined && <p className="wb-editor-notice">{notice}</p>}
      <div className="wb-editor-monaco-wrap">
        <div className="wb-editor-monaco" ref={host} aria-label={`contents of ${path}`} />
        {pick !== null && (
          <button
            type="button"
            className="wb-ask-chip"
            style={{ top: pick.top, left: pick.left }}
            title="把选中的代码放进输入框，后面接着写你要它做什么"
            // 同兜底编辑器：按钮不能把焦点从输入框手里拿走。
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => {
              onAskSelection?.({
                path: pick.path,
                fromLine: pick.fromLine,
                toLine: pick.toLine,
                text: pick.text,
              });
            }}
          >
            问 agent（{rangeLabel(pick.fromLine, pick.toLine)}）
          </button>
        )}
      </div>
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