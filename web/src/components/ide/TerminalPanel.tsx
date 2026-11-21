// The terminal drawer: one command line at a time, streamed back live.
//
// GET /api/sessions/{id}/terminal speaks an older, smaller protocol than the
// event stream: a run frame starts one process, output frames arrive in order,
// and one exit frame ends it. There is no reconnect and no replay — a command is
// not a log entry you can catch up on, so a dropped socket is reported, not
// healed. That is why this panel keeps no cursor and never retries.
//
// The socket itself is injected (`connect`) so a test can drive frames without a
// server; `defaultConnect` is the only place that knows about WebSocket.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { FormEvent, KeyboardEvent as ReactKeyboardEvent } from 'react';
import { WS_SUBPROTOCOL, terminalUrl } from '../../api/client';
import { decodeTerminalFrame } from '../../api/protocol';
import type { TerminalClientFrame, TerminalServerFrame } from '../../api/types';

/**
 * The socket, as this panel needs it: send a frame, be told about frames, be
 * told when it ends. Deliberately smaller than WebSocket — a fake link in a test
 * is nine lines.
 */
export interface TerminalLink {
  send(frame: TerminalClientFrame): void;
  close(): void;
  onFrame(listener: (frame: TerminalServerFrame) => void): void;
  onClose(listener: (reason: string) => void): void;
}

export type TerminalConnect = (sessionId: string) => TerminalLink;

/**
 * The production link: a WebSocket on the terminal route with the `hearth.v1`
 * subprotocol. The token is not sent here — the server set the HttpOnly cookie
 * when it served the shell, exactly as for the event stream.
 */
export function defaultConnect(sessionId: string): TerminalLink {
  const frameListeners: ((frame: TerminalServerFrame) => void)[] = [];
  const closeListeners: ((reason: string) => void)[] = [];
  /** Frames typed before the handshake finished. Bounded: a dead socket drops. */
  let queue: string[] = [];
  let socket: WebSocket | null = null;
  let failure: string | null = null;
  let settled = false;

  const settle = (reason: string): void => {
    if (settled) return;
    settled = true;
    queue = [];
    for (const listener of closeListeners) listener(reason);
  };

  try {
    if (typeof WebSocket === 'undefined') {
      throw new Error('这个浏览器没有 WebSocket');
    }
    socket = new WebSocket(terminalUrl('', sessionId), [WS_SUBPROTOCOL]);
    socket.onopen = (): void => {
      const pending = queue;
      queue = [];
      for (const raw of pending) socket?.send(raw);
    };
    socket.onmessage = (event: MessageEvent): void => {
      if (typeof event.data !== 'string') return;
      let frame: TerminalServerFrame;
      try {
        frame = decodeTerminalFrame(event.data);
      } catch {
        // A frame this client does not know is a newer server, not a dead one:
        // dropping the frame keeps the socket usable for the kinds it does know.
        return;
      }
      for (const listener of frameListeners) listener(frame);
    };
    // A read-only session fails here: the server answers 403 before the upgrade,
    // so the browser can report only that the handshake did not complete.
    socket.onerror = (): void => {
      settle('');
    };
    socket.onclose = (event: CloseEvent): void => {
      settle(typeof event.reason === 'string' ? event.reason : '');
    };
  } catch (error) {
    failure = error instanceof Error ? error.message : String(error);
  }

  return {
    send: (frame) => {
      const raw = JSON.stringify(frame);
      if (socket !== null && socket.readyState === 1) {
        socket.send(raw);
        return;
      }
      if (queue.length < 16) queue.push(raw);
    },
    close: () => {
      // A close we asked for is not a failure: mark it settled so no listener
      // hears about it during unmount.
      settled = true;
      queue = [];
      socket?.close();
      socket = null;
    },
    onFrame: (listener) => {
      frameListeners.push(listener);
    },
    onClose: (listener) => {
      closeListeners.push(listener);
      if (failure !== null) listener(failure);
    },
  };
}

type RowKind = 'command' | 'stdout' | 'stderr' | 'info' | 'error';

interface Row {
  /** Monotonic within one log; exists only to key the React list. */
  id: number;
  kind: RowKind;
  text: string;
}

interface TerminalLog {
  rows: Row[];
  nextId: number;
  /**
   * The tail of each stream that has no newline yet. Output chunks split on pipe
   * buffer boundaries, not line boundaries, so a line can arrive in pieces.
   */
  partial: { stdout: string; stderr: string };
}

