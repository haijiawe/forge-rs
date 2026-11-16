// The projection and its generic fallback. events-v1.md §6 requires an old
// reader to survive a new event type: an unknown `future/thing` must render as
// itself, never vanish and never throw.
import { describe, expect, it } from 'vitest';
import type { EventEnvelope } from '../api/events';
import { RowAccumulator, projectRows, rowFromEvent } from './rows';
import { interact, mount } from '../test/render';
import { TranscriptRow } from '../components/TranscriptRow';

function envelope(overrides: Partial<EventEnvelope> & { type: string }): EventEnvelope {
  return {
    v: 1,
    seq: 1,
    id: 'evt-1',
    ts: 1_700_000_000_000,
    session: 'sess-1',
    ...overrides,
  };
}

describe('rowFromEvent', () => {
  it('turns a user message into a user row', () => {
    const row = rowFromEvent(envelope({ type: 'user/message', seq: 3, payload: { text: 'hello' } }));
    expect(row).toMatchObject({ kind: 'user', seq: 3, text: 'hello' });
  });

  it('turns an assistant message into an assistant row', () => {
    const row = rowFromEvent(
      envelope({ type: 'assistant/message', seq: 4, payload: { text: 'hi there', stopReason: 'end_turn' } }),
    );
    expect(row).toMatchObject({ kind: 'assistant', text: 'hi there', streaming: false });
  });

  it('turns a tool call into a running tool row and a result into the finished one', () => {
    const call = rowFromEvent(
      envelope({
        type: 'tool/call',
        seq: 5,
        payload: { callId: 'c1', tool: 'read_file', args: { path: 'a.txt' } },
      }),
    );
    expect(call).toMatchObject({ kind: 'tool', key: 'tool:c1', done: false, tool: 'read_file' });

    const result = rowFromEvent(
      envelope({
        type: 'tool/result',
        seq: 6,
        payload: { callId: 'c1', ok: false, output: 'boom', code: 'tool_failed', truncated: false },
      }),
    );
    expect(result).toMatchObject({ kind: 'tool', key: 'tool:c1', done: true, ok: false, errorCode: 'tool_failed' });
  });

  it('renders an unknown event type generically instead of dropping it', () => {
    const row = rowFromEvent(
      envelope({ type: 'future/thing', seq: 9, payload: { anything: [1, 2, 3] } }),
    );
    expect(row).toMatchObject({ kind: 'event', type: 'future/thing', seq: 9, folded: true });
    if (row === null || row.kind !== 'event') throw new Error('expected an event row');
    expect(row.payload).toEqual({ anything: [1, 2, 3] });
  });

  it('keeps an unknown event with no payload', () => {
    const row = rowFromEvent(envelope({ type: 'future/thing', seq: 10 }));
    expect(row).toMatchObject({ kind: 'event', payload: undefined });
  });
});

describe('unknown events through the transcript', () => {
  it('renders the type of an event it has no view for, and its payload on demand', () => {
    const rows = projectRows(
      [
        envelope({ type: 'user/message', seq: 1, payload: { text: 'hello' } }),
        envelope({ type: 'future/thing', seq: 2, payload: { why: 'a newer server wrote this' } }),
      ],
      [],
    );
    const unknown = rows.find((row) => row.kind === 'event');
    if (unknown === undefined) throw new Error('unknown event row is missing');
    const view = mount(<TranscriptRow row={unknown} />);
    expect(view.container.textContent).toContain('future/thing');
    // Folded by default: the row is visible, the payload is one click away.
    expect(view.container.textContent).not.toContain('a newer server wrote this');
    const button = view.container.querySelector('.link-button');
    interact(() => button?.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    expect(view.container.textContent).toContain('a newer server wrote this');
    view.unmount();
  });
});

describe('RowAccumulator', () => {
  it('merges a tool result into the call row by callId, keeping one card', () => {
    const accumulator = new RowAccumulator();
    accumulator.push(
      envelope({ type: 'tool/call', seq: 1, payload: { callId: 'c1', tool: 'shell', args: { cmd: 'ls' } } }),
    );
    accumulator.push(
      envelope({ type: 'tool/result', seq: 2, payload: { callId: 'c1', ok: true, output: 'a.txt' } }),
    );
    const rows = accumulator.rows;
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ kind: 'tool', callId: 'c1', done: true, ok: true, output: 'a.txt' });
  });

  it('keeps row keys stable when a row is updated', () => {
    const accumulator = new RowAccumulator();
    accumulator.push(envelope({ type: 'tool/call', seq: 1, payload: { callId: 'c1', tool: 'shell' } }));
    const before = accumulator.rowKeys();
    accumulator.push(envelope({ type: 'tool/result', seq: 2, payload: { callId: 'c1', ok: true } }));
    expect(accumulator.rowKeys()).toEqual(before);
  });

  it('appends advisory deltas after the events that back them', () => {
    const accumulator = new RowAccumulator();
    accumulator.push(envelope({ type: 'assistant/message', seq: 1, payload: { text: 'partial' } }));
    accumulator.setDeltas([{ key: '1:1', turn: 1, step: 1, seq: 2, text: 'more' }]);
    expect(accumulator.rows.map((row) => row.kind)).toEqual(['assistant', 'assistant']);
    accumulator.setDeltas([]);
    expect(accumulator.rows).toHaveLength(1);
  });
});