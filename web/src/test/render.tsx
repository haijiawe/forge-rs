// A tiny render helper: react-dom/client + act, no testing-library (the
// dependency list for this milestone is fixed and does not include it).
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import type { ReactElement } from 'react';

declare global {
  // React reads this flag to decide whether act() is supported; jsdom has no
  // window.IS_REACT_ACT_ENVIRONMENT of its own.
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

export interface Mounted {
  container: HTMLElement;
  unmount: () => void;
  rerender: (element: ReactElement) => void;
}

export function mount(element: ReactElement): Mounted {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  const container = document.createElement('div');
  document.body.appendChild(container);
  let root: Root = createRoot(container);
  act(() => {
    root.render(element);
  });
  return {
    container,
    rerender: (next: ReactElement) => {
      act(() => {
        root.render(next);
      });
    },
    unmount: () => {
      act(() => {
        root.unmount();
      });
      container.remove();
    },
  };
}

/** Let queued promise callbacks and microtasks run inside act(). */
export async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

/**
 * Pump the event loop — microtasks *and* timers — inside act() until
 * `predicate` holds, then let React commit.
 *
 * The store publishes its snapshot from a frame callback (requestAnimationFrame,
 * or a 16 ms timer where rAF is absent), so a fixed number of microtask flushes
 * is a race: sometimes the frame has fired, sometimes it has not. Waiting on the
 * condition is the only deterministic option, and it still fails loudly on a
 * real timeout instead of hanging or passing by luck.
 */
export async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) {
      throw new Error(`waitFor: condition not met within ${timeoutMs}ms`);
    }
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 5));
    });
  }
  await flush();
}

/** Dispatch a DOM event and let React commit the resulting render. */
export function interact(action: () => void): void {
  act(() => {
    action();
  });
}

/** Type into a controlled input/textarea the way a user would. */
export function typeInto(element: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  interact(() => {
    const setter = Object.getOwnPropertyDescriptor(
      element instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype,
      'value',
    )?.set;
    setter?.call(element, value);
    element.dispatchEvent(new Event('input', { bubbles: true }));
  });
}