/**
 * How many output lines are kept. A build can print hundreds of thousands of
 * lines; the operator reads the tail, and an unbounded log is a way to run the
 * tab out of memory while nobody is watching.
 */
const MAX_ROWS = 2000;

const EMPTY_LOG: TerminalLog = { rows: [], nextId: 1, partial: { stdout: '', stderr: '' } };

function pushRows(log: TerminalLog, kind: RowKind, texts: string[]): TerminalLog {
  if (texts.length === 0) return log;
  const added: Row[] = texts.map((text, index) => ({ id: log.nextId + index, kind, text }));
  const rows = log.rows.concat(added);
  // Drop from the front, never from the back: the newest lines are the ones the
  // operator is looking at.
  return {
    rows: rows.length > MAX_ROWS ? rows.slice(rows.length - MAX_ROWS) : rows,
    nextId: log.nextId + added.length,
    partial: log.partial,
  };
}

function appendOutput(log: TerminalLog, stream: 'stdout' | 'stderr', text: string): TerminalLog {
  const pieces = (log.partial[stream] + text).split('\n');
  const rest = pieces.pop() ?? '';
  const partial: TerminalLog['partial'] =
    stream === 'stdout' ? { ...log.partial, stdout: rest } : { ...log.partial, stderr: rest };
  return pushRows({ ...log, partial }, stream, pieces);
}

/** Print whatever the last chunk left without a newline, e.g. a prompt. */
function flushPartials(log: TerminalLog): TerminalLog {
  let next = log;
  if (next.partial.stdout !== '') next = pushRows(next, 'stdout', [next.partial.stdout]);
  if (next.partial.stderr !== '') next = pushRows(next, 'stderr', [next.partial.stderr]);
  if (next === log) return log;
  return { ...next, partial: { stdout: '', stderr: '' } };
}

function applyFrame(log: TerminalLog, frame: TerminalServerFrame): TerminalLog {
  switch (frame.kind) {
    case 'start':
      // The `$ command` row is printed when the frame is sent, so start adds
      // nothing to the log; it only tells the panel the command is really live.
      return log;
    case 'output':
      return appendOutput(log, frame.stream, frame.text);
    case 'exit': {
      const after = flushPartials(log);
      // stdoutTail/stderrTail are for a client that missed frames; this panel
      // rendered them as they arrived, so echoing the tails would duplicate.
      const text = frame.cancelled
        ? `已取消（耗时 ${String(frame.durationMs)}ms）`
        : frame.exitCode === -1
          ? `没有跑起来（退出码 -1，耗时 ${String(frame.durationMs)}ms）`
          : `退出码 ${String(frame.exitCode)}（耗时 ${String(frame.durationMs)}ms）`;
      return pushRows(after, 'info', [text]);
    }
    case 'denied':
      // The socket is still usable: this frame refused one run, not the socket.
      return pushRows(flushPartials(log), 'error', [`已拒绝：${frame.reason}`]);
  }
}

export interface TerminalPanelProps {
  sessionId: string;
  /** Injected in tests; production uses `defaultConnect`. */
  connect?: TerminalConnect;
}

/**
 * The terminal drawer. One command at a time — the server refuses a second run
 * while one is live, so the input is disabled rather than left to bounce.
 */
