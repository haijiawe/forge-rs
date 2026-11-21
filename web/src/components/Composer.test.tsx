// The composer's turn controls: Enter sends while idle, the same Enter steers
// while a turn runs (§4.10), and Stop asks the running turn to end (§4.9).
// The second half covers attachments: what a pick shows, what the send carries,
// and what survives a refusal or a failure.
//
// The component takes the three verbs as props, so here they are spies: that is
// what makes "exactly once" and "the draft was not thrown away" observable.
// The session id and the visible error banner belong to SessionView/the store
// and are covered in SessionView.test.tsx / sessionStore.test.ts.
import { describe, expect, it, vi } from 'vitest';
import { Composer } from './Composer';
import { flush, interact, mount, typeInto, waitFor } from '../test/render';

function textareaOf(container: HTMLElement): HTMLTextAreaElement {
  const element = container.querySelector('textarea');
  if (element === null) throw new Error('no textarea');
  return element;
}

function buttonByText(container: HTMLElement, label: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll('button')).find(
    (candidate) => candidate.textContent === label,
  );
  if (button === undefined) throw new Error(`no button labelled ${label}`);
  return button;
}

function click(container: HTMLElement, label: string): void {
  interact(() => {
    buttonByText(container, label).dispatchEvent(new MouseEvent('click', { bubbles: true }));
  });
}

/** Enter (or Shift+Enter) the way a keyboard would: one bubbling keydown. */
function keydown(input: HTMLTextAreaElement, init: { shiftKey?: boolean } = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', {
    key: 'Enter',
    shiftKey: init.shiftKey ?? false,
    bubbles: true,
    cancelable: true,
  });
  interact(() => {
    input.dispatchEvent(event);
  });
  return event;
}

