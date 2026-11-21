// `GET /api/sessions/{id}/goal`: the goal a session is working toward.
//
// The goal is not server state; it is three `goal/*` lines in the session log,
// folded by the backend (`crates/server/src/goal_route.rs`) into this wire
// value. Two facts about that fold are easy to get wrong on this side of the
// wire, so each is spelled out once here and never recomputed elsewhere:
//
//   * `rounds` counts continuations, not turns. `goal/created` already spent
//     turn 1, so the turn on screen is `rounds + 1`.
//   * `nextRound` is present only when the loop's own budget check says another
//     turn will run. An `active` goal whose budget is spent has no `nextRound`
//     yet, because the loop has not written its `goal/ended` line. A browser
//     that promised `rounds + 1` anyway would show a round nobody will run.
//
// Nothing here throws. `{"goal": null}` means "no goal", which is a fact and
// not a failure; a parser that threw would surface in the UI as "the route is
// broken" and send the reader looking for a bug that does not exist. The same
// reason makes an unknown `status` become `unknown` instead of an error: a
// newer server may know a word this build does not, and the pill still has to
// render.

/** What the loop is doing, as the wire spells it; `unknown` is ours, not the server's. */
export type GoalStatus =
  | 'active'
  | 'complete'
  | 'blocked'
  | 'cancelled'
  | 'exhausted'
  | 'unknown';

/** One goal, decoded. */
export interface Goal {
  id: string;
  text: string;
  /** The budget, in turns (`1` is legal: `goal/created` alone spends it). */
  maxRounds: number;
  /** Continuations opened so far — not turns. See `currentRound`. */
  rounds: number;
  status: GoalStatus;
  /** Present only when another turn will actually run. */
  nextRound?: number;
}

const GOAL_STATUSES = [
  'active',
  'complete',
  'blocked',
  'cancelled',
  'exhausted',
] as const satisfies readonly GoalStatus[];

function statusOf(value: unknown): GoalStatus {
  return GOAL_STATUSES.find((known) => known === value) ?? 'unknown';
}

/**
 * A finite number, or `0`.
 *
 * Zero is the honest stand-in for a budget this build cannot read: it makes the
 * pill say "round 1 of 0", which reads as broken data rather than as a promise
 * of rounds that do not exist.
 */
function count(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

/** The goal object inside a reply body, or `null` when there is not one. */
function unwrap(value: unknown): Record<string, unknown> | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
  const body = value as Record<string, unknown>;
  // The route answers `{"goal": …}`, but a caller already holding the goal must
  // not get a silent blank pill just because it passed the inner half. The
  // absence of the `goal` key is what tells the two shapes apart.
  if (!('goal' in body)) return body;
  const inner = body.goal;
  if (typeof inner !== 'object' || inner === null || Array.isArray(inner)) return null;
  return inner as Record<string, unknown>;
}

/**
 * Decode one reply body without trusting its shape.
 *
 * `value` is the whole body (`{"goal": null}` or `{"goal": {…}}`); a bare goal
 * object is accepted as well. Returns `null` for "no goal here" and for
 * anything unreadable — never a thrown error.
 */
export function parseGoal(value: unknown): Goal | null {
  const raw = unwrap(value);
  if (raw === null) return null;
  const id = raw.id;
  // Without an id there is nothing to key the goal by, and two goals on one
  // session cannot be told apart.
  if (typeof id !== 'string' || id === '') return null;
  const goal: Goal = {
    id,
    text: typeof raw.text === 'string' ? raw.text : '',
    maxRounds: count(raw.maxRounds),
    rounds: count(raw.rounds),
    status: statusOf(raw.status),
  };
  // Kept only as an absent member, never as `undefined`: `'nextRound' in goal`
  // is how a reader checks the promise, and a present-but-undefined key would
  // make that check lie.
  if (typeof raw.nextRound === 'number' && Number.isFinite(raw.nextRound)) {
    goal.nextRound = raw.nextRound;
  }
  return goal;
}

/**
 * The turn number to show.
 *
 * `rounds` counts continuations: `goal/created` already spent turn 1, so a goal
 * that has continued zero times is on round 1, not round 0. Changing this to
 * `goal.rounds` would relabel every goal one turn early.
 */
export function currentRound(goal: Goal): number {
  return goal.rounds + 1;
}

/** One line for a tooltip or aria-label. */
export function describeGoal(goal: Goal): string {
  const round = `第 ${String(currentRound(goal))} 轮，共 ${String(goal.maxRounds)} 轮`;
  const head = goal.text === '' ? `${round}，${goal.status}` : `${goal.text} — ${round}，${goal.status}`;
  // Silence about the next round when the server sent none: that is the case
  // where the budget is spent and the loop will stop, whatever `status` says.
  if (goal.nextRound === undefined) return head;
  return `${head}，下一轮 ${String(goal.nextRound)}`;
}