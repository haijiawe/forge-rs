//! A read-only projection of a board: what a reader wants to know without
//! replaying the files themselves.
//!
//! The board already answers "what is on it" (`Board::tasks`), and the journal
//! already answers "what did each lead do, in order" (`Journal::entries`). This
//! module answers the question neither does alone: *what does this run look like
//! right now*, in one sorted shape, so a CLI, an API response and a test can all
//! describe the same board the same way.
//!
//! Two rules, both because a projection that guesses is worse than one that
//! refuses:
//!
//! - Ordering is by task id, not by insertion. Two readers folding the same
//!   board must produce the same bytes, and insertion order is not something the
//!   file promises.
//! - A task blocked by an id that is not on the board keeps its blocker in the
//!   view and is reported as `missing`, rather than being quietly dropped. A
//!   dependency that vanished is a fact about the board, not noise.

use std::path::Path;

use forge_core::Result;

use crate::board::{Board, Status, Task};
use crate::journal::Journal;

/// One task, flattened for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    pub id: String,
    pub subject: String,
    pub status: Status,
    pub owner: Option<String>,
    pub revision: u32,
    /// Blockers that are still not completed.
    pub waiting_on: Vec<String>,
    /// Blockers that name a task which is not on the board at all.
    pub missing: Vec<String>,
}

impl TaskView {
    /// Whether every blocker this task names is completed and present.
    pub fn is_ready(&self) -> bool {
        self.waiting_on.is_empty() && self.missing.is_empty() && self.status == Status::Pending
    }
}

/// Counts per status, for a one-line summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub pending: usize,
    pub in_progress: usize,
    pub completed: usize,
}

impl Counts {
    pub fn total(self) -> usize {
        self.pending + self.in_progress + self.completed
    }
}

/// The whole projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub board: String,
    pub revision: u64,
    pub tasks: Vec<TaskView>,
    pub counts: Counts,
    /// Journal entries per lead, in the order the leads first appear.
    pub leads: Vec<(String, usize)>,
    /// Problems the board itself reported (scope conflicts, cycles, and so on).
    pub problems: Vec<String>,
}

impl View {
    /// Tasks that could be claimed right now.
    pub fn ready(&self) -> Vec<&TaskView> {
        self.tasks.iter().filter(|t| t.is_ready()).collect()
    }

    /// A stable, human-readable rendering. Sorted by task id, one line per task,
    /// because two runs over the same board must print the same text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "board {}\nrevision {}\n{} task(s): {} pending, {} in progress, {} completed\n",
            self.board,
            self.revision,
            self.counts.total(),
            self.counts.pending,
            self.counts.in_progress,
            self.counts.completed
        ));
        if self.tasks.is_empty() {
            out.push_str("no tasks\n");
        }
        for task in &self.tasks {
            out.push_str(&format!(
                "  {:<24} {:<12} {}",
                task.id,
                task.status.as_str(),
                task.subject
            ));
            if let Some(owner) = &task.owner {
                out.push_str(&format!(" [{owner}]"));
            }
            if !task.waiting_on.is_empty() {
                out.push_str(&format!(" waiting on {}", task.waiting_on.join(", ")));
            }
            if !task.missing.is_empty() {
                out.push_str(&format!(" missing {}", task.missing.join(", ")));
            }
            out.push('\n');
        }
        if !self.leads.is_empty() {
            let parts: Vec<String> = self
                .leads
                .iter()
                .map(|(lead, count)| format!("{lead} ({count})"))
                .collect();
            out.push_str(&format!("leads: {}\n", parts.join(", ")));
        }
        for problem in &self.problems {
            out.push_str(&format!("problem: {problem}\n"));
        }
        out
    }
}

