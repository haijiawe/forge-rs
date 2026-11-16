import { describe, expect, it } from 'vitest';
import type { Event, EventEnvelope } from './events';
import { StreamMachine } from './stream';
import type { DeltaFrame, EventsPage, EventFrame, SessionDetail, SnapshotFrame } from './types';

const PROTOCOL = 1;

function event(seq: number, type: string, payload?: unknown, extra: Partial<EventEnvelope> = {}): Event {
  return {
    v: 1,
    seq,
    id: `evt-${String(seq)}`,
    ts: 1_700_000_000_000 + seq,
    session: 'sess-1',
    type,
    ...(payload === undefined ? {} : { payload }),
    ...extra,
  };
}

function eventFrame(seq: number, type: string, payload?: unknown, extra: Partial<EventEnvelope> = {}): EventFrame {
  return { kind: 'event', protocol: PROTOCOL, event: event(seq, type, payload, extra) };
}

function snapshot(lastSeq: number): SnapshotFrame {
  const session: SessionDetail = {
    id: 'sess-1',
    createdAt: 1_700_000_000_000,
    updatedAt: 1_700_000_000_000,
    status: 'running',
    lastSeq,
    turns: 1,
    workspace: 'D:/work',
    model: 'deepseek-chat',
  };
  return { kind: 'snapshot', protocol: PROTOCOL, lastSeq, session };
}

function delta(seq: number, turn: number, step: number, text: string): DeltaFrame {
  return { kind: 'delta', protocol: PROTOCOL, turn, step, seq, text };
}

function page(from: number, to: number, lastSeq: number): EventsPage {
  const events: unknown[] = [];
  for (let seq = from; seq <= to; seq += 1) {
    events.push(event(seq, 'step/start', { step: seq }));
  }
  return { sessionId: 'sess-1', from, to, lastSeq, events, nextAfter: to };
}

function machine(): StreamMachine {
  const m = new StreamMachine('sess-1', PROTOCOL);
  m.apply({ kind: 'snapshot', frame: snapshot(0) });
  return m;
}

describe('StreamMachine ordering', () => {
  it('applies snapshot, then events, then deltas', () => {
    const m = machine();
    expect(m.state.session?.id).toBe('sess-1');

    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start', { turn: 1 }) });
    m.apply({ kind: 'event', frame: eventFrame(2, 'user/message', { text: 'hi' }) });
    m.apply({ kind: 'event', frame: eventFrame(3, 'assistant/message', { text: 'Hel' }) });
    expect(m.state.events.map((e) => e.seq)).toEqual([1, 2, 3]);
    expect(m.state.lastSeq).toBe(3);
    expect(m.state.status).toBe('running');

    // The advisory delta for the step still rendering is buffered.
    m.apply({ kind: 'delta', frame: delta(4, 1, 2, 'lo th') });
    m.apply({ kind: 'delta', frame: delta(4, 1, 2, 'ere') });
    expect(m.state.deltas).toHaveLength(1);
    expect(m.state.deltas[0]?.text).toBe('lo there');
  });

  it('drops buffered delta text once the authoritative event lands', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start', { turn: 1 }) });
    m.apply({ kind: 'delta', frame: delta(2, 1, 1, 'partial wor') });
    expect(m.state.deltas).toHaveLength(1);

    m.apply({
      kind: 'event',
      frame: eventFrame(2, 'assistant/message', { text: 'partial words' }, { turn: 1, step: 1 }),
    });
    expect(m.state.deltas).toHaveLength(0);
    expect(m.state.events.at(-1)?.seq).toBe(2);
  });

  it('drops a delta whose seq was claimed by a non-assistant event', () => {
    const m = machine();
    // A tool call at seq 1 means no assistant/message text can still be
    // streamed under seq 1: deltas are advisory and must be backed by an
    // assistant/message event at the same seq.
    m.apply({ kind: 'delta', frame: delta(1, 1, 1, 'orphan text') });
    m.apply({ kind: 'event', frame: eventFrame(1, 'tool/call', { callId: 'call-1', tool: 'read' }) });
    expect(m.state.deltas).toHaveLength(0);
  });

  it('ignores a delta below the applied seq', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start') });
    m.apply({ kind: 'delta', frame: delta(1, 1, 1, 'late text') });
    expect(m.state.deltas).toHaveLength(0);
  });

  it('keeps unknown event types and never rejects them', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'future/thing', { whatever: true }) });
    expect(m.state.events).toHaveLength(1);
    expect(m.state.events[0]?.type).toBe('future/thing');
    expect(m.state.lastSeq).toBe(1);
  });
});

