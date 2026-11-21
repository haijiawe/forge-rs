// Smoke test of the whole tree: the app mounts, reads the hash route, renders
// the session list and a session screen without a runtime error. jsdom has no
// layout, so the transcript renders an empty window; the point here is wiring,
// not geometry (geometry is covered in Transcript.test.tsx).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import { mount, waitFor } from './test/render';

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const session = {
  id: 'sess-1',
  createdAt: 1_700_000_000_000,
  updatedAt: 1_700_000_001_000,
  status: 'idle',
  lastSeq: 2,
  turns: 1,
  workspace: 'C:\\work\\repo',
  model: 'deepseek-v4',
};

beforeEach(() => {
  window.location.hash = '';
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/api/sessions/sess-1/events')) {
        return jsonResponse({
          sessionId: 'sess-1',
          from: 0,
          to: 2,
          lastSeq: 2,
          events: [
            { v: 1, seq: 1, id: 'e1', ts: 1_700_000_000_000, session: 'sess-1', type: 'user/message', payload: { text: 'hello' } },
            { v: 1, seq: 2, id: 'e2', ts: 1_700_000_001_000, session: 'sess-1', type: 'assistant/message', payload: { text: 'hi' } },
          ],
          nextAfter: 2,
        });
      }
      if (url.includes('/api/sessions/sess-1')) return jsonResponse(session);
      return jsonResponse({ sessions: [session] });
    }),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  window.location.hash = '';
});

describe('App', () => {
  it('renders the session list from the API', async () => {
    const view = mount(<App baseUrl="" />);
    const rendered = () => view.container.textContent ?? '';
    await waitFor(() => rendered().includes('sess-1'));
    const text = rendered();
    expect(text).toContain('会话');
    expect(text).toContain('sess-1');
    expect(text).toContain('deepseek-v4');
    expect(text).toContain('work/repo');
    view.unmount();
  });

  it('routes #/s/<id> to the session screen', async () => {
    window.location.hash = '#/s/sess-1';
    const view = mount(<App baseUrl="" />);
    const rendered = () => view.container.textContent ?? '';
    await waitFor(() => rendered().includes('hello') && rendered().includes('序号'));
    const text = rendered();
    expect(text).toContain('hello');
    expect(text).toContain('hi');
    expect(text).toContain('序号');
    view.unmount();
  });
});