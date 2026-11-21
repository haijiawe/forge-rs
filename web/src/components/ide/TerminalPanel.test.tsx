// The terminal drawer driven by a fake link: no server, no socket, no clock.
//
// Every test here is about what the panel does with frames the server really
// sends (crates/protocol/src/protocol.rs Terminal*Frame), so the fake link only
// records what was sent and replays what arrives.
import { describe, expect, it, vi } from 'vitest';
import type { TerminalClientFrame, TerminalServerFrame } from '../../api/types';
import { interact, mount, typeInto } from '../../test/render';
import { TerminalPanel } from './TerminalPanel';
import type { TerminalLink } from './TerminalPanel';

interface FakeLink {
  link: TerminalLink;
  sent: TerminalClientFrame[];
  emit: (frame: TerminalServerFrame) => void;
  emitAll: (frames: TerminalServerFrame[]) => void;
  closeWith: (reason: string) => void;
}

function fakeLink(): FakeLink {
  const sent: TerminalClientFrame[] = [];
  let frames: ((frame: TerminalServerFrame) => void) | null = null;
  let closed: ((reason: string) => void) | null = null;
  const link: TerminalLink = {
    send: (frame) => {
      sent.push(frame);
    },
    close: () => {
      // The panel closes on unmount; a fake link has nothing to release.
    },
    onFrame: (listener) => {
      frames = listener;
    },
    onClose: (listener) => {
      closed = listener;
    },
  };
  return {
    link,
    sent,
    emit: (frame) => {
      interact(() => frames?.(frame));
    },
    emitAll: (batch) => {
      // One act for the whole batch: 2000 frames must not mean 2000 renders.
      interact(() => {
        for (const frame of batch) frames?.(frame);
      });
    },
    closeWith: (reason) => {
      interact(() => closed?.(reason));
    },
  };
}

function rows(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>('.wb-term-line'));
}

function rowTexts(container: HTMLElement): string[] {
  return rows(container).map((row) => row.textContent ?? '');
}

function input(container: HTMLElement): HTMLInputElement {
  const field = container.querySelector<HTMLInputElement>('.wb-term-input');
  if (field === null) throw new Error('the panel has no command input');
  return field;
}

/** Type a command and press Enter, the way the operator does it. */
function run(container: HTMLElement, command: string): void {
  const field = input(container);
  typeInto(field, command);
  interact(() => {
    field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  });
}

function startFrame(command: string): TerminalServerFrame {
  return { kind: 'start', protocol: 1, command, cwd: '' };
}

function outputFrame(stream: 'stdout' | 'stderr', text: string): TerminalServerFrame {
  return { kind: 'output', protocol: 1, stream, text };
}

function exitFrame(exitCode: number, cancelled = false): TerminalServerFrame {
  return {
    kind: 'exit',
    protocol: 1,
    exitCode,
    durationMs: 42,
    cancelled,
    stdoutTail: '',
    stderrTail: '',
  };
}