describe('StreamMachine reconnect', () => {
  it('never applies an event twice', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start') });
    m.apply({ kind: 'event', frame: eventFrame(2, 'user/message', { text: 'hi' }) });
    m.apply({ kind: 'event', frame: eventFrame(3, 'turn/end') });

    // Reconnect with ?after=3 replays 2 and 3: both are duplicates.
    m.apply({ kind: 'event', frame: eventFrame(2, 'user/message', { text: 'hi' }) });
    m.apply({ kind: 'event', frame: eventFrame(3, 'turn/end') });
    expect(m.state.events.map((e) => e.seq)).toEqual([1, 2, 3]);
    expect(m.state.lastSeq).toBe(3);
    expect(m.state.needsResync).toBe(false);
  });

  it('reports a resync when a seq gap appears instead of applying it', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start') });
    m.apply({ kind: 'event', frame: eventFrame(2, 'step/start') });

    const effects = m.apply({ kind: 'event', frame: eventFrame(5, 'step/end') });
    expect(effects).toEqual([{ kind: 'resync', after: 2, reason: 'gap' }]);
    expect(m.state.needsResync).toBe(true);
    // The gaping event is not applied, so the cursor still points at 2.
    expect(m.state.lastSeq).toBe(2);
    expect(m.state.events).toHaveLength(2);
  });

  it('refetches from lastSeq and catches up without duplicating', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start') });
    m.apply({ kind: 'event', frame: eventFrame(2, 'step/start') });
    m.apply({ kind: 'event', frame: eventFrame(5, 'step/end') });
    expect(m.state.needsResync).toBe(true);

    // GET /events?after=2&limit=100 returns 3..5 (2 is not replayed).
    m.apply({ kind: 'page', page: page(3, 5, 5) });
    expect(m.state.events.map((e) => e.seq)).toEqual([1, 2, 3, 4, 5]);
    expect(m.state.lastSeq).toBe(5);
    expect(m.state.needsResync).toBe(false);
    expect(m.state.streamError).toBeNull();
  });

  it('keeps resyncing when the page does not reach headSeq', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start') });
    m.apply({ kind: 'event', frame: eventFrame(4, 'step/start') });

    // Page is truncated: 2..3 read, but the server claims lastSeq 4.
    const effects = m.apply({ kind: 'page', page: page(2, 3, 4) });
    expect(m.state.lastSeq).toBe(3);
    expect(m.state.needsResync).toBe(true);
    expect(effects).toEqual([{ kind: 'resync', after: 3, reason: 'gap' }]);

    m.apply({ kind: 'page', page: page(4, 4, 4) });
    expect(m.state.lastSeq).toBe(4);
    expect(m.state.needsResync).toBe(false);
  });

  it('a page with a hole inside it keeps the stream untrustworthy', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start') });
    const holed: EventsPage = {
      sessionId: 'sess-1',
      from: 2,
      to: 4,
      lastSeq: 4,
      events: [event(2, 'step/start'), event(4, 'step/end')],
      nextAfter: 4,
    };
    m.apply({ kind: 'page', page: holed });
    expect(m.state.lastSeq).toBe(2);
    expect(m.state.needsResync).toBe(true);
    expect(m.state.streamError).toContain('jumps from seq 2 to 4');
  });

  it('treats a protocol mismatch as a resync, not a crash', () => {
    const m = machine();
    const effects = m.apply({ kind: 'event', frame: { ...eventFrame(1, 'turn/start'), protocol: 2 } });
    expect(effects[0]?.kind).toBe('resync');
    expect(m.state.needsResync).toBe(true);
    expect(m.state.streamError).toContain('protocol 2');
  });

  it('reports a fatal frame and stops trusting the stream', () => {
    const m = machine();
    const effects = m.apply({
      kind: 'stream-error',
      frame: { kind: 'error', protocol: PROTOCOL, fatal: true, error: { code: 'internal_error', message: 'boom' } },
    });
    expect(effects).toEqual([{ kind: 'fatal', message: 'boom' }]);
    expect(m.state.streamError).toBe('boom');
  });
});

describe('StreamMachine approvals and status', () => {
  it('surfaces an approval request and clears it on the decision', () => {
    const m = machine();
    m.apply({ kind: 'event', frame: eventFrame(1, 'turn/start') });
    m.apply({
      kind: 'event',
      frame: eventFrame(2, 'approval/request', {
        approvalId: 'ap-1',
        callId: 'call-1',
        tool: 'exec',
        args: { argv: ['git', 'status'] },
      }),
    });
    expect(m.state.status).toBe('awaiting_approval');
    expect(m.state.session?.pendingApproval?.approvalId).toBe('ap-1');
    expect(m.state.session?.pendingApproval?.tool).toBe('exec');

    m.apply({ kind: 'event', frame: eventFrame(3, 'approval/decision', { approvalId: 'ap-1', decision: 'allow' }) });
    expect(m.state.session?.pendingApproval).toBeUndefined();
    expect(m.state.status).toBe('running');
  });
});