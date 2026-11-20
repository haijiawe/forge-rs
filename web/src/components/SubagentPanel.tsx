// The children a session delegated to, with how long each one took.
//
// Everything shown here comes from the log: the rows are the parent's
// `subagent/catalog` entries and the timings are folds of each child's own
// turns. The panel adds no state of its own beyond the last read, because the
// answer is cheap (two file reads) and stale numbers are worse than a short
// wait.
//
// Three silences on purpose:
// - no children, no panel: a session that never delegated should not carry an
//   empty box saying so;
// - a failed read prints one quiet line pointing at the CLI, because the
//   sessions still works and the operator still has `forge subagent tree`;
// - a child whose log is unreadable keeps its row with the diagnostic, since
//   hiding it would make it look like a child that never existed.
import { describeState, flatten, type SubagentRow, type SubagentTree } from '../api/subagents';

export interface SubagentPanelProps {
  tree: SubagentTree | null;
  error?: string | null;
}

function title(row: SubagentRow): string {
  const parts = [row.path, row.mode];
  if (row.label !== undefined) parts.push(row.label);
  return parts.join(' · ');
}

export function SubagentPanel({ tree, error }: SubagentPanelProps) {
  if (error !== undefined && error !== null) {
    return (
      <section className="subagents" aria-label="subagents">
        <p className="subagents-empty">
          the subagent tree could not be read: <code>{error}</code> — try <code>forge subagent tree</code>
        </p>
      </section>
    );
  }
  if (tree === null) return null;
  const rows = flatten(tree.subagents);
  if (rows.length === 0) return null;
  return (
    <section className="subagents" aria-label="subagents">
      <h3 className="subagents-title">
        {String(tree.count)} subagent{tree.count === 1 ? '' : 's'} below this session
      </h3>
      <ul className="subagent-list">
        {rows.map((row) => (
          <li
            key={row.sessionId}
            className="subagent-row"
            style={{ marginLeft: `${String((row.depth - 1) * 12)}px` }}
            title={title(row)}
          >
            <span className="mono subagent-name">{row.name ?? '(unnamed)'}</span>
            <span className="mono subagent-mode">{row.mode}</span>
            <span
              className={
                row.diagnostic !== undefined
                  ? 'subagent-state subagent-state-bad'
                  : row.timing?.active === true
                    ? 'subagent-state subagent-state-live'
                    : 'subagent-state'
              }
            >
              {describeState(row)}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}