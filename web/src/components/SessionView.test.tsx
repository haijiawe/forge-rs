// Turn-control wiring, end to end: the real Composer + SessionStore + SessionView
// against a fake ForgeApi and a fake WebSocket. What the Composer test cannot
// see live here: the session id on the wire, that a running turn's Enter goes to
// POST /steer instead of POST /messages, and that a failed steer becomes a
// visible role="alert" banner without losing the draft.
//
// No second harness: mount/flush/waitFor/interact/typeInto all come from
// test/render.tsx. The fake API is a local object literal because render.tsx has
// no API fixture (client.ts only says ForgeApi exists so tests can fake it).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api/client';
import type { ForgeApi } from '../api/client';
import type { CancelResponse, EventsPage, SessionDetail } from '../api/types';
import type { EventEnvelope } from '../api/events';
import { SessionView } from './SessionView';
import { flush, interact, mount, typeInto, waitFor } from '../test/render';

const sessionDetail: SessionDetail = {
  id: 'sess-1',
  createdAt: 1_700_000_000_000,
  updatedAt: 1_700_000_001_000,
  status: 'idle',
  lastSeq: 2,
  turns: 1,
  workspace: 'C:\\work\\repo',
  model: 'deepseek-v4',
};

function eventsPage(types: readonly string[]): EventsPage {
  const events = types.map((type, index) => ({
    v: 1,
    seq: index + 1,
    id: `e${String(index + 1)}`,
    ts: 1_700_000_000_000 + index,
    session: 'sess-1',
    type,
    ...(type === 'turn/start' ? { turn: 1, payload: { number: 1 } } : {}),
  }));
  return {
    sessionId: 'sess-1',
    from: 0,
    to: types.length,
    lastSeq: types.length,
    events,
    nextAfter: types.length,
  };
}

/** A socket that never opens: the stream is not under test, only the wiring. */
class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  readonly url: string;
  readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }
  send(): void {
    // no client frames are needed by these tests
  }
  close(): void {
    this.readyState = 3;
  }
}

function fakeApi(overrides: Partial<ForgeApi> = {}): ForgeApi {
  const base: ForgeApi = {
    listSessions: vi.fn(async () => ({ sessions: [] })),
    getSession: vi.fn(async () => sessionDetail),
    events: vi.fn(async () => eventsPage(['session/start'])),
    postMessage: vi.fn(async () => ({ seq: 3, accepted: true, deduplicated: false })),
    cancel: vi.fn(async () => ({ cancelled: true, seq: 3 })),
    steer: vi.fn(async () => ({ seq: 4, accepted: true, deduplicated: false })),
    decide: vi.fn(async () => ({ approvalId: 'apr-1', decision: 'allow' as const, seq: 5 })),
    capabilities: vi.fn(async () => ({
      mode: 'workspace-write',
      workspace: 'C:/work',
      model: 'm1',
      plugins: [],
      experts: [],
      teams: [],
      bundles: [],
      skills: [],
      mcp: [],
    })),
    teams: vi.fn(async () => ({ boards: [] })),
    // No goal by default: the pill renders nothing unless a test asks for one,
    // which keeps the transcript assertions in this file independent of it.
    getGoal: vi.fn(async () => null),
  };
  return { ...base, ...overrides };
}

function mountView(api: ForgeApi) {
  return mount(
    <SessionView api={api} sessionId="sess-1" token="tok" onBack={() => undefined} />,
  );
}

function textareaOf(container: HTMLElement): HTMLTextAreaElement {
  const element = container.querySelector('textarea');
  if (element === null) throw new Error('no textarea');
  return element;
}

function keydown(input: HTMLTextAreaElement): void {
  interact(() => {
    input.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );
  });
}

function stopButton(container: HTMLElement): HTMLButtonElement {
  const button = container.querySelector('.composer button[type="button"]');
  if (button === null) throw new Error('no Stop button in the composer');
  return button as HTMLButtonElement;
}

beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.stubGlobal('WebSocket', FakeWebSocket);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('SessionView turn controls', () => {
  it('idle: Enter posts the message exactly once and never steers or cancels', async () => {
    const api = fakeApi();
    const view = mountView(api);
    const textarea = () => textareaOf(view.container);

    await waitFor(() => textarea().placeholder.includes('Message the agent'));
    typeInto(textarea(), 'hello agent');
    keydown(textarea());
    await waitFor(() => textarea().value === '');

    expect(api.postMessage).toHaveBeenCalledTimes(1);
    // The list is always passed, empty or not: the client is what decides to
    // omit the key, so a message with no files still sends byte-identical JSON
    // (see client.test.ts). Asserting the empty array here is asserting that
    // the store did not drop its argument on the way.
    expect(api.postMessage).toHaveBeenCalledWith('sess-1', 'hello agent', []);
    expect(api.steer).not.toHaveBeenCalled();
    expect(api.cancel).not.toHaveBeenCalled();
    view.unmount();
  });

  it('running: Enter steers the running turn with this session id and clears the draft', async () => {
    const api = fakeApi({
      events: vi.fn(async () => eventsPage(['session/start', 'turn/start'])),
    });
    const view = mountView(api);
    const textarea = () => textareaOf(view.container);

    // turn/start flips the store to running, which turns Enter into steer.
    await waitFor(() => textarea().placeholder.includes('Steer the running turn'));
    typeInto(textarea(), 'also check the retry path');
    keydown(textarea());
    await waitFor(() => textarea().value === '');

    expect(api.steer).toHaveBeenCalledTimes(1);
    expect(api.steer).toHaveBeenCalledWith('sess-1', 'also check the retry path');
    expect(api.postMessage).not.toHaveBeenCalled();
    expect(api.cancel).not.toHaveBeenCalled();
    view.unmount();
  });

  it('running: Stop cancels once, and a second click in the same tick does not double-cancel', async () => {
    let release!: () => void;
    const gate = new Promise<CancelResponse>((resolve) => {
      release = () => resolve({ cancelled: true, seq: 3 });
    });
    const api = fakeApi({
      events: vi.fn(async () => eventsPage(['session/start', 'turn/start'])),
      cancel: vi.fn(() => gate),
    });
    const view = mountView(api);
    const textarea = () => textareaOf(view.container);
    const stop = () => stopButton(view.container);

    await waitFor(() => textarea().placeholder.includes('Steer the running turn'));
    expect(stop().textContent).toBe('Stop');

    // Two clicks, one tick (no await between the dispatches). The first sets
    // `stopping` before awaiting, so React commits the disabled button and the
    // second click never reaches the handler. Note the guard is state, not a
    // ref (contrast ApprovalDialog): two clicks *batched into one act()* would
    // both run, because no commit happens between them. A browser never
    // batches two discrete click events into one task, so this is the
    // real-input guarantee.
    interact(() => {
      stop().dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    interact(() => {
      stop().dispatchEvent(new MouseEvent('click', { bubbles: true }));
    });
    expect(api.cancel).toHaveBeenCalledTimes(1);
    expect(api.cancel).toHaveBeenCalledWith('sess-1');
    expect(stop().textContent).toBe('stopping…');
    expect(stop().disabled).toBe(true);

    release();
    await waitFor(() => stop().textContent === 'Stop');
    expect(api.cancel).toHaveBeenCalledTimes(1);
    view.unmount();
  });

  it('running: a steer API error is visible as an alert banner and keeps the draft', async () => {
    const api = fakeApi({
      events: vi.fn(async () => eventsPage(['session/start', 'turn/start'])),
      steer: vi.fn(async () => {
        throw new ApiError('conflict', 'no turn is running', 409);
      }),
    });
    const view = mountView(api);
    const textarea = () => textareaOf(view.container);

    await waitFor(() => textarea().placeholder.includes('Steer the running turn'));
    typeInto(textarea(), 'retry with backoff');
    keydown(textarea());

    await waitFor(() =>
      (view.container.textContent ?? '').includes('conflict: no turn is running'),
    );
    const banner = view.container.querySelector('.banner-error');
    expect(banner).not.toBeNull();
    expect(banner?.getAttribute('role')).toBe('alert');
    expect(banner?.textContent).toContain('no turn is running');
    // The draft survives the failure, so the text is not lost with the error.
    expect(textarea().value).toBe('retry with backoff');
    expect(api.postMessage).not.toHaveBeenCalled();
    view.unmount();
    await flush();
  });
});

describe('SessionView compressed blocks', () => {
  const BLOCK = 'a'.repeat(32);

  /** Two records and the block that replaced them. */
  function compressedPage(): EventsPage {
    const events: EventEnvelope[] = [
      {
        v: 1,
        seq: 1,
        id: 'e1',
        ts: 1_700_000_000_000,
        session: 'sess-1',
        type: 'user/message',
        payload: { text: 'one' },
      },
      {
        v: 1,
        seq: 2,
        id: 'e2',
        ts: 1_700_000_001_000,
        session: 'sess-1',
        type: 'user/message',
        payload: { text: 'two' },
      },
      {
        v: 1,
        seq: 3,
        id: 'e3',
        ts: 1_700_000_002_000,
        session: 'sess-1',
        type: 'context/compressed',
        payload: { blockId: BLOCK, from: 1, to: 2, summary: 'a summary', messages: 2 },
      },
    ];
    return { sessionId: 'sess-1', from: 0, to: 3, lastSeq: 3, events, nextAfter: 3 };
  }

  it('says how many blocks are hidden from the model, next to the session title', async () => {
    const api = fakeApi({ events: vi.fn(async () => compressedPage()) });
    const view = mountView(api);

    await waitFor(() => (view.container.textContent ?? '').includes('hidden from the model'));
    const badge = view.container.querySelector('.session-compressed');
    expect(badge).not.toBeNull();
    expect(badge?.textContent).toBe('1 block hidden from the model');
    // The ids are on the title, so the operator can correlate with the note rows.
    expect(badge?.getAttribute('title')).toContain(BLOCK);
    view.unmount();
    await flush();
  });

  it('says nothing when no block was written', async () => {
    const api = fakeApi();
    const view = mountView(api);

    await waitFor(() => textareaOf(view.container).placeholder.includes('Message the agent'));
    expect(view.container.querySelector('.session-compressed')).toBeNull();
    view.unmount();
    await flush();
  });
});