/// Project the board at `root`.
///
/// The journal is optional on purpose: a board written by a run that never
/// journalled anything is still a board, and refusing to describe it would hide
/// the tasks that exist.
pub fn project(root: impl AsRef<Path>) -> Result<View> {
    let root = root.as_ref();
    let board = Board::open(root)?;
    let tasks = board.tasks();

    let known: std::collections::BTreeMap<String, Status> =
        tasks.iter().map(|t| (t.id.clone(), t.status)).collect();

    let mut views: Vec<TaskView> = tasks.iter().map(|task| view_of(task, &known)).collect();
    // Sorted by id so the same board prints the same bytes every time.
    views.sort_by(|a, b| a.id.cmp(&b.id));

    let mut counts = Counts::default();
    for task in &views {
        match task.status {
            Status::Pending => counts.pending += 1,
            Status::InProgress => counts.in_progress += 1,
            Status::Completed => counts.completed += 1,
        }
    }

    let problems: Vec<String> = board
        .board_problems()
        .iter()
        .map(|p| format!("{} {}: {}", p.kind.as_str(), p.tasks.join(", "), p.message))
        .collect();

    let mut leads: Vec<(String, usize)> = Vec::new();
    if let Ok(journal) = Journal::open(root) {
        let mut seen: Vec<String> = Vec::new();
        for lead in journal.leads().unwrap_or_default() {
            let count = journal.entries(&lead).map(|e| e.len()).unwrap_or(0);
            if !seen.contains(&lead) {
                seen.push(lead.clone());
                leads.push((lead, count));
            }
        }
    }

    Ok(View {
        board: root.to_string_lossy().into_owned(),
        revision: board.revision(),
        tasks: views,
        counts,
        leads,
        problems,
    })
}

fn view_of(task: &Task, known: &std::collections::BTreeMap<String, Status>) -> TaskView {
    let mut waiting_on = Vec::new();
    let mut missing = Vec::new();
    for blocker in &task.blocked_by {
        match known.get(blocker) {
            Some(Status::Completed) => {}
            Some(_) => waiting_on.push(blocker.clone()),
            None => missing.push(blocker.clone()),
        }
    }
    TaskView {
        id: task.id.clone(),
        subject: task.subject.clone(),
        status: task.status,
        owner: task.owner.clone(),
        revision: task.revision,
        waiting_on,
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::NewTask;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("forge-team-view")
            .join(format!("{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn a_task_whose_blocker_is_completed_is_ready() {
        let dir = scratch("ready");
        let board = Board::open(&dir).expect("board");
        let first = board.add_task(NewTask::new("first")).expect("add");
        let second = board
            .add_task(NewTask::new("second").blocked_by([first.id.as_str()]))
            .expect("add");
        board.complete(&first.id, first.revision).expect("complete");

        let view = project(&dir).expect("project");
        let found = view.tasks.iter().find(|t| t.id == second.id).expect("task");
        assert!(found.is_ready(), "a completed blocker unblocks: {found:?}");
        assert_eq!(view.counts.completed, 1);
        assert_eq!(view.counts.pending, 1);
    }

    #[test]
    fn a_task_blocked_by_a_missing_id_says_so_instead_of_dropping_it() {
        let dir = scratch("missing");
        let board = Board::open(&dir).expect("board");
        let task = board
            .add_task(NewTask::new("orphan").blocked_by(["nope"]))
            .expect("add");

        let view = project(&dir).expect("project");
        let found = view.tasks.iter().find(|t| t.id == task.id).expect("task");
        assert_eq!(found.missing, vec!["nope".to_string()]);
        assert!(found.waiting_on.is_empty());
        assert!(!found.is_ready(), "a missing blocker is not ready");
        assert!(view.render().contains("missing nope"));
    }

    #[test]
    fn the_projection_is_sorted_by_id_not_by_insertion() {
        let dir = scratch("order");
        let board = Board::open(&dir).expect("board");
        for id in ["c", "a", "b"] {
            board
                .add_task(NewTask::new(format!("task {id}")).id(id))
                .expect("add");
        }
        let view = project(&dir).expect("project");
        let ids: Vec<&str> = view.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn an_empty_board_renders_a_usable_line() {
        let dir = scratch("empty");
        let view = project(&dir).expect("project");
        let text = view.render();
        assert!(text.contains("0 task(s)"), "{text}");
        assert!(text.contains("no tasks"), "{text}");
    }
}
