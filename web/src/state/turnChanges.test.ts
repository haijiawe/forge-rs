// The fold from log events to "what did each turn change".
//
// These tests pin the three claims the pane makes: a turn exists only if a write
// reported lines, a path written twice in one turn is one row whose ranges
// describe the last write, and a malformed payload is skipped rather than
// rendered as a row nobody can trust.
import { describe, expect, it } from 'vitest';
import type { Event, JsonValue } from '../api/events';
import { latestWrite, rangeLabel, turnChangesFrom } from './turnChanges';

let seq = 0;

function result(turn: number | undefined, meta: Record<string, JsonValue>, type = 'tool/result'): Event {
  seq += 1;
  return {
    v: 1,
    seq,
    id: `e${String(seq)}`,
    ts: 1,
    session: 's1',
    ...(turn === undefined ? {} : { turn }),
    type,
    payload: { tool: 'fs_write', ok: true, meta },
  } as Event;
}

function lines(added: number, removed: number, ranges: Array<[number, number]>, extra: Record<string, JsonValue> = {}): Record<string, JsonValue> {
  return {
    added,
    removed,
    ranges: ranges.map(([from, to]) => ({ from, to })),
    ...extra,
  };
}

describe('turnChangesFrom', () => {
  it('groups a turn\'s writes under one turn, in the order they happened', () => {
    const turns = turnChangesFrom([
      result(1, { path: 'src/a.rs', lines: lines(2, 1, [[3, 4]]) }),
      result(1, { path: 'src/b.rs', lines: lines(1, 0, [[9, 9]]) }),
    ]);
    expect(turns).toHaveLength(1);
    expect(turns[0]?.turn).toBe(1);
    expect(turns[0]?.files.map((file) => file.path)).toEqual(['src/a.rs', 'src/b.rs']);
    expect(turns[0]).toMatchObject({ added: 3, removed: 1 });
  });

  it('keeps the last write\'s ranges and counts the writes', () => {
    const turns = turnChangesFrom([
      result(2, { path: 'src/a.rs', lines: lines(5, 0, [[1, 5]]) }),
      result(2, { path: 'src/a.rs', lines: lines(1, 1, [[7, 7]]) }),
    ]);
    const file = turns[0]?.files[0];
    expect(turns[0]?.files).toHaveLength(1);
    expect(file?.added).toBe(1);
    expect(file?.removed).toBe(1);
    expect(file?.ranges).toEqual([{ from: 7, to: 7 }]);
    expect(file?.writes).toBe(2);
  });

  it('skips events that belong to no turn', () => {
    expect(turnChangesFrom([result(undefined, { path: 'a', lines: lines(1, 0, [[1, 1]]) })])).toEqual([]);
    expect(turnChangesFrom([result(0, { path: 'a', lines: lines(1, 0, [[1, 1]]) })])).toEqual([]);
  });

  it('ignores a result that reported no lines — a shell command, for instance', () => {
    const turns = turnChangesFrom([result(3, { path: 'a', bytes: 12 })]);
    expect(turns).toEqual([]);
  });

  it('ignores a change with no path, and one that is not a tool result', () => {
    const turns = turnChangesFrom([
      result(4, { lines: lines(1, 0, [[1, 1]]) }),
      result(4, { path: 'a', lines: lines(1, 0, [[1, 1]]) }, 'tool/call'),
    ]);
    expect(turns).toEqual([]);
  });

  it('drops a malformed range instead of drawing it', () => {
    const turns = turnChangesFrom([
      result(5, {
        path: 'a',
        lines: {
          added: 2,
          removed: 0,
          ranges: [{ from: 0, to: 1 }, { from: 4, to: 2 }, { from: 6, to: 7 }, 'nope'],
        },
      }),
    ]);
    expect(turns[0]?.files[0]?.ranges).toEqual([{ from: 6, to: 7 }]);
  });

  it('keeps the coarse and cut flags, because they qualify every range', () => {
    const turns = turnChangesFrom([
      result(6, { path: 'a', lines: lines(40, 2, [[10, 40]], { coarse: true, cut: true }) }),
    ]);
    expect(turns[0]?.files[0]).toMatchObject({ coarse: true, cut: true });
  });

  it('returns turns oldest first', () => {
    const turns = turnChangesFrom([
      result(2, { path: 'b', lines: lines(1, 0, [[1, 1]]) }),
      result(1, { path: 'a', lines: lines(1, 0, [[1, 1]]) }),
    ]);
    expect(turns.map((turn) => turn.turn)).toEqual([1, 2]);
  });
});

describe('latestWrite', () => {
  it('names the newest write, with the sequence number that identifies it', () => {
    const first = result(1, { path: 'src/a.rs', lines: lines(1, 0, [[1, 1]]) });
    const second = result(2, { path: 'src/b.rs', lines: lines(1, 0, [[2, 2]]) });
    expect(latestWrite([first, second])).toEqual({ seq: second.seq, path: 'src/b.rs' });
  });

  it('skips results that did not report lines, so a read is not a write', () => {
    const read = result(1, { path: 'src/a.rs' });
    const write = result(1, { path: 'src/a.rs', lines: lines(1, 0, [[1, 1]]) });
    expect(latestWrite([read])).toBeNull();
    expect(latestWrite([write, read])).toEqual({ seq: write.seq, path: 'src/a.rs' });
  });

  it('has nothing to say about an empty log', () => {
    expect(latestWrite([])).toBeNull();
  });
});

describe('rangeLabel', () => {
  it('names a single line and a run', () => {
    expect(rangeLabel({ from: 3, to: 3 })).toBe('第 3 行');
    expect(rangeLabel({ from: 3, to: 7 })).toBe('第 3–7 行');
  });
});
