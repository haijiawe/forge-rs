// The goal chip: what this session is working toward, and how much budget is
// left, in the space a header has.
//
// It is a pure view on purpose. The route that answers `GET
// /api/sessions/{id}/goal` is newer than the frozen protocol module, so the
// fetch lives with the other fetches and this file owns only the two decisions
// a chip can get wrong:
//
//   * no goal is not an empty chip. `{"goal": null}` is a fact about the
//     session, and a blank chip in a header reads as a loading skeleton that
//     never finished.
//   * a round is promised only when the server promised it. `nextRound` is
//     absent when the budget is spent, so nothing here derives a round from
//     `rounds` to fill the gap.
import { currentRound, describeGoal, type Goal } from '../api/goal';

export interface GoalPillProps {
  /** The decoded goal; `null` when this session pursues none. */
  goal: Goal | null;
  /** Why the goal could not be read, when it could not. */
  error?: string | null;
}

export function GoalPill({ goal, error = null }: GoalPillProps) {
  if (error !== null) {
    // The error replaces the chip rather than sitting next to it: a cached or
    // stale goal rendered beside "could not be read" invites the reader to
    // trust a number this render could not confirm.
    return (
      <span className="goal-error" data-testid="goal-error">
        {`目标无法读取：${error}`}
      </span>
    );
  }
  if (goal === null) return null;
  const label = describeGoal(goal);
  return (
    <span
      className={`goal-pill goal-pill-${goal.status}`}
      data-testid="goal-pill"
      title={label}
      aria-label={label}
    >
      <span className="goal-pill-status">{goal.status}</span>
      <span className="goal-pill-round">
        {`第 ${String(currentRound(goal))} 轮，共 ${String(goal.maxRounds)} 轮`}
      </span>
      {/* The text stays a text node so the chip is still labelled when the
          stylesheet truncates it; colour alone is not a label. */}
      <span className="goal-pill-text">{goal.text}</span>
    </span>
  );
}