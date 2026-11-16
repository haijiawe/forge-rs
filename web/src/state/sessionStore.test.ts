// The store's turn-control verbs, the ones SessionView hands to the Composer:
// steerTurn (§4.10) and cancelTurn (§4.9). The Composer owns *when* to call
// them; the store owns *what* reaches the wire and how a failure is reported.
//
// No start() here on purpose: these are mutation paths, and the stream machine
// is covered by api/stream.test.ts. Frame batching is neutralised with a
// synchronous scheduleFrame so a failure is observable without timers.
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api/client';
import type { ForgeApi } from '../api/client';
import type { EventsPage, SessionDetail } from '../api/types';
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