describe('Composer', () => {
  it('idle: Enter sends once and never steers or cancels', async () => {
    let release!: () => void;
    const gate = new Promise<boolean>((resolve) => {
      release = () => resolve(true);
    });
    const onSend = vi.fn(() => gate);
    const onSteer = vi.fn(async () => true);
    const onStop = vi.fn(async () => true);

    const view = mount(
      <Composer disabled={false} onSend={onSend} onSteer={onSteer} onStop={onStop} />,
    );
    const textarea = textareaOf(view.container);
    typeInto(textarea, 'hello agent');

    // Two Enters back to back (no await in between) must reach the network
    // once: `sending` is set before the first await and React commits the
    // disabled textarea after each discrete event. Two keydowns *batched into
    // one act()* are not deduped — the guard is state, not a ref — but a
    // browser never dispatches two key events before that commit.
    keydown(textarea);
    keydown(textarea);

    expect(onSend).toHaveBeenCalledTimes(1);
    // The second argument is the attachment list — empty, because nothing was
    // picked. It is passed on every send so the caller never has to guess
    // whether "no files" means `undefined` or `[]`.
    expect(onSend).toHaveBeenCalledWith('hello agent', []);
    expect(onSteer).not.toHaveBeenCalled();
    expect(onStop).not.toHaveBeenCalled();

    release();
    await waitFor(() => textarea.value === '');
    expect(buttonByText(view.container, '发送')).toBeDefined();
    view.unmount();
  });

  it('running: Enter steers with the typed text, does not send, and clears the draft', async () => {
    const onSend = vi.fn(async () => true);
    const onSteer = vi.fn(async () => true);
    const onStop = vi.fn(async () => true);

    const view = mount(
      <Composer disabled={false} running onSend={onSend} onSteer={onSteer} onStop={onStop} />,
    );
    const textarea = textareaOf(view.container);
    // The surface says what Enter will do before anything is typed.
    expect(buttonByText(view.container, '引导')).toBeDefined();
    expect(textarea.placeholder).toContain('引导正在运行的回合');

    typeInto(textarea, 'also check the retry path');
    keydown(textarea);
    await waitFor(() => textarea.value === '');

    expect(onSteer).toHaveBeenCalledTimes(1);
    expect(onSteer).toHaveBeenCalledWith('also check the retry path');
    expect(onSend).not.toHaveBeenCalled();
    expect(onStop).not.toHaveBeenCalled();
    view.unmount();
  });

  it('running: Stop cancels once, and a second click in the same tick is ignored', async () => {
    let release!: () => void;
    const gate = new Promise<boolean>((resolve) => {
      release = () => resolve(true);
    });
    const onStop = vi.fn(() => gate);
    const onSend = vi.fn(async () => true);
    const onSteer = vi.fn(async () => true);

    const view = mount(
      <Composer disabled={false} running onSend={onSend} onSteer={onSteer} onStop={onStop} />,
    );
    const stopButton = (): HTMLButtonElement => {
      const button = view.container.querySelector('.composer button[type="button"]');
      if (button === null) throw new Error('no Stop button');
      return button as HTMLButtonElement;
    };

    click(view.container, '停止');
    expect(onStop).toHaveBeenCalledTimes(1);
    // In flight: the label says so and the button is disabled.
    expect(stopButton().textContent).toBe('正在停止…');
    expect(stopButton().disabled).toBe(true);

    // A second click already queued in the same tick (dispatched past the
    // disabled attribute, as a synthetic event or a re-queued one would be)
    // must not reach the network a second time.
    interact(() => {
      stopButton().dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(onStop).toHaveBeenCalledTimes(1);

    release();
    await waitFor(() => stopButton().textContent === '停止');
    expect(stopButton().disabled).toBe(false);
    // Stopping is not sending and not steering.
    expect(onSend).not.toHaveBeenCalled();
    expect(onSteer).not.toHaveBeenCalled();
    view.unmount();
  });

  it('does not send or steer empty or whitespace-only input', async () => {
    const onSend = vi.fn(async () => true);
    const onSteer = vi.fn(async () => true);

    const idle = mount(<Composer disabled={false} onSend={onSend} />);
    const idleInput = textareaOf(idle.container);
    // Empty draft: the submit button is disabled and Enter is a no-op.
    expect(buttonByText(idle.container, '发送').disabled).toBe(true);
    keydown(idleInput);
    typeInto(idleInput, '  \n\t  ');
    expect(buttonByText(idle.container, '发送').disabled).toBe(true);
    keydown(idleInput);
    expect(onSend).not.toHaveBeenCalled();
    expect(idleInput.value).toBe('  \n\t  '); // the draft is not swallowed
    idle.unmount();

    // Same while running: whitespace must not become a steer (the server would
    // answer 422 unprocessable_entity, api.md §4.10).
    const running = mount(
      <Composer disabled={false} running onSend={onSend} onSteer={onSteer} />,
    );
    const runningInput = textareaOf(running.container);
    typeInto(runningInput, '   ');
    keydown(runningInput);
    expect(onSteer).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
    running.unmount();
  });

  it('Shift+Enter does not submit and leaves the newline to the browser', async () => {
    const onSend = vi.fn(async () => true);
    const view = mount(<Composer disabled={false} onSend={onSend} />);
    const textarea = textareaOf(view.container);

    typeInto(textarea, 'line one');
    const event = keydown(textarea, { shiftKey: true });
    // jsdom has no text editing, so the newline itself cannot be observed: what
    // is testable is that the component does not call preventDefault, leaving
    // the browser's own newline insertion in place, and does not submit.
    expect(event.defaultPrevented).toBe(false);
    expect(onSend).not.toHaveBeenCalled();
    expect(textarea.value).toBe('line one');

    // A multi-line draft is still submitted whole by a plain Enter.
    typeInto(textarea, 'line one\nline two');
    keydown(textarea);
    await waitFor(() => onSend.mock.calls.length === 1);
    expect(onSend).toHaveBeenCalledWith('line one\nline two', []);
    view.unmount();
  });

  it('a failed steer keeps the draft and re-enables the composer', async () => {
    // The store's contract is a boolean, not a throw: steerTurn() reports the
    // 409 as `false` and puts the message in state.actionError (see
    // sessionStore.test.ts). The composer must not throw the text away.
    const onSteer = vi.fn(async () => false);
    const onSend = vi.fn(async () => true);

    const view = mount(
      <Composer disabled={false} running onSend={onSend} onSteer={onSteer} />,
    );
    const textarea = textareaOf(view.container);
    typeInto(textarea, 'retry with backoff');
    keydown(textarea);

    // Wait on the *submit button's* label, not a lookup by name: while the
    // handler is in flight the label is 'sending…', so a by-name lookup races.
    const submitButton = (): HTMLButtonElement => {
      const button = view.container.querySelector('button[type="submit"]');
      if (!(button instanceof HTMLButtonElement)) throw new Error('no submit button');
      return button;
    };
    await waitFor(() => submitButton().textContent === '引导');
    expect(submitButton().disabled).toBe(false);
    expect(onSteer).toHaveBeenCalledWith('retry with backoff');
    expect(onSend).not.toHaveBeenCalled();
    // Actual behaviour: the draft survives the rejection (no data loss).
    expect(textarea.value).toBe('retry with backoff');

    // No error surface of its own: the visible message is SessionView's
    // role="alert" banner fed by state.actionError (SessionView.tsx:75-82),
    // proven end-to-end in SessionView.test.tsx.
    await flush();
    view.unmount();
  });

  // Files. What is proven here is the round trip through the picker, the removal
// and what survives a failed send; the encoding, the kind decision and every cap
// are pinned without a DOM in state/attachments.test.ts, because a 2 GB refusal
// is not something a component test should have to allocate.

function fileInputOf(container: HTMLElement): HTMLInputElement {
  const input = container.querySelector('input[type="file"]');
  if (!(input instanceof HTMLInputElement)) throw new Error('no file input');
  return input;
}

/**
 * Pick files the way a browser delivers them: the input's FileList holds them
 * and one change event follows.
 *
 * jsdom exposes no way to set a file input's selection, so the list is shadowed
 * on the element — the same technique @testing-library/user-event uses. The
 * component reads `input.files` before it clears the input, so the shadow is
 * what it sees.
 */
function pickFiles(container: HTMLElement, files: File[]): void {
  const input = fileInputOf(container);
  Object.defineProperty(input, 'files', { value: files, configurable: true });
  interact(() => {
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  });
}

/** The eight-byte PNG signature: 89 50 4E 47 0D 0A 1A 0A. */
const PNG_BYTES = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function pngFile(name = 'shot.png'): File {
  return new File([PNG_BYTES], name, { type: 'image/png' });
}

function chips(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>('.composer-attachment'));
}

function refusalsOf(container: HTMLElement): HTMLElement | null {
  return container.querySelector('.composer-refusals');
}

describe('Composer attachments', () => {
  it('offers the picker as a real, labelled control', () => {
    const view = mount(<Composer disabled={false} onSend={vi.fn(async () => true)} />);

    const input = fileInputOf(view.container);
    expect(input.getAttribute('aria-label')).toBe('添加图片或 .diff/.patch 文件');
    expect(input.multiple).toBe(true);
    expect(input.accept).toBe('image/*,.diff,.patch');
    // Not display:none and not `hidden`: an invisible input cannot be reached
    // with Tab, and picking a file is the only way to attach one.
    expect(input.hidden).toBe(false);
    expect(input.style.display).not.toBe('none');
    expect(input.disabled).toBe(false);
    view.unmount();
  });

  it('shows a picked file and sends its bytes with the message', async () => {
    const onSend = vi.fn(async () => true);
    const view = mount(<Composer disabled={false} onSend={onSend} />);

    pickFiles(view.container, [pngFile()]);
    await waitFor(() => chips(view.container).length === 1);

    const chip = chips(view.container)[0];
    expect(chip?.textContent).toContain('shot.png');
    expect(chip?.textContent).toContain('image · 8 B');

    const textarea = textareaOf(view.container);
    typeInto(textarea, 'what is wrong in this screenshot?');
    keydown(textarea);

    await waitFor(() => onSend.mock.calls.length === 1);
    expect(onSend).toHaveBeenCalledWith('what is wrong in this screenshot?', [
      // 89 50 4E 47 0D 0A 1A 0A -> iVBORw0KGgo= (see attachments.test.ts).
      { kind: 'image', data: 'iVBORw0KGgo=', name: 'shot.png', mediaType: 'image/png' },
    ]);
    // Accepted, so the chip goes with the draft.
    await waitFor(() => chips(view.container).length === 0);
    view.unmount();
  });

  it('removes one attachment before sending, leaving the others', async () => {
    const onSend = vi.fn(async () => true);
    const view = mount(<Composer disabled={false} onSend={onSend} />);

    pickFiles(view.container, [pngFile('a.png'), pngFile('b.png')]);
    await waitFor(() => chips(view.container).length === 2);

    const remove = view.container.querySelector('.composer-attachment .link-button');
    if (!(remove instanceof HTMLButtonElement)) throw new Error('no remove button');
    // A real <button>: Tab reaches it and the browser activates it on Enter and
    // Space without any key handler of ours. The label says which file it
    // removes, because "remove" alone is ambiguous with several attached.
    expect(remove.tagName).toBe('BUTTON');
    expect(remove.getAttribute('type')).toBe('button');
    expect(remove.getAttribute('aria-label')).toBe('移除 a.png');

    click(view.container, '移除');
    await waitFor(() => chips(view.container).length === 1);
    expect(chips(view.container)[0]?.textContent).toContain('b.png');
    expect(onSend).not.toHaveBeenCalled();
    view.unmount();
  });

  it('keeps the file when the send fails', async () => {
    // A failed send dumps the message into state.actionError and resolves false;
    // the file must survive it, because a picked file cannot be retyped.
    const onSend = vi.fn(async () => false);
    const view = mount(<Composer disabled={false} onSend={onSend} />);

    pickFiles(view.container, [pngFile()]);
    await waitFor(() => chips(view.container).length === 1);

    const textarea = textareaOf(view.container);
    typeInto(textarea, 'look at this');
    keydown(textarea);
    await waitFor(() => onSend.mock.calls.length === 1);
    await flush();

    expect(textarea.value).toBe('look at this');
    expect(chips(view.container).length).toBe(1);
    // And the box is usable again, so the retry is possible.
    expect(buttonByText(view.container, '发送').disabled).toBe(false);
    view.unmount();
  });

  it('refuses an unsupported file in words, and the refusal can be dismissed', async () => {
    const onSend = vi.fn(async () => true);
    const view = mount(<Composer disabled={false} onSend={onSend} />);

    pickFiles(view.container, [
      new File([new Uint8Array([0x68, 0x69])], 'notes.txt', { type: 'text/plain' }),
    ]);
    await waitFor(() => refusalsOf(view.container) !== null);

    const alert = refusalsOf(view.container);
    // role="alert" is what makes a screen reader say it without focus moving.
    expect(alert?.getAttribute('role')).toBe('alert');
    expect(alert?.textContent).toContain('不支持的文件："notes.txt" 类型 text/plain。');
    expect(alert?.textContent).toContain('工作区内的路径');
    expect(chips(view.container).length).toBe(0);

    click(view.container, '关闭');
    await waitFor(() => refusalsOf(view.container) === null);
    view.unmount();
  });

  it('refuses to steer with a file instead of dropping it', async () => {
    const onSend = vi.fn(async () => true);
    const onSteer = vi.fn(async () => true);
    const view = mount(
      <Composer disabled={false} running onSend={onSend} onSteer={onSteer} />,
    );

    pickFiles(view.container, [pngFile()]);
    await waitFor(() => chips(view.container).length === 1);

    const textarea = textareaOf(view.container);
    typeInto(textarea, 'and this too');
    keydown(textarea);
    await waitFor(() => refusalsOf(view.container) !== null);

    // SteerRequest carries text only, so a steer would lose the file silently.
    expect(onSteer).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
    expect(refusalsOf(view.container)?.textContent).toContain(
      '转交时不能附带文件',
    );
    expect(chips(view.container).length).toBe(1);
    expect(textarea.value).toBe('and this too');
    view.unmount();
  });

  // Note on the finding that used to live here: submit() had try/finally with no
  // catch, so a rejecting onSend/onSteer escaped as an unhandled rejection. The
  // attachment rewrite added the catch (both submit() and stop()), so the note
  // is gone rather than stale — the draft now survives a throwing handler too.
  });
});