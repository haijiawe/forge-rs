//! Reading a parent's children back out of its log.
//!
//! A read, never a start: `forge subagent list` must not spawn a child, resume a
//! child, or open a child log — it answers from the parent's own rows. That is
//! the whole reason the catalog is on the parent log rather than only in memory.

use std::path::Path;

use forge_core::event::Event;
use forge_core::Result as CoreResult;
use forge_session::Store;

use crate::catalog::{self, CatalogEntry};

/// Every event on `path`, or an empty list if the file does not exist yet (a
/// session that never ran a turn has no catalog and that is not an error).
pub fn read_events(path: &Path) -> CoreResult<Vec<Event>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let store = Store::open(path, forge_session::Options::default())?;
    let mut events = Vec::new();
    store.read_from(0, |event| {
        events.push(event.clone());
        Ok(())
    })?;
    Ok(events)
}

/// The direct children recorded on a parent log at `path`.
pub fn children_in_log(path: &Path) -> CoreResult<Vec<CatalogEntry>> {
    Ok(catalog::fold(&read_events(path)?))
}

/// The direct children recorded in an open store.
pub fn children(store: &Store) -> CoreResult<Vec<CatalogEntry>> {
    let mut events = Vec::new();
    store.read_from(0, |event| {
        events.push(event.clone());
        Ok(())
    })?;
    Ok(catalog::fold(&events))
}

/// A one-line-per-child rendering for `forge subagent list`.
pub fn render(entries: &[CatalogEntry]) -> String {
    if entries.is_empty() {
        return "no subagents in this session\n".to_string();
    }
    let mut text = String::from("name\tchild\tsession\tmode\tlabel\n");
    for entry in entries {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            entry.name.as_deref().unwrap_or("-"),
            entry.child_id,
            entry.child_id,
            entry.mode.as_str(),
            entry.label.as_deref().unwrap_or("-"),
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::Mode;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-subagent-list-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir.join("parent.jsonl")
    }

    #[test]
    fn a_missing_log_reads_as_no_children() {
        let path = scratch("missing").with_file_name("never.jsonl");
        assert!(children_in_log(&path).expect("read").is_empty());
    }

    #[test]
    fn rows_written_to_a_log_read_back() {
        let path = scratch("rows");
        let store = Store::open(&path, forge_session::Options::default()).expect("open");
        store
            .append(
                &CatalogEntry::new("s_a", Mode::Continuable)
                    .with_name("agent-1")
                    .event("s_p"),
            )
            .expect("append a");
        store
            .append(
                &CatalogEntry::new("s_b", Mode::OneShot)
                    .with_name("agent-2")
                    .event("s_p"),
            )
            .expect("append b");
        let rows = children_in_log(&path).expect("read");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name.as_deref(), Some("agent-1"));
        assert_eq!(rows[1].name.as_deref(), Some("agent-2"));
        let text = render(&rows);
        assert!(text.contains("agent-1"), "{text}");
        assert!(text.contains("continuable"), "{text}");
    }

    #[test]
    fn an_empty_session_renders_a_plain_sentence() {
        assert_eq!(
            render(&[]),
            "no subagents in this session\n",
            "an empty list is an answer, not a blank table"
        );
    }
}
