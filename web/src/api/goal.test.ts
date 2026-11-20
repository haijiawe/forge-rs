import { describe, expect, it } from 'vitest';
import { currentRound, describeGoal, parseGoal, type Goal } from './goal';

/** The shape `GET /api/sessions/{id}/goal` sends for an active goal. */
function body(overrides: Record<string, unknown> = {}): unknown {
  return {
    goal: {
      id: '4f8277c6e0c47db7d990594f4bd165a4',
      text: 'keep the smoke test honest',
      maxRounds: 4,
      rounds: 0,
      status: 'active',
      nextRound: 1,
      ...overrides,
    },
  };
}

function goal(overrides: Partial<Goal> = {}): Goal {
  return {
    id: 'g1',
    text: 'keep the smoke test honest',
    maxRounds: 4,
    rounds: 0,
    status: 'active',
    nextRound: 1,
    ...overrides,
  };
}

describe('parseGoal', () => {
  it('reads the reply the route sends', () => {
    expect(parseGoal(body())).toEqual({
      id: '4f8277c6e0c47db7d990594f4bd165a4',
      text: 'keep the smoke test honest',
      maxRounds: 4,
      rounds: 0,
      status: 'active',
      nextRound: 1,
    });
  });

  it('reads a session with no goal as null, not as an error', () => {
    expect(parseGoal({ goal: null })).toBeNull();
  });

  it('never throws on something that is not a reply', () => {
    for (const value of ['text', [], [1, 2], 42, 0, true, null, undefined]) {
      expect(parseGoal(value)).toBeNull();
    }
  });

  it('drops a goal without an id', () => {
    expect(parseGoal({ goal: { text: 'no id', maxRounds: 4, rounds: 0, status: 'active' } })).toBeNull();
    expect(parseGoal({ goal: { id: '', text: 'blank id' } })).toBeNull();
  });

  it('keeps an unknown status visible as unknown', () => {
    const decoded = parseGoal(body({ status: 'rebooting' }));
    expect(decoded?.status).toBe('unknown');
    // An absent status is the same answer, never a crash.
    expect(parseGoal(body({ status: undefined }))?.status).toBe('unknown');
  });

  it('survives a non-numeric budget and a non-numeric round count', () => {
    const decoded = parseGoal(body({ rounds: '2', maxRounds: null }));
    expect(decoded?.rounds).toBe(0);
    expect(decoded?.maxRounds).toBe(0);
    // A number the arithmetic could not survive is not a count either.
    expect(parseGoal(body({ rounds: Number.NaN }))?.rounds).toBe(0);
  });

  it('keeps nextRound only when it is a number', () => {
    expect(parseGoal(body({ nextRound: 2 }))?.nextRound).toBe(2);
    expect(parseGoal(body({ nextRound: '2' }))?.nextRound).toBeUndefined();
    // Absent, not present-and-undefined: `'nextRound' in goal` is the check a
    // reader uses to know whether another turn is coming.
    expect(parseGoal(body({ nextRound: undefined }))).not.toHaveProperty('nextRound');
    expect(parseGoal(body({ nextRound: 3 }))).toHaveProperty('nextRound');
  });

  it('defaults a missing text to an empty string', () => {
    expect(parseGoal(body({ text: 7 }))?.text).toBe('');
  });
});

describe('currentRound', () => {
  it('counts the turn goal/created already spent', () => {
    // `rounds` counts continuations, so zero continuations is round 1.
    expect(currentRound(goal({ rounds: 0 }))).toBe(1);
    expect(currentRound(goal({ rounds: 2 }))).toBe(3);
  });
});

describe('describeGoal', () => {
  it('names the turn, the budget and the status on one line', () => {
    // `nextRound` is the server's own continuation number and is printed as
    // given: deriving one here would be a second opinion on the budget.
    expect(describeGoal(goal({ nextRound: 2 }))).toBe(
      'keep the smoke test honest — round 1 of 4, active, next round 2',
    );
  });

  it('says nothing about a next round when the server promised none', () => {
    // `maxRounds: 4` with three continuations: the budget is spent, the status
    // is still active, and no round will run.
    const spent = goal({ rounds: 3 });
    const line = describeGoal({ ...spent, nextRound: undefined });
    expect(line).toBe('keep the smoke test honest — round 4 of 4, active');
    expect(line).not.toContain('next round');
  });

  it('still reads when the goal has no text', () => {
    expect(describeGoal(goal({ text: '', nextRound: undefined }))).toBe('round 1 of 4, active');
  });
});