describe('TerminalPanel', () => {
  it('sends a run frame carrying the command and the root cwd', () => {
    const fake = fakeLink();
    const view = mount(<TerminalPanel sessionId="s1" connect={() => fake.link} />);

    run(view.container, 'cargo test');

    expect(fake.sent).toEqual([{ kind: 'run', command: 'cargo test', cwd: '' }]);
    // The command line is echoed above its output, so the reader can tell which
    // run the following lines belong to.
    expect(rowTexts(view.container)).toEqual(['$ cargo test']);
    // One command at a time: the input closes until the exit frame arrives.
    expect(input(view.container).disabled).toBe(true);

    view.unmount();
  });

  it('appends output in arrival order and colours stdout and stderr apart', () => {
    const fake = fakeLink();
    const view = mount(<TerminalPanel sessionId="s1" connect={() => fake.link} />);

    fake.emit(startFrame('make'));
    fake.emit(outputFrame('stdout', 'building\n'));
    fake.emit(outputFrame('stderr', 'warning: unused\n'));
    fake.emit(outputFrame('stdout', 'done\n'));

    const lines = rows(view.container);
    expect(lines.map((row) => row.textContent)).toEqual(['building', 'warning: unused', 'done']);
    expect(lines[0]?.className).toContain('wb-term-stdout');
    expect(lines[1]?.className).toContain('wb-term-stderr');
    expect(lines[2]?.className).toContain('wb-term-stdout');

    view.unmount();
  });

  it('prints the exit code and duration, and re-opens the input', () => {
    const fake = fakeLink();
    const view = mount(<TerminalPanel sessionId="s1" connect={() => fake.link} />);

    run(view.container, 'false');
    fake.emit(startFrame('false'));
    fake.emit(exitFrame(1));

    expect(rowTexts(view.container)).toEqual(['$ false', '退出码 1（耗时 42ms）']);
    expect(input(view.container).disabled).toBe(false);

    // A cancelled command says so instead of pretending it failed on its own.
    run(view.container, 'sleep 900');
    fake.emit(startFrame('sleep 900'));
    fake.emit(exitFrame(-1, true));
    expect(rowTexts(view.container).at(-1)).toContain('已取消');

    view.unmount();
  });

  it('prints a denial and leaves the socket usable for the next command', () => {
    const fake = fakeLink();
    const view = mount(<TerminalPanel sessionId="s1" connect={() => fake.link} />);

    run(view.container, 'cargo build');
    fake.emit(startFrame('cargo build'));
    fake.emit({ kind: 'denied', protocol: 1, reason: 'a command is already running' });

    expect(rowTexts(view.container).at(-1)).toContain('a command is already running');
    const denial = rows(view.container).at(-1);
    expect(denial?.className).toContain('wb-term-error');
    // A denial refuses one run, not the socket: the input must come back.
    expect(input(view.container).disabled).toBe(false);

    run(view.container, 'cargo build --release');
    expect(fake.sent).toEqual([
      { kind: 'run', command: 'cargo build', cwd: '' },
      { kind: 'run', command: 'cargo build --release', cwd: '' },
    ]);

    view.unmount();
  });

  it('closes the input when a live socket drops, and says a refresh is the way back', () => {
    const fake = fakeLink();
    const view = mount(<TerminalPanel sessionId="s1" connect={() => fake.link} />);

    fake.emit(startFrame('cargo build'));
    fake.emit(outputFrame('stdout', 'compiling\n'));
    fake.closeWith('the server went away');

    expect(input(view.container).disabled).toBe(true);
    const notice = view.container.querySelector('.wb-term-notice');
    expect(notice?.textContent).toContain('终端连接已断开');
    expect(notice?.textContent).toContain('the server went away');
    expect(notice?.textContent).toContain('刷新页面');

    view.unmount();
  });

  it('keeps the newest 2000 lines and drops the oldest', () => {
    const fake = fakeLink();
    const view = mount(<TerminalPanel sessionId="s1" connect={() => fake.link} />);

    const batch: TerminalServerFrame[] = [];
    for (let index = 0; index < 2005; index += 1) {
      batch.push(outputFrame('stdout', `old-${String(index).padStart(4, '0')}\n`));
    }
    fake.emitAll(batch);

    const texts = rowTexts(view.container);
    expect(texts).toHaveLength(2000);
    expect(texts[0]).toBe('old-0005');
    expect(texts.at(-1)).toBe('old-2004');
    expect(texts).not.toContain('old-0000');

    view.unmount();
  });

  it('reports a failed handshake as a read-only session and never reconnects', () => {
    const fake = fakeLink();
    const connect = vi.fn(() => fake.link);
    const view = mount(<TerminalPanel sessionId="s1" connect={connect} />);

    // A read-only session is a 403 before the upgrade, which the browser can
    // only report as a connection that never opened.
    fake.closeWith('');

    const notice = view.container.querySelector('.wb-term-notice');
    expect(notice?.textContent).toContain('这个会话是只读模式启动的，没有终端');
    expect(connect).toHaveBeenCalledTimes(1);
    // Typing into a socket that is gone would queue a command nobody sends.
    expect(input(view.container).disabled).toBe(true);
    expect(input(view.container).placeholder).toContain('刷新页面');

    view.unmount();
  });
});
