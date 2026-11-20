//! Dependency-graph validation for a team's tasks, run before a plan is
//! submitted.
//!
//! The board already has `blocked_by` on every task and a
//! [`Board::board_problems`](crate::Board::board_problems) report, but that
//! report is *after the fact*: it describes a board that was already written.
//! A bad graph is cheaper to refuse than to repair, and a plan is the last
//! moment where refusing costs nothing — no worker has started, no claim has
//! moved. This module is that refusal. It answers three defects, in a fixed
//! order, and every error names **both** ends of the bad edge so a human can act
//! without reading the graph themselves:
//!
//! * [`TeamTaskGraphError::Missing`] — a task is blocked by an id that is not on
//!   the board. It names the blocked (`dependent`) task and the absent
//!   (`dependency`) one.
//! * [`TeamTaskGraphError::Duplicate`] — a task lists the same blocker twice. It
//!   names the task and the repeated id. `Board::add_task` does not de-duplicate
//!   `blocked_by`, so this can genuinely reach a board.
//! * [`TeamTaskGraphError::Cycle`] — a dependency cycle, including a task that
//!   blocks itself. It names two *adjacent* tasks on the ring (`from` depends on
//!   `to`), which is the shortest fact a human needs to break it.
//!
//! The classification mirrors DeepSeek's `task-graph.ts` (`missing` /
//! `duplicate` / `cycle`), with one deliberate difference in emphasis: the
//! category is [`TeamTaskGraphViolation`] and the messages are worded so the two
//! task ids always read as an ordered pair.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use hearth_core::{Error, ErrorKind};

use crate::board::{Board, Task};
use crate::run::Plan;

/// The kind of graph defect, without the task ids.
///
/// Kept separate from [`TeamTaskGraphError`] so a caller can branch on the
/// category (`missing` / `duplicate` / `cycle`) without matching every payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeamTaskGraphViolation {
    Missing,
    Duplicate,
    Cycle,
}

impl TeamTaskGraphViolation {
    /// Stable, lowercase wire word for this violation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Duplicate => "duplicate",
            Self::Cycle => "cycle",
        }
    }
}

impl fmt::Display for TeamTaskGraphViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected task graph. Every variant names both tasks involved.
///
/// * `Missing`: `dependent` is blocked by `dependency`, which is absent.
/// * `Duplicate`: `dependent` lists `dependency` more than once.
/// * `Cycle`: `from` depends on `to`, and both sit on one dependency ring; for a
///   self-dependency both ids are the same task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamTaskGraphError {
    Missing {
        dependent: String,
        dependency: String,
    },
    Duplicate {
        dependent: String,
        dependency: String,
    },
    Cycle {
        from: String,
        to: String,
    },
}

impl TeamTaskGraphError {
    /// The category of this defect.
    pub fn violation(&self) -> TeamTaskGraphViolation {
        match self {
            Self::Missing { .. } => TeamTaskGraphViolation::Missing,
            Self::Duplicate { .. } => TeamTaskGraphViolation::Duplicate,
            Self::Cycle { .. } => TeamTaskGraphViolation::Cycle,
        }
    }

    /// The task that declares the bad edge:
    /// the blocked task for `missing`/`duplicate`, the `from` end for a cycle.
    pub fn dependent(&self) -> &str {
        match self {
            Self::Missing { dependent, .. } | Self::Duplicate { dependent, .. } => dependent,
            Self::Cycle { from, .. } => from,
        }
    }

    /// The other end of the bad edge:
    /// the absent or repeated blocker, or the `to` end of a cycle edge.
    pub fn dependency(&self) -> &str {
        match self {
            Self::Missing { dependency, .. } | Self::Duplicate { dependency, .. } => dependency,
            Self::Cycle { to, .. } => to,
        }
    }

    /// Both task ids, in `[dependent, dependency]` order. For a self-dependency
    /// the two entries are equal.
    pub fn tasks(&self) -> [&str; 2] {
        [self.dependent(), self.dependency()]
    }

