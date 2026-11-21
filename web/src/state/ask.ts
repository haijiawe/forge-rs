// 从编辑器选区到输入框的那条线。
//
// 两侧隔着一层组件：编辑器在 Workbench 的中间栏，Composer 由 SessionView 生成后
// 作为 `agent` 节点交给它，两者之间没有共享的父组件可以传 prop。所以这里是一个
// 只有一件事的发射器：编辑器侧调用 `askAboutSelection`，输入框侧用
// `onAskRequested` 接住，把选区写成一段带文件与行号的文本放进草稿。
//
// 它不做任何格式化以外的判断：要不要发、发给谁、发完是否清空，都是使用方的决定。

/** 一次「把这段代码交给 agent」的请求。行号是 1 起的，闭区间。 */
export interface SelectionAsk {
  path: string;
  fromLine: number;
  toLine: number;
  /** 选中的原文。超过上限的部分会被截掉，并在正文里注明。 */
  text: string;
}

/** 一条消息里附上的代码最多这么多行：再长就不是「这段」，而是整份文件。 */
export const MAX_SNIPPET_LINES = 400;
/** 以及最多这么多字符，防止单行超长的压缩产物把输入框塞满。 */
export const MAX_SNIPPET_CHARS = 16_000;

const FENCE = '```';

/** 「第 12 行」或「第 12–28 行」：一行时说区间是噪音。 */
export function rangeLabel(fromLine: number, toLine: number): string {
  const from = Math.max(1, Math.trunc(fromLine));
  const to = Math.max(from, Math.trunc(toLine));
  return from === to ? `第 ${String(from)} 行` : `第 ${String(from)}–${String(to)} 行`;
}

/**
 * 选区变成草稿里的那一段。
 *
 * 文件与行号写在围栏外面，是因为围栏里的内容要原样交给模型：一段没有出处的
 * 代码，模型只能猜它属于哪个文件。截断写成句子而不是省略号，是因为「少了
 * 300 行」这件事本身会影响对方怎么回答。
 */
export function snippetFor(ask: SelectionAsk): string {
  const lines = ask.text.split('\n');
  let body = lines.slice(0, MAX_SNIPPET_LINES).join('\n');
  // 按码点切，不把一个字符劈成两半。
  const cutByChars = body.length > MAX_SNIPPET_CHARS;
  if (cutByChars) body = Array.from(body).slice(0, MAX_SNIPPET_CHARS).join('');
  // 行数按最终正文重算：截断之后报的行数才是真的少了多少行。
  const dropped = lines.length - body.split('\n').length;
  // 少了行就报行数（哪怕同时被字符上限切过）；只切了字符没有少行时说明是切了。
  let note = '';
  if (dropped > 0) note = `\n（选区还有 ${String(dropped)} 行没有附上）`;
  else if (cutByChars) note = '\n（选区太长，只附上了前一部分）';
  return `文件 ${ask.path}，${rangeLabel(ask.fromLine, ask.toLine)}：\n${FENCE}\n${body}\n${FENCE}${note}\n\n`;
}

type Listener = (ask: SelectionAsk) => void;

const listeners = new Set<Listener>();

/** 编辑器侧：把这条选区交给输入框。没有人在听时它安静地消失。 */
export function askAboutSelection(ask: SelectionAsk): void {
  for (const listener of [...listeners]) listener(ask);
}

/** 输入框侧：接住选区。返回的函数取消订阅。 */
export function onAskRequested(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
