// The composer's turn controls: Enter sends while idle, the same Enter steers
// while a turn runs (§4.10), and Stop asks the running turn to end (§4.9).
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
    expect(onSend).toHaveBeenCalledWith('hello agent');
    expect(onSteer).not.toHaveBeenCalled();
    expect(onStop).not.toHaveBeenCalled();

    release();
    await waitFor(() => textarea.value === '');
    expect(buttonByText(view.container, 'Send')).toBeDefined();
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
    expect(buttonByText(view.container, 'Steer')).toBeDefined();
    expect(textarea.placeholder).toContain('Steer the running turn');

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

    click(view.container, 'Stop');
    expect(onStop).toHaveBeenCalledTimes(1);
    // In flight: the label says so and the button is disabled.
    expect(stopButton().textContent).toBe('stopping…');
    expect(stopButton().disabled).toBe(true);

    // A second click already queued in the same tick (dispatched past the
    // disabled attribute, as a synthetic event or a re-queued one would be)
    // must not reach the network a second time.
    interact(() => {
      stopButton().dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(onStop).toHaveBeenCalledTimes(1);

    release();
    await waitFor(() => stopButton().textContent === 'Stop');
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
    expect(buttonByText(idle.container, 'Send').disabled).toBe(true);
    keydown(idleInput);
    typeInto(idleInput, '  \n\t  ');
    expect(buttonByText(idle.container, 'Send').disabled).toBe(true);
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
    expect(onSend).toHaveBeenCalledWith('line one\nline two');
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
    await waitFor(() => submitButton().textContent === 'Steer');
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

  // TODO(finding, not fixed: product code is off-limits here): submit() in
  // Composer.tsx:38-47 has try/finally but no catch, and it is called as
  // `void submit()` (Composer.tsx:70,84). An onSend/onSteer that *rejects*
  // instead of resolving false therefore escapes as an unhandled rejection and
  // the user sees nothing. SessionView always passes the store's methods, which
  // catch and resolve false, so this is a latent robustness gap, not a live bug.
});