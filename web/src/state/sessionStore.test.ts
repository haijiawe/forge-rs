// The store's turn-control verbs, the ones SessionView hands to the Composer:
// steerTurn (§4.10) and cancelTurn (§4.9). The Composer owns *when* to call
// them; the store owns *what* reaches the wire and how a failure is reported.
//
// No start() here on purpose: these are mutation paths, and the stream machine
// is covered by api/stream.test.ts. Frame batching is neutralised with a
// synchronous scheduleFrame so a failure is observable without timers.
//
// The compressed-block describe at the end is the exception: it drives start()
// once, because building the block list is a flush-time job and the wiring from
// log to marked row is exactly what it has to prove.
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api/client';
import type { ForgeApi } from '../api/client';
import type { EventsPage, SessionDetail } from '../api/types';
import type { EventEnvelope } from '../api/events';
import { SessionStore } from './sessionStore';

const sessionDetail: SessionDetail = {
  id: 'sess-1',
  createdAt: 1_700_000_000_000,
  updatedAt: 1_700_000_001_000,
  status: 'idle',
  lastSeq: 1,
  turns: 0,
  workspace: 'C:\\work\\repo',
  model: 'deepseek-v4',
};

const emptyPage: EventsPage = {
  sessionId: 'sess-1',
  from: 0,
  to: 0,
  lastSeq: 0,
  events: [],
  nextAfter: 0,
};

function fakeApi(overrides: Partial<ForgeApi> = {}): ForgeApi {
  const base: ForgeApi = {
    listSessions: vi.fn(async () => ({ sessions: [] })),
    getSession: vi.fn(async () => sessionDetail),
    events: vi.fn(async () => emptyPage),
    postMessage: vi.fn(async () => ({ seq: 2, accepted: true, deduplicated: false })),
    cancel: vi.fn(async () => ({ cancelled: true, seq: 2 })),
    steer: vi.fn(async () => ({ seq: 3, accepted: true, deduplicated: false })),
    decide: vi.fn(async () => ({ approvalId: 'apr-1', decision: 'allow' as const, seq: 4 })),
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
  };
  return { ...base, ...overrides };
}

function storeFor(api: ForgeApi): SessionStore {
  return new SessionStore({
    api,
    sessionId: 'sess-1',
    token: 'tok',
    // Publish inline instead of waiting for a frame: no fake timers needed.
    scheduleFrame: () => 0,
  });
}

describe('SessionStore.steerTurn', () => {
  it('posts the trimmed text to /steer for this session and never to /messages', async () => {
    const api = fakeApi();
    const store = storeFor(api);

    await expect(store.steerTurn('  also check the retry path  ')).resolves.toBe(true);

    expect(api.steer).toHaveBeenCalledTimes(1);
    expect(api.steer).toHaveBeenCalledWith('sess-1', 'also check the retry path');
    expect(api.postMessage).not.toHaveBeenCalled();
    expect(store.getSnapshot().actionError).toBeNull();
  });

  it('does not call the API for empty or whitespace-only text', async () => {
    const api = fakeApi();
    const store = storeFor(api);

    await expect(store.steerTurn('')).resolves.toBe(false);
    await expect(store.steerTurn('  \n\t ')).resolves.toBe(false);

    expect(api.steer).not.toHaveBeenCalled();
    expect(store.getSnapshot().actionError).toBeNull();
  });

  it('reports a 409 conflict as actionError and resolves false', async () => {
    const api = fakeApi({
      steer: vi.fn(async () => {
        throw new ApiError('conflict', 'no turn is running', 409);
      }),
    });
    const store = storeFor(api);

    await expect(store.steerTurn('too late')).resolves.toBe(false);

    // `${code}: ${message}` is what SessionView renders in its role="alert".
    expect(store.getSnapshot().actionError).toBe('conflict: no turn is running');
    // A failed steer must not change the derived status by itself.
    expect(store.getSnapshot().status).toBe('idle');
  });

  it('surfaces a non-ApiError failure by its message', async () => {
    const api = fakeApi({
      steer: vi.fn(async () => {
        throw new Error('socket hang up');
      }),
    });
    const store = storeFor(api);

    await expect(store.steerTurn('still there?')).resolves.toBe(false);
    expect(store.getSnapshot().actionError).toBe('socket hang up');
  });
});

