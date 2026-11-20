// The decoder, tested against the shapes `crates/server/src/teams.rs` can
// actually produce — including the ones it produces when something is wrong.
import { describe, expect, it } from 'vitest';
import {
  parseBoard,
  parseBoards,
  taskStatusCounts,
  workerState,
  workerStateCounts,
  type TeamTask,
  type TeamWorker,
} from './teams';

const healthy = {
  boards: [
    {
      board: 'demo-20261120-000000',
      team: 'demo',
      parallel: null,
      createdAt: '2026-11-20T00:00:00Z',
      tasks: [
        { id: 't1', subject: 'do a thing', status: 'completed', owner: 'alpha' },
        { id: 't2', subject: 'do another', status: 'pending', owner: null },
      ],
      workers: [
        { name: 'alpha', ok: true, note: '1 turn(s)', events: 2 },
        { name: 'beta', ok: null, note: '', events: 1 },
      ],
    },
  ],
};

function worker(ok: unknown): TeamWorker {
  return { name: 'w', ok: ok === true ? true : ok === false ? false : null, note: '', events: 1 };
}

function task(status: string): TeamTask {
  return { id: 't', subject: 's', status, owner: null };
}

describe('parseBoards', () => {
  it('decodes a healthy board without inventing the fields the server left null', () => {
    const report = parseBoards(healthy);
    expect(report.boards).toHaveLength(1);
    const board = report.boards[0];
    expect(board?.board).toBe('demo-20261120-000000');
    expect(board?.team).toBe('demo');
    // The API always sends null here; a decoder that turned that into 0 (or a
    // default) would be inventing a parallelism nobody recorded.
    expect(board?.parallel).toBeNull();
    expect(board?.createdAt).toBe('2026-11-20T00:00:00Z');
    expect(board?.tasks).toHaveLength(2);
    expect(board?.tasks[1]?.owner).toBeNull();
    expect(board?.workers[1]?.ok).toBeNull();
    // Absent on a healthy board: not `undefined`, an empty list.
    expect(board?.diagnostics).toEqual([]);
  });

  it('degrades every malformed answer to an empty board list instead of throwing', () => {
    for (const value of [null, undefined, 42, 'nope', [], {}, { boards: 'nope' }, { boards: 3 }]) {
      const report = parseBoards(value);
      expect(Array.isArray(report.boards)).toBe(true);
      expect(report.boards).toEqual([]);
    }
  });

  it('skips a row with no board name rather than rendering an unaddressable board', () => {
    const report = parseBoards({
      boards: [
        { team: 'nameless', createdAt: '2026-11-20T00:00:00Z' },
        { board: 'kept-20261120-000000', team: 'kept' },
      ],
    });
    expect(report.boards.map((board) => board.board)).toEqual(['kept-20261120-000000']);
  });

  it('turns a board with no tasks, workers or diagnostics keys into empty lists', () => {
    const report = parseBoards({ boards: [{ board: 'bare-20261120-000000', team: 'bare' }] });
    const board = report.boards[0];
    expect(board?.tasks).toEqual([]);
    expect(board?.workers).toEqual([]);
    expect(board?.diagnostics).toEqual([]);
    expect(board?.createdAt).toBeNull();
    expect(board?.parallel).toBeNull();
  });

  it('keeps human diagnostics and drops non-strings and blanks', () => {
    const report = parseBoards({
      boards: [
        {
          board: 'diag-20261120-000000',
          team: 'diag',
          diagnostics: ['board.json is missing', 7, null, '', 'workers/ is missing'],
        },
      ],
    });
    expect(report.boards[0]?.diagnostics).toEqual([
      'board.json is missing',
      'workers/ is missing',
    ]);
  });

  it('reads a non-numeric parallelism or event count as unknown, not as zero', () => {
    const report = parseBoards({
      boards: [
        {
          board: 'odd-20261120-000000',
          team: 'odd',
          parallel: '2',
          tasks: [{ id: 't1', subject: 's', status: 'pending', owner: 7 }],
          workers: [{ name: 'a', ok: 'true', note: 'n', events: 'two' }],
        },
      ],
    });
    const board = report.boards[0];
    expect(board?.parallel).toBeNull();
    expect(board?.workers[0]?.events).toBeNull();
    // A non-boolean `ok` is not a verdict: only a literal true or false is.
    expect(board?.workers[0]?.ok).toBeNull();
    // A non-string owner is not a name; the task is unassigned, as for null.
    expect(board?.tasks[0]?.owner).toBeNull();
  });
});

describe('parseBoard', () => {
  it('decodes the unwrapped single-board answer', () => {
    const board = parseBoard(healthy.boards[0]);
    expect(board?.board).toBe('demo-20261120-000000');
    expect(board?.workers).toHaveLength(2);
  });

  it('refuses the envelope, which is the list route and not this one', () => {
    expect(parseBoard(healthy)).toBeNull();
    expect(parseBoard(null)).toBeNull();
    expect(parseBoard({ team: 'no-name' })).toBeNull();
  });
});

describe('workerState', () => {
  it('names the three states: finished, failed, and cannot say', () => {
    expect(workerState(worker(true))).toBe('finished');
    expect(workerState(worker(false))).toBe('failed');
    expect(workerState(worker(null))).toBe('unknown');
    expect(workerState({ name: 'w', ok: null, note: '', events: null })).toBe('unknown');
  });

  it('counts the three states separately so an unfinished worker is not a failure', () => {
    const counts = workerStateCounts([worker(true), worker(false), worker(null), worker(null)]);
    expect(counts).toEqual({ finished: 1, failed: 1, unknown: 2 });
    expect(workerStateCounts([])).toEqual({ finished: 0, failed: 0, unknown: 0 });
  });
});

describe('taskStatusCounts', () => {
  it('counts the three board statuses in board order', () => {
    expect(taskStatusCounts([task('completed'), task('pending'), task('pending')])).toEqual([
      { status: 'pending', count: 2 },
      { status: 'completed', count: 1 },
    ]);
  });

  it('keeps an unknown status visible after the known ones', () => {
    expect(taskStatusCounts([task('weird'), task('completed'), task('')])).toEqual([
      { status: 'completed', count: 1 },
      { status: '', count: 1 },
      { status: 'weird', count: 1 },
    ]);
  });

  it('counts nothing for a board with no tasks', () => {
    expect(taskStatusCounts([])).toEqual([]);
  });
});