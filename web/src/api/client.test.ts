// The client's two security-critical behaviours, and its wire headers:
//   * ?token= is read once, kept in sessionStorage (never localStorage) and
//     removed from the URL, so it cannot leak into history or a screenshot.
//   * every request carries X-Hearth-Protocol, and the Bearer header only when
//     a token exists (the server also accepts an HttpOnly cookie).
import { describe, expect, it, vi } from 'vitest';
import {
  ApiError,
  HearthClient,
  TOKEN_STORAGE_KEY,
  captureToken,
  streamUrl,
} from './client';
import { HeaderVersion, IdempotencyKeyHeader, Version } from './protocol';

class MemoryStorage {
  private readonly map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
}

function jsonResponse(body: unknown, init: { status?: number; headers?: Record<string, string> } = {}): Response {
  return new Response(JSON.stringify(body), {
    status: init.status ?? 200,
    headers: { 'Content-Type': 'application/json', ...(init.headers ?? {}) },
  });
}

describe('captureToken', () => {
  it('stores the URL token for this tab and strips it from the address bar', () => {
    const storage = new MemoryStorage();
    const replaced: string[] = [];
    const token = captureToken({
      location: { href: '', search: '?token=secret&x=1', pathname: '/', hash: '#/s/abc' },
      storage,
      replaceState: (url) => replaced.push(url),
    });
    expect(token).toBe('secret');
    expect(storage.getItem(TOKEN_STORAGE_KEY)).toBe('secret');
    expect(replaced).toEqual(['/?x=1#/s/abc']);
  });

  it('drops the query entirely when the token was the only parameter', () => {
    const replaced: string[] = [];
    captureToken({
      location: { href: '', search: '?token=secret', pathname: '/', hash: '' },
      storage: new MemoryStorage(),
      replaceState: (url) => replaced.push(url),
    });
    expect(replaced).toEqual(['/']);
  });

  it('falls back to the stored token on reload', () => {
    const storage = new MemoryStorage();
    storage.setItem(TOKEN_STORAGE_KEY, 'kept');
    const token = captureToken({
      location: { href: '', search: '', pathname: '/', hash: '' },
      storage,
      replaceState: () => undefined,
    });
    expect(token).toBe('kept');
  });

  it('returns an empty string when no token is available', () => {
    expect(
      captureToken({
        location: { href: '', search: '', pathname: '/', hash: '' },
        storage: new MemoryStorage(),
        replaceState: () => undefined,
      }),
    ).toBe('');
  });

  it('never writes to localStorage', () => {
    const localStorageSpy = vi.spyOn(Storage.prototype, 'setItem');
    try {
      captureToken({
        location: { href: '', search: '?token=abc', pathname: '/', hash: '' },
        storage: new MemoryStorage(),
        replaceState: () => undefined,
      });
      expect(localStorageSpy).not.toHaveBeenCalled();
    } finally {
      localStorageSpy.mockRestore();
    }
  });
});

describe('HearthClient', () => {
  it('sends the protocol header and the bearer token', async () => {
    const fetchImpl = vi.fn<typeof fetch>(async () => jsonResponse({ sessions: [], nextCursor: undefined }));
    const client = new HearthClient({ token: 'tok', fetchImpl });
    await client.listSessions();
    const init = fetchImpl.mock.calls[0]?.[1] ?? {};
    const headers = init.headers as Record<string, string>;
    expect(headers[HeaderVersion]).toBe(String(Version));
    expect(headers['Authorization']).toBe('Bearer tok');
  });

  it('omits the bearer header when there is no token (cookie auth)', async () => {
    const fetchImpl = vi.fn<typeof fetch>(async () => jsonResponse({ sessions: [] }));
    const client = new HearthClient({ token: '', fetchImpl });
    await client.listSessions();
    const init = fetchImpl.mock.calls[0]?.[1] ?? {};
    expect((init.headers as Record<string, string>)['Authorization']).toBeUndefined();
  });

  it('maps an error envelope onto ApiError', async () => {
    const fetchImpl = vi.fn<typeof fetch>(async () =>
      jsonResponse({ error: { code: 'unauthorized', message: 'bad token' } }, { status: 401 }),
    );
    const client = new HearthClient({ token: 'x', fetchImpl });
    await expect(client.getSession('s1')).rejects.toMatchObject({
      name: 'ApiError',
      code: 'unauthorized',
      message: 'bad token',
      status: 401,
    });
    await expect(client.getSession('s1')).rejects.toBeInstanceOf(ApiError);
  });

  it('sends an Idempotency-Key with a posted message', async () => {
    const fetchImpl = vi.fn<typeof fetch>(async () => jsonResponse({ seq: 4, accepted: true, deduplicated: false }));
    const client = new HearthClient({ token: 'tok', fetchImpl });
    await client.postMessage('s1', 'hello');
    const init = fetchImpl.mock.calls[0]?.[1] ?? {};
    const headers = init.headers as Record<string, string>;
    expect(headers[IdempotencyKeyHeader]).toBeTypeOf('string');
    expect(headers[IdempotencyKeyHeader]?.length).toBeGreaterThan(8);
    expect(init.body).toBe(JSON.stringify({ text: 'hello' }));
  });

  it('posts a decision with and without a note', async () => {
    const fetchImpl = vi.fn<typeof fetch>(async () => jsonResponse({ approvalId: 'a1', decision: 'allow', seq: 9 }));
    const client = new HearthClient({ token: 'tok', fetchImpl });
    await client.decide('s1', 'a1', 'allow');
    await client.decide('s1', 'a1', 'deny', '   ');
    const bodies = fetchImpl.mock.calls.map((call) => call[1]?.body);
    expect(bodies).toEqual([JSON.stringify({ decision: 'allow' }), JSON.stringify({ decision: 'deny' })]);
  });

  it('builds the stream URL with the last applied seq', () => {
    expect(streamUrl('http://127.0.0.1:8848', 'sess 1', 42)).toBe(
      'ws://127.0.0.1:8848/api/sessions/sess%201/stream?after=42',
    );
    expect(streamUrl('https://hearth.local', 'a', 0)).toBe('wss://hearth.local/api/sessions/a/stream?after=0');
  });
});