import { describe, expect, it } from 'vitest';
import { GoalPill, type GoalPillProps } from './GoalPill';
import type { Goal, GoalStatus } from '../api/goal';
import { mount } from '../test/render';

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

function render(props: GoalPillProps) {
  return mount(<GoalPill {...props} />);
}

function pill(container: HTMLElement): Element | null {
  return container.querySelector('[data-testid="goal-pill"]');
}

describe('GoalPill', () => {
  it('renders nothing for a session that pursues no goal', () => {
    const { container, unmount } = render({ goal: null });
    // An empty chip in a header reads as a skeleton that never finished.
    expect(container.innerHTML).toBe('');
    expect(pill(container)).toBeNull();
    unmount();
  });

  it('shows the turn number as rounds + 1, and the goal text', () => {
    const { container, unmount } = render({ goal: goal() });
    const chip = pill(container);
    expect(chip).not.toBeNull();
    expect(chip?.textContent).toContain('第 1 轮，共 4 轮');
    // Round 0 of 4 would be the off-by-one this component exists to avoid.
    expect(chip?.textContent).not.toContain('第 0 轮');
    expect(chip?.textContent).toContain('keep the smoke test honest');
    expect(chip?.className).toBe('goal-pill goal-pill-active');
    unmount();
  });

  it('carries the whole line in title and aria-label', () => {
    const { container, unmount } = render({ goal: goal({ nextRound: 2 }) });
    const chip = pill(container);
    const line = 'keep the smoke test honest — 第 1 轮，共 4 轮，active，下一轮 2';
    expect(chip?.getAttribute('title')).toBe(line);
    expect(chip?.getAttribute('aria-label')).toBe(line);
    unmount();
  });

  it('promises no round when the budget is spent but the status is active', () => {
    // Three continuations against a budget of four turns: the loop will stop,
    // and the log has no `goal/ended` line yet, so the status is still active.
    const { container, unmount } = render({ goal: goal({ rounds: 3, nextRound: undefined }) });
    const chip = pill(container);
    expect(chip?.textContent).toContain('第 4 轮，共 4 轮');
    expect(chip?.textContent).toContain('active');
    // Nothing may invent round 5 here: the server did not promise it.
    expect(chip?.textContent).not.toContain('下一轮');
    expect(chip?.getAttribute('title')).toBe('keep the smoke test honest — 第 4 轮，共 4 轮，active');
    unmount();
  });

  it('renders every status word as a label, not only as a colour', () => {
    const statuses: GoalStatus[] = ['complete', 'blocked', 'cancelled', 'exhausted', 'unknown'];
    for (const status of statuses) {
      const { container, unmount } = render({ goal: goal({ status, nextRound: undefined }) });
      const chip = pill(container);
      expect(chip?.className).toBe(`goal-pill goal-pill-${status}`);
      expect(chip?.textContent).toContain(status);
      unmount();
    }
  });

  it('reports a goal it could not read instead of showing a stale chip', () => {
    const { container, unmount } = render({ goal: null, error: 'connection refused' });
    const failure = container.querySelector('[data-testid="goal-error"]');
    expect(failure?.textContent).toBe('目标无法读取：connection refused');
    expect(pill(container)).toBeNull();
    unmount();
  });

  it('does not show the chip beside the failure line', () => {
    const { container, unmount } = render({ goal: goal(), error: 'timeout' });
    expect(container.querySelector('[data-testid="goal-error"]')).not.toBeNull();
    // The error replaces the chip: a number this render could not confirm must
    // not sit next to "could not be read".
    expect(pill(container)).toBeNull();
    unmount();
  });
});