    /// A human-readable message that always contains both task ids.
    pub fn message(&self) -> String {
        match self {
            Self::Missing {
                dependent,
                dependency,
            } => format!("team: task {dependent:?} is blocked by missing task {dependency:?}"),
            Self::Duplicate {
                dependent,
                dependency,
            } => format!("team: task {dependent:?} lists blocker {dependency:?} more than once"),
            Self::Cycle { from, to } => {
                format!("team: task dependency cycle: task {from:?} depends on {to:?}")
            }
        }
    }
}

impl fmt::Display for TeamTaskGraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for TeamTaskGraphError {}

impl From<TeamTaskGraphError> for Error {
    fn from(error: TeamTaskGraphError) -> Self {
        Error::new(ErrorKind::Invalid, error.message())
    }
}

/// Validate a slice of tasks. See the module docs for the pass order.
///
/// The slice need not be sorted; results are reported in task-id order and,
/// within a task, in `blocked_by` declaration order, so the same bad graph
/// always yields the same error.
pub fn validate_tasks(tasks: &[Task]) -> Result<(), TeamTaskGraphError> {
    let by_id: BTreeMap<&str, &Task> = tasks.iter().map(|task| (task.id.as_str(), task)).collect();

    // 1. Duplicate blockers first: a repeated edge is the cheapest defect to
    //    name, and checking it first keeps a duplicate from being reported as a
    //    missing dependency on its second occurrence.
    for task in by_id.values() {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for dependency in &task.blocked_by {
            if !seen.insert(dependency.as_str()) {
                return Err(TeamTaskGraphError::Duplicate {
                    dependent: task.id.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }

    // 2. Self-dependencies and blockers that are not on the board.
    for task in by_id.values() {
        for dependency in &task.blocked_by {
            if dependency == &task.id {
                return Err(TeamTaskGraphError::Cycle {
                    from: task.id.clone(),
                    to: task.id.clone(),
                });
            }
            if !by_id.contains_key(dependency.as_str()) {
                return Err(TeamTaskGraphError::Missing {
                    dependent: task.id.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }

    // 3. Longer cycles. Every dependency now exists, so the whole graph is
    //    walkable and a back edge is a genuine ring.
    if let Some((from, to)) = find_cycle(&by_id) {
        return Err(TeamTaskGraphError::Cycle { from, to });
    }
    Ok(())
}

/// Validate the tasks currently on `board`.
pub fn validate_board(board: &Board) -> Result<(), TeamTaskGraphError> {
    validate_tasks(&board.tasks())
}

/// Validate the tasks on the board a [`Plan`] is rooted at.
///
/// This opens the board the plan would run against, so it can be called before
/// the plan is submitted. Board I/O errors propagate; a graph defect becomes an
/// [`ErrorKind::Invalid`] error carrying the two task ids.
pub fn validate_plan(plan: &Plan) -> hearth_core::Result<()> {
    let board = Board::open(plan.root())?;
    validate_board(&board).map_err(Error::from)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    White,
    Grey,
    Black,
}

/// Iterative depth-first search for one cycle, returning two adjacent tasks
/// (`from`, `to`) with `from` depending on `to`.
///
/// Iterative rather than recursive so a long dependency chain cannot overflow
/// the stack. `by_id` must be closed under `blocked_by`; callers guarantee that
/// by checking `missing` first.
fn find_cycle(by_id: &BTreeMap<&str, &Task>) -> Option<(String, String)> {
    let mut color: BTreeMap<&str, Color> = by_id.keys().map(|&id| (id, Color::White)).collect();

    for &start in by_id.keys() {
        if color[start] != Color::White {
            continue;
        }
        color.insert(start, Color::Grey);
        // Each frame is (node, next dependency index to visit).
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        while let Some((node, index)) = stack.pop() {
            let dependencies = &by_id[node].blocked_by;
            if let Some(dependency) = dependencies.get(index) {
                let dependency = dependency.as_str();
                stack.push((node, index + 1));
                match color.get(dependency).copied() {
                    Some(Color::White) => {
                        color.insert(dependency, Color::Grey);
                        stack.push((dependency, 0));
                    }
                    Some(Color::Grey) => {
                        // A grey dependency is an ancestor of `node`: this edge
                        // closes the ring, and these two tasks are adjacent on
                        // it.
                        return Some((node.to_string(), dependency.to_string()));
                    }
                    _ => {}
                }
            } else {
                color.insert(node, Color::Black);
            }
        }
    }
    None
}
