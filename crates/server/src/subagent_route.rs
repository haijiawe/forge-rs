//! The subagent tree of one session, with the timing the logs already record.
//!
//! Layer L3. Everything here is a read: `hearth_subagent::list::descendants`
//! walks the parent catalog, `hearth_subagent::timing::of_child` folds each
//! child's own log, and this module joins the two into one wire value. No
//! process is started, no log is created, and nothing is appended -- the same
//! discipline as `present_route` and `teams`.
//!
//! Why a route at all: `hearth subagent tree` and `hearth subagent timing` can
//! answer both halves separately, but a person watching a run wants them
//! together -- "which children are still working, and how long did the ones
//! that finished take". The browser cannot shell out, so without this route the
//! answer exists and is invisible.
//!
//! Four rules for what belongs here:
//!
//! 1. **Two projections, joined, not recomputed.** The tree comes from the
//!    parent's `subagent/catalog` rows and the timing from each child's own
//!    events. Recomputing either here would give the same numbers two owners
//!    and eventually two answers.
//! 2. **A child this build cannot read is a row, not a hole.** `descendants`
//!    already returns corrupt, unsupported and unavailable children as nodes
//!    carrying a diagnostic; they are passed through with `diagnostic` set and
//!    timing omitted. Dropping them would make a broken child look like a child
//!    that was never started.
//! 3. **No timing is not an error.** A child whose log has no turns yet, or
//!    whose log cannot be read, simply has `timing: null`. The tree still
//!    exists; inventing zeroes would claim it settled instantly.
//! 4. **A missing root log is a 404, not an empty tree.** "This session does
//!    not exist" and "this session has no children" are different answers, and
//!    only the caller knows which they asked.

use std::path::Path;

use serde::Serialize;

use hearth_subagent::list::{self, Diagnostic, Node};
use hearth_subagent::timing;

/// What went wrong reading the tree. The HTTP layer maps these to status codes;
/// keeping them apart is what stops a disk error from being reported as "no
/// such session".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The root session has no log on disk.
    NotFound,
    /// The log exists but its bytes did not come back.
    Read(String),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TimingRow {
    pub settled_ms: i64,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn_completed: Option<i64>,
    pub turns: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRow {
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub path: String,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<TimingRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<DiagnosticRow>,
    pub children: Vec<Row>,
}

/// The whole answer. `count` is the number of rows in the tree, not the number
/// of direct children, because "how many subagents did this session start" is
/// the question a reader is actually asking.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub session: String,
    pub count: usize,
    pub subagents: Vec<Row>,
}

/// Read the subagent tree of `session`, joined with each child's timing.
pub fn read(data_dir: &Path, session: &str) -> Result<Report, Problem> {
    let log = data_dir.join("sessions").join(format!("{session}.jsonl"));
    if !log.is_file() {
        return Err(Problem::NotFound);
    }
    let nodes = list::descendants(data_dir, session).map_err(|error| match error.kind() {
        hearth_core::ErrorKind::NotFound => Problem::NotFound,
        // The root log is there but unreadable. Blaming the caller for the disk
        // is the one thing this must not do.
        _ => Problem::Read(error.message().to_string()),
    })?;
    let mut count = 0;
    let rows = rows_of(data_dir, &nodes, &mut count);
    Ok(Report {
        session: session.to_string(),
        count,
        subagents: rows,
    })
}

fn rows_of(data_dir: &Path, nodes: &[Node], count: &mut usize) -> Vec<Row> {
    nodes
        .iter()
        .map(|node| {
            *count += 1;
            Row {
                session_id: node.entry.child_id.clone(),
                name: node.entry.name.clone(),
                mode: node.entry.mode.as_str().to_string(),
                label: node.entry.label.clone(),
                path: node.path.clone(),
                depth: node.depth,
                timing: timing_of(data_dir, &node.entry.child_id),
                diagnostic: node.diagnostic.as_ref().map(diagnostic_row),
                children: rows_of(data_dir, &node.children, count),
            }
        })
        .collect()
}

/// A child whose timing cannot be folded has no timing, not zero timing. The
/// diagnostic on the row already says why the log could not be read, so this
/// does not repeat it.
fn timing_of(data_dir: &Path, child_id: &str) -> Option<TimingRow> {
    timing::of_child(data_dir, child_id)
        .ok()
        .map(|found| TimingRow {
            settled_ms: found.settled_ms,
            active: found.active,
            last_turn_completed: found.last_turn_completed,
            turns: found.turns,
        })
}

fn diagnostic_row(diagnostic: &Diagnostic) -> DiagnosticRow {
    DiagnosticRow {
        kind: diagnostic.as_str().to_string(),
        detail: diagnostic.detail().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_root_log_is_not_an_empty_tree() {
        let dir =
            std::env::temp_dir().join(format!("hearth-subagent-route-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read(&dir, "nobody"), Err(Problem::NotFound));
    }

    #[test]
    fn an_empty_answer_still_serialises_count_and_session() {
        let report = Report {
            session: "root".to_string(),
            count: 0,
            subagents: Vec::new(),
        };
        let value = serde_json::to_value(&report).expect("report serialises");
        assert_eq!(value["session"], serde_json::json!("root"));
        assert_eq!(value["count"], serde_json::json!(0));
        assert_eq!(value["subagents"], serde_json::json!([]));
    }

    #[test]
    fn a_row_omits_what_it_does_not_know() {
        let row = Row {
            session_id: "child".to_string(),
            name: None,
            mode: "one-shot".to_string(),
            label: None,
            path: "lead/agent-1".to_string(),
            depth: 1,
            timing: None,
            diagnostic: None,
            children: Vec::new(),
        };
        let value = serde_json::to_value(&row).expect("row serialises");
        assert!(value.get("name").is_none());
        assert!(value.get("timing").is_none());
        assert!(value.get("diagnostic").is_none());
        assert_eq!(value["mode"], serde_json::json!("one-shot"));
    }

    #[test]
    fn a_diagnostic_names_its_kind_and_says_what_it_found() {
        let row = diagnostic_row(&Diagnostic::Corrupt("line 2: invalid JSON".to_string()));
        assert_eq!(row.kind, "corrupt");
        assert!(row.detail.contains("line 2"));
    }
}