export function TerminalPanel({ sessionId, connect }: TerminalPanelProps) {
  const [log, setLog] = useState<TerminalLog>(EMPTY_LOG);
  const [draft, setDraft] = useState('');
  const [running, setRunning] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  /**
   * The socket ended and this panel never reconnects. Typing into it would queue
   * a command nobody sends, so the input closes with it — the notice already says
   * that a refresh is the way back.
   */
  const [dead, setDead] = useState(false);

  const linkRef = useRef<TerminalLink | null>(null);
  const logRef = useRef<HTMLDivElement | null>(null);
  /** True while the output is scrolled to the bottom; false once the reader leaves. */
  const stickRef = useRef(true);
  /** True once any frame arrived: a close before that is a failed handshake. */
  const sawFrameRef = useRef(false);
  const factoryRef = useRef<TerminalConnect>(connect ?? defaultConnect);

  useEffect(() => {
    factoryRef.current = connect ?? defaultConnect;
  });

  useEffect(() => {
    let live = true;
    sawFrameRef.current = false;
    stickRef.current = true;
    setLog(EMPTY_LOG);
    setRunning(false);
    setNotice(null);
    setDead(false);

    const link = factoryRef.current(sessionId);
    linkRef.current = link;

    link.onFrame((frame) => {
      if (!live) return;
      sawFrameRef.current = true;
      setLog((current) => applyFrame(current, frame));
      if (frame.kind === 'start') setRunning(true);
      if (frame.kind === 'exit' || frame.kind === 'denied') setRunning(false);
    });

    link.onClose((reason) => {
      if (!live) return;
      setRunning(false);
      setDead(true);
      const because = reason === '' ? '' : `（${reason}）`;
      // The socket cannot say *why* it did not open. A read-only session is a 403
      // before the upgrade, which the browser reports only as a failed
      // connection, so a close before one frame is that case by default — and
      // saying so is the honest reading rather than an empty pane.
      setNotice(
        sawFrameRef.current
          ? `终端连接已断开${because}。刷新页面可以重新连接。`
          : `这个会话是只读模式启动的，没有终端${because}。`,
      );
    });

    return () => {
      live = false;
      linkRef.current = null;
      link.close();
    };
  }, [sessionId]);

  // Follow the tail, but never fight the reader: once they scroll up, new output
  // stops pulling them back down until they return to the bottom.
  useEffect(() => {
    const node = logRef.current;
    if (node === null || !stickRef.current) return;
    node.scrollTop = node.scrollHeight;
  }, [log.rows]);

  const sendFrame = useCallback((frame: TerminalClientFrame): void => {
    linkRef.current?.send(frame);
  }, []);

  function onLogScroll(): void {
    const node = logRef.current;
    if (node === null) return;
    // A few pixels of slack: a fractional layout leaves scrollTop just short of
    // the mathematical bottom.
    stickRef.current = node.scrollHeight - node.scrollTop - node.clientHeight < 24;
  }

  function submit(event?: FormEvent<HTMLFormElement>): void {
    if (event !== undefined) event.preventDefault();
    if (running || dead) return;
    const command = draft.trim();
    if (command === '') return;
    setDraft('');
    // cwd is the workspace root: the drawer has no path picker, and the field is
    // a workspace-relative path where '' means the root.
    sendFrame({ kind: 'run', command, cwd: '' });
    setLog((current) => pushRows(current, 'command', [`$ ${command}`]));
    stickRef.current = true;
    setRunning(true);
  }

  function onInputKeyDown(event: ReactKeyboardEvent<HTMLInputElement>): void {
    if (event.key !== 'Enter') return;
    event.preventDefault();
    submit();
  }

  function clearLog(): void {
    setLog(EMPTY_LOG);
    stickRef.current = true;
  }

  return (
    <div className="wb-term">
      <div className="wb-term-head">
        <span className="wb-term-title">终端</span>
        <span className="wb-term-state">{running ? '运行中' : '空闲'}</span>
        <div className="wb-term-actions">
          <button type="button" className="button button-ghost" onClick={clearLog}>
            清屏
          </button>
          <button type="button" className="button button-ghost" onClick={() => sendFrame({ kind: 'cancel' })} disabled={!running}>
            中断
          </button>
        </div>
      </div>
      <div className="wb-term-log" ref={logRef} onScroll={onLogScroll} role="log" aria-label="终端输出">
        {log.rows.map((row) => (
          <div key={row.id} className={`wb-term-line wb-term-${row.kind}`}>
            {row.text === '' ? '\u00a0' : row.text}
          </div>
        ))}
        {log.rows.length === 0 ? <p className="wb-term-hint">在这里输入命令，Enter 运行。</p> : null}
      </div>
      {notice !== null ? (
        <p className="wb-term-notice" role="alert">
          {notice}
        </p>
      ) : null}
      <form className="wb-term-form" onSubmit={submit}>
        <span className="wb-term-prompt" aria-hidden="true">
          $
        </span>
        <input
          className="wb-term-input"
          value={draft}
          disabled={running || dead}
          aria-label="终端命令"
          placeholder={
            dead ? '终端已断开，刷新页面重连' : running ? '命令运行中，等它结束' : '输入命令，Enter 运行'
          }
          spellCheck={false}
          autoComplete="off"
          onChange={(event) => {
            setDraft(event.target.value);
          }}
          onKeyDown={onInputKeyDown}
        />
        <button type="submit" className="button button-ghost" disabled={running || dead}>
          运行
        </button>
      </form>
    </div>
  );
}
