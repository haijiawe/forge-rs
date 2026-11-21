import { describe, expect, it } from 'vitest';
import { describeState, flatten, formatSettled, parseTree } from './subagents';

describe('parseTree', () => {
  it('reads a full answer', () => {
    const tree = parseTree({
      session: 'root',
      count: 1,
      subagents: [
        {
          sessionId: 'child-a',
          name: 'agent-1',
          mode: 'one-shot',
          label: 'build',
          path: 'lead/agent-1',
          depth: 1,
          timing: { settledMs: 1500, active: false, lastTurnCompleted: 1763643003300, turns: 2 },
          children: [],
        },
      ],
    });
    expect(tree.session).toBe('root');
    expect(tree.count).toBe(1);
    expect(tree.subagents[0]?.name).toBe('agent-1');
    expect(tree.subagents[0]?.timing?.settledMs).toBe(1500);
    expect(tree.subagents[0]?.timing?.turns).toBe(2);
  });

  it('degrades to an empty tree rather than throwing', () => {
    expect(parseTree(null)).toEqual({ session: '', count: 0, subagents: [] });
    expect(parseTree('nope')).toEqual({ session: '', count: 0, subagents: [] });
    expect(parseTree({ subagents: 'nope' }).subagents).toEqual([]);
  });

  it('drops a row with no session id instead of rendering a blank one', () => {
    const tree = parseTree({ session: 'root', subagents: [{ mode: 'one-shot' }, { sessionId: 'ok' }] });
    expect(tree.subagents).toHaveLength(1);
    expect(tree.subagents[0]?.sessionId).toBe('ok');
  });

  it('keeps a diagnostic so a broken child stays visible', () => {
    const tree = parseTree({
      session: 'root',
      subagents: [
        { sessionId: 'ghost', path: 'lead/agent-1', diagnostic: { kind: 'unavailable', detail: 'no log' } },
      ],
    });
    expect(tree.subagents[0]?.diagnostic?.kind).toBe('unavailable');
    expect(tree.subagents[0]?.timing).toBeUndefined();
  });

  it('computes the count when the server omits it', () => {
    const tree = parseTree({
      session: 'root',
      subagents: [
        { sessionId: 'a', children: [{ sessionId: 'b', children: [] }] },
      ],
    });
    expect(tree.count).toBe(2);
  });
});

describe('flatten', () => {
  it('walks depth first so a parent is above its children', () => {
    const tree = parseTree({
      session: 'root',
      subagents: [
        { sessionId: 'a', children: [{ sessionId: 'a1', children: [] }] },
        { sessionId: 'b', children: [] },
      ],
    });
    expect(flatten(tree.subagents).map((row) => row.sessionId)).toEqual(['a', 'a1', 'b']);
  });
});

describe('formatSettled', () => {
  it('says milliseconds below a second and seconds above', () => {
    expect(formatSettled(420)).toBe('420 ms');
    expect(formatSettled(1500)).toBe('1.5 s');
    expect(formatSettled(15000)).toBe('15 s');
  });

  it('does not turn a missing number into a duration', () => {
    expect(formatSettled(-1)).toBe('—');
    expect(formatSettled(Number.NaN)).toBe('—');
  });
});

describe('describeState', () => {
  it('names the diagnostic when there is one', () => {
    expect(
      describeState({
        sessionId: 'x',
        mode: 'one-shot',
        path: 'lead/agent-1',
        depth: 1,
        children: [],
        diagnostic: { kind: 'corrupt', detail: 'line 2' },
      }),
    ).toBe('corrupt: line 2');
  });

  it('says working while the child is active', () => {
    expect(
      describeState({
        sessionId: 'x',
        mode: 'one-shot',
        path: 'lead/agent-1',
        depth: 1,
        children: [],
        timing: { settledMs: 0, active: true, turns: 1 },
      }),
    ).toBe('正在工作');
  });

  it('reports the settled duration and the turn count', () => {
    expect(
      describeState({
        sessionId: 'x',
        mode: 'one-shot',
        path: 'lead/agent-1',
        depth: 1,
        children: [],
        timing: { settledMs: 1500, active: false, turns: 2 },
      }),
    ).toBe('已在 1.5 s 内结束，共 2 轮');
  });

  it('says no log yet rather than settled in 0 ms', () => {
    expect(
      describeState({
        sessionId: 'x',
        mode: 'one-shot',
        path: 'lead/agent-1',
        depth: 1,
        children: [],
      }),
    ).toBe('还没有日志');
  });
});