describe('SessionStore.cancelTurn', () => {
  it('posts to /cancel for this session and resolves true', async () => {
    const api = fakeApi();
    const store = storeFor(api);

    await expect(store.cancelTurn()).resolves.toBe(true);

    expect(api.cancel).toHaveBeenCalledTimes(1);
    expect(api.cancel).toHaveBeenCalledWith('sess-1');
    // Cancel is the server's verb: the store must not write or guess the ending.
    expect(api.postMessage).not.toHaveBeenCalled();
    expect(api.steer).not.toHaveBeenCalled();
    expect(store.getSnapshot().actionError).toBeNull();
  });

  it('reports a failed cancel instead of pretending the turn stopped', async () => {
    const api = fakeApi({
      cancel: vi.fn(async () => {
        throw new ApiError('not_found', 'session sess-1 does not exist', 404);
      }),
    });
    const store = storeFor(api);

    await expect(store.cancelTurn()).resolves.toBe(false);
    expect(store.getSnapshot().actionError).toBe('not_found: session sess-1 does not exist');
  });
});

/** A socket that never opens: these tests are about the flush, not the wire. */
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
    // the store sends a resume frame at most; not under test here
  }
  close(): void {
    this.readyState = 3;
  }
}

function envelope(seq: number, type: string, payload?: unknown): EventEnvelope {
  return {
    v: 1,
    seq,
    id: `e${String(seq)}`,
    ts: 1_700_000_000_000 + seq,
    session: 'sess-1',
    type,
    ...(payload === undefined ? {} : { payload }),
  };
}

async function until(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error('until: the condition never held');
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

describe('SessionStore flush with compressed blocks', () => {
  const BLOCK = 'ffffffffffffffffffffffffffffffff';

  function pageOf(events: EventEnvelope[]): EventsPage {
    const lastSeq = events.length;
    return { sessionId: 'sess-1', from: 1, to: lastSeq, lastSeq, events, nextAfter: lastSeq };
  }

  function storeThatFlushes(api: ForgeApi): SessionStore {
    return new SessionStore({
      api,
      sessionId: 'sess-1',
      token: 'tok',
      // A real macrotask: the flush happens off the loadInitial promise, so the
      // test observes the same ordering the browser gets.
      scheduleFrame: (callback) => setTimeout(callback, 0),
    });
  }

  it('publishes the blocks and marks the rows they hide', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket);
    try {
      const api = fakeApi({
        events: vi.fn(async () =>
          pageOf([
            envelope(1, 'user/message', { text: 'one' }),
            envelope(2, 'user/message', { text: 'two' }),
            envelope(3, 'user/message', { text: 'three' }),
            envelope(4, 'context/compressed', { blockId: BLOCK, from: 2, to: 3, summary: 'a summary', messages: 2 }),
          ]),
        ),
      });
      const store = storeThatFlushes(api);
      store.start();
      await until(() => store.getSnapshot().rows.length >= 4);

      const snapshot = store.getSnapshot();
      expect(snapshot.blocks).toEqual([
        { blockId: BLOCK, from: 2, to: 3, seq: 4, toBeyondHead: false, messages: 2 },
      ]);
      expect(snapshot.rows.map((row) => row.kind)).toEqual(['user', 'user', 'user', 'compressed']);
      // The note itself is not a record the model was sent, so it is not marked.
      expect(snapshot.rows.filter((row) => row.foldedBy !== undefined).map((row) => row.seq)).toEqual([2, 3]);
      store.stop();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('keeps a range that runs past lastSeq open instead of clamping it', async () => {
    vi.stubGlobal('WebSocket', FakeWebSocket);
    try {
      const api = fakeApi({
        events: vi.fn(async () =>
          pageOf([
            envelope(1, 'user/message', { text: 'one' }),
            envelope(2, 'user/message', { text: 'two' }),
            // `to` is past every seq this window has been sent: the writer
            // covered records that have not arrived here yet.
            envelope(3, 'context/compressed', { blockId: BLOCK, from: 1, to: 9, summary: 'a summary' }),
          ]),
        ),
      });
      const store = storeThatFlushes(api);
      store.start();
      await until(() => store.getSnapshot().rows.length >= 3);

      const snapshot = store.getSnapshot();
      expect(snapshot.blocks[0]).toMatchObject({ from: 1, to: 9, toBeyondHead: true });
      // Nothing is clamped: the seqs that do exist are marked, and the range stays
      // as written so seqs 4..9 are marked when they arrive.
      expect(snapshot.rows.filter((row) => row.foldedBy !== undefined).map((row) => row.seq)).toEqual([1, 2]);
      store.stop();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});