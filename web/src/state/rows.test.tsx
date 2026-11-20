// The projection and its generic fallback. events-v1.md §6 requires an old
// reader to survive a new event type: an unknown `future/thing` must render as
// itself, never vanish and never throw.
import { describe, expect, it } from 'vitest';
import type { EventEnvelope } from '../api/events';
import { RowAccumulator, blockHiding, compressedBlocks, projectRows, rowFromEvent } from './rows';
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

// `context/compressed` (§4.0.3): the model was sent `summary` instead of the
// records in seq `from..to`. The transcript must show both halves of that — the
// note, and which rows it took away — and it must never lose the event, however
// odd the payload is (§4: payload schemas are descriptive, not enforced).
describe('context/compressed rows', () => {
  const BLOCK = '0123456789abcdef0123456789abcdef';

  function block(seq: number, payload: unknown): EventEnvelope {
    return envelope({ type: 'context/compressed', seq, payload });
  }

  function messages(seqs: readonly number[]): EventEnvelope[] {
    return seqs.map((seq) => envelope({ type: 'user/message', seq, payload: { text: `m${String(seq)}` } }));
  }

  it('renders a well-formed block as a note naming the hidden range', () => {
    const row = rowFromEvent(
      block(41, { blockId: BLOCK, from: 12, to: 40, summary: 'a summary the model wrote', messages: 29 }),
    );
    if (row === null || row.kind !== 'compressed') throw new Error('expected a compressed row');
    expect(row).toMatchObject({
      kind: 'compressed',
      blockId: BLOCK,
      from: 12,
      to: 40,
      summary: 'a summary the model wrote',
      messages: 29,
      incomplete: false,
    });
    expect(row.problem).toBeUndefined();

    const view = mount(<TranscriptRow row={row} />);
    expect(view.container.textContent).toContain('summary replaced seq 12..40 (29 records)');
    expect(view.container.querySelector('.row-compressed')).not.toBeNull();
    expect(view.container.querySelector('.row-compressed-incomplete')).toBeNull();
    view.unmount();
  });

  it('counts the range itself when the payload carries no message count', () => {
    const row = rowFromEvent(block(41, { blockId: BLOCK, from: 12, to: 40, summary: 's' }));
    if (row === null || row.kind !== 'compressed') throw new Error('expected a compressed row');
    expect(row.messages).toBeUndefined();
    const view = mount(<TranscriptRow row={row} />);
    expect(view.container.textContent).toContain('summary replaced seq 12..40 (29 records)');
    view.unmount();
  });

  it('shows a block with no summary as an incomplete note instead of dropping it', () => {
    const row = rowFromEvent(block(41, { blockId: BLOCK, from: 12, to: 40, messages: 29 }));
    if (row === null || row.kind !== 'compressed') throw new Error('expected a compressed row');
    expect(row).toMatchObject({ kind: 'compressed', incomplete: true, summary: '' });
    expect(row.problem).toContain('no summary');

    const view = mount(<TranscriptRow row={row} />);
    expect(view.container.textContent).toContain('summary missing for seq 12..40 (29 records)');
    expect(view.container.textContent).toContain('incomplete');
    expect(view.container.querySelector('.row-compressed-incomplete')).not.toBeNull();
    view.unmount();
  });

  it('keeps a nonsense range on screen: from after to is incomplete, not fatal', () => {
    const row = rowFromEvent(block(41, { blockId: BLOCK, from: 40, to: 12, summary: 's' }));
    if (row === null || row.kind !== 'compressed') throw new Error('expected a compressed row');
    expect(row).toMatchObject({ kind: 'compressed', from: 40, to: 12, incomplete: true });
    expect(row.problem).toContain('from is after to');

    // A range that cannot be read cannot hide anything: it is a note, not a block.
    expect(compressedBlocks([block(41, { blockId: BLOCK, from: 40, to: 12, summary: 's' })])).toEqual([]);
  });

  it('survives a payload that is not even an object', () => {
    const rows = projectRows([
      envelope({ type: 'user/message', seq: 1, payload: { text: 'hello' } }),
      block(2, 'not-an-object'),
    ]);
    expect(rows.map((row) => row.kind)).toEqual(['user', 'compressed']);
    const note = rows[1];
    if (note === undefined || note.kind !== 'compressed') throw new Error('expected a compressed row');
    expect(note.incomplete).toBe(true);
    expect(note.problem).toContain('no blockId');
    expect(note.problem).toContain('no from seq');
    expect(note.problem).toContain('no to seq');
  });

  it('marks the rows inside the range folded, boundary seqs included', () => {
    const events = messages([1, 2, 3, 4, 5]);
    events.push(block(6, { blockId: BLOCK, from: 2, to: 4, summary: 'a summary' }));
    expect(projectRows(events).map((row) => row.foldedBy ?? null)).toEqual([
      null,
      BLOCK,
      BLOCK,
      BLOCK,
      null,
      null,
    ]);
  });

  it('marks nothing when the log wrote no block', () => {
    const events = messages([1, 2, 3]);
    expect(compressedBlocks(events)).toEqual([]);
    expect(projectRows(events).map((row) => row.foldedBy ?? null)).toEqual([null, null, null]);
  });

  it('does not mark the note with its own range', () => {
    // A block event whose seq falls inside its own range cannot be built by a
    // conforming writer (§4.0.3: the range is closed before the block is
    // written), but a reader must not draw a note that hides itself either.
    const events = messages([1, 2, 4, 5]);
    events.splice(2, 0, block(3, { blockId: BLOCK, from: 1, to: 5, summary: 'a summary' }));
    expect(projectRows(events).map((row) => row.foldedBy ?? null)).toEqual([
      BLOCK,
      BLOCK,
      null,
      BLOCK,
      BLOCK,
    ]);
  });

  it('marks rows that arrive later when a block runs past the last seq seen', () => {
    // `to` past the newest seq is not an error: the writer covered records this
    // window has not been sent. The range is kept as written and re-applied, so
    // seqs that arrive later are marked too instead of looking sent.
    const early = messages([1, 2, 3]);
    early.push(block(4, { blockId: BLOCK, from: 2, to: 9, summary: 'a summary' }));
    const blocksBefore = compressedBlocks(early);
    expect(blocksBefore).toHaveLength(1);
    expect(blocksBefore[0]?.toBeyondHead).toBe(true);

    const accumulator = new RowAccumulator();
    for (const event of early) accumulator.push(event);
    accumulator.setBlocks(blocksBefore);
    expect(accumulator.rows.map((row) => row.foldedBy ?? null)).toEqual([null, BLOCK, BLOCK, null]);

    for (const event of messages([5, 6])) accumulator.push(event);
    const blocksAfter = compressedBlocks([...early, ...messages([5, 6])]);
    expect(blocksAfter[0]?.toBeyondHead).toBe(true);
    accumulator.setBlocks(blocksAfter);
    expect(accumulator.rows.map((row) => row.foldedBy ?? null)).toEqual([
      null,
      BLOCK,
      BLOCK,
      null,
      BLOCK,
      BLOCK,
    ]);
  });

  it('does not move rows or keys when only the fold marks change', () => {
    const accumulator = new RowAccumulator();
    for (const event of messages([1, 2, 3])) accumulator.push(event);
    // A copy: `rows` hands back the internal array, and the fold pass rewrites
    // entries in place, so the snapshot has to be taken before it runs.
    const keys = accumulator.rowKeys();
    const before = [...accumulator.rows];
    accumulator.setBlocks(compressedBlocks([block(4, { blockId: BLOCK, from: 2, to: 3, summary: 's' })]));
    const after = accumulator.rows;
    expect(accumulator.rowKeys()).toEqual(keys);
    // Only the marked row is a new object: the untouched ones keep their identity,
    // so a memoized row view does not re-render when a block lands elsewhere.
    expect(after).toHaveLength(before.length);
    expect(after[0]).toBe(before[0]);
    expect(after[1]).not.toBe(before[1]);
    expect(after[1]?.foldedBy).toBe(BLOCK);
    expect(after[2]?.foldedBy).toBe(BLOCK);
  });

  it('finds the block hiding a seq by bisection, and nothing outside a range', () => {
    const blocks = compressedBlocks([
      block(11, { blockId: 'a'.repeat(32), from: 1, to: 5, summary: 's' }),
      block(12, { blockId: 'b'.repeat(32), from: 6, to: 9, summary: 's' }),
    ]);
    expect(blocks.map((entry) => entry.from)).toEqual([1, 6]);
    expect(blockHiding(0, blocks)).toBeUndefined();
    expect(blockHiding(1, blocks)?.blockId).toBe('a'.repeat(32));
    expect(blockHiding(5, blocks)?.blockId).toBe('a'.repeat(32));
    expect(blockHiding(6, blocks)?.blockId).toBe('b'.repeat(32));
    expect(blockHiding(9, blocks)?.blockId).toBe('b'.repeat(32));
    expect(blockHiding(10, blocks)).toBeUndefined();
  });
});