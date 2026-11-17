//! A scheduled task: the rule, the prompt to deliver, and the bookkeeping that
//! keeps a due occurrence from being delivered twice.
//!
//! The rule is kept as the operator typed it, not as its parsed form, so a
//! later version that understands more parts can re-read the same file without
//! a migration. Everything a delivery decision needs is a field: the library
//! never asks a clock, a session or a model anything.

use serde::{Deserialize, Serialize};

use crate::rule::Rule;

/// One schedule entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// 32 hex characters. A task without an id is refused by the registry, so
    /// the id is the identity in the file.
    pub id: String,
    /// The rrule text exactly as given, so a re-parse in a later version sees
    /// the same input.
    pub rule: String,
    /// What to deliver when the occurrence comes due.
    pub prompt: String,
    /// Session to deliver into, or `None` for a new session per occurrence.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub created_ms: i64,
    /// DTSTART. The recurrence is anchored here, and this instant itself is
    /// never delivered: it is the start line, not the first occurrence.
    pub start_ms: i64,
    /// The instant of the last delivery, which is also where the next search
    /// begins. `None` means nothing has been delivered yet.
    #[serde(default)]
    pub last_run_ms: Option<i64>,
    #[serde(default)]
    pub runs: u32,
}

impl Task {
    pub fn new(rule: &str, prompt: &str, start_ms: i64, now_ms: i64) -> Task {
        Task {
            // The OS random source failing is a real failure, but this
            // constructor cannot return one without making every caller handle
            // it. An empty id is unambiguous and `Registry::add` refuses it, so
            // the failure is still loud at the only place that matters.
            id: forge_core::new_id().unwrap_or_default(),
            rule: rule.trim().to_string(),
            prompt: prompt.trim().to_string(),
            target: None,
            created_ms: now_ms,
            start_ms,
            last_run_ms: None,
            runs: 0,
        }
    }

    /// The next occurrence, or `None` when the rule cannot be read or has no
    /// more occurrences.
    ///
    /// The search starts after the last delivery, falling back to DTSTART. That
    /// is what makes a second `due` call in the same minute return nothing
    /// instead of the same occurrence again.
    pub fn next_after_ms(&self) -> Option<i64> {
        let rule = Rule::parse(&self.rule).ok()?;
        let after_ms = self.last_run_ms.unwrap_or(self.start_ms);
        rule.next_after(self.start_ms, after_ms)
    }

    /// The occurrence that is due at `now_ms`, if there is one.
    pub fn due_at(&self, now_ms: i64) -> Option<i64> {
        self.next_after_ms().filter(|at_ms| *at_ms <= now_ms)
    }
}

/// A task and the occurrence of it that is owed now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub task: Task,
    pub at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(rule: &str, start_ms: i64) -> Task {
        Task::new(rule, " say hello ", start_ms, 1_794_873_600_000)
    }

    #[test]
    fn a_new_task_trims_its_text_and_has_not_run() {
        let task = task("FREQ=DAILY;BYHOUR=9", 1_794_873_600_000);
        assert_eq!(task.rule, "FREQ=DAILY;BYHOUR=9");
        assert_eq!(task.prompt, "say hello");
        assert_eq!(task.runs, 0);
        assert_eq!(task.last_run_ms, None);
        assert_eq!(task.created_ms, 1_794_873_600_000);
        assert_eq!(task.id.len(), 32, "32 hex characters: {:?}", task.id);
        assert!(task.id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn the_anchor_is_not_the_first_delivery() {
        let start = 1_794_873_600_000;
        let task = task("FREQ=DAILY;BYHOUR=9", start);
        assert_eq!(task.next_after_ms(), Some(1_794_906_000_000));
        assert_eq!(
            task.due_at(1_794_873_600_000),
            None,
            "the anchor itself is not due"
        );
        assert_eq!(task.due_at(1_794_906_000_000), Some(1_794_906_000_000));
    }

    #[test]
    fn a_delivered_occurrence_is_not_due_again() {
        let start = 1_794_873_600_000;
        let mut task = task("FREQ=DAILY;BYHOUR=9", start);
        task.last_run_ms = Some(1_794_906_000_000);
        assert_eq!(task.due_at(1_794_906_000_000), None);
        assert_eq!(task.next_after_ms(), Some(1_794_992_400_000));
    }

    #[test]
    fn an_unreadable_rule_is_not_a_delivery() {
        let task = task("FREQ=DAILY;BYMONTH=3", 1_794_873_600_000);
        assert_eq!(task.next_after_ms(), None);
        assert_eq!(task.due_at(i64::MAX), None);
    }
}
