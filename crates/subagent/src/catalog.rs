//! The parent's record of a direct child — its `subagent/catalog` row.
//!
//! Ported from DSH `packages/subagent/subagent/src/catalog.ts` without the
//! projection/remote machinery. A row is written on the **parent** log the moment
//! a child is created, and it is the stable handle everything else uses: the
//! sibling name is reserved against the set of names already here, `hearth
//! subagent list` prints these rows, and a slash path resolves by walking them.
//!
//! Because the parent log has exactly one writer (the agent loop, AGENTS.md rule
//! 1), a tool never appends this row itself. The subagent tool returns the row in
//! `Result.meta["subagentCatalog"]` and the loop writes it, the same channel
//! `compress` uses for `context/compressed`.

use std::collections::BTreeSet;

use hearth_core::event::{Event, EventType};
use serde_json::{json, Value};

use crate::descriptor::Mode;

/// The catalog row schema version this build writes.
pub const SUBAGENT_CATALOG_VERSION: u32 = 3;

/// The member of `Result.meta` that carries the row the loop must append.
pub const CATALOG_META: &str = "subagentCatalog";

/// One direct child, as the parent remembers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The child session id.
    pub child_id: String,
    pub mode: Mode,
    pub label: Option<String>,
    /// The sibling name, if one was assigned.
    pub name: Option<String>,
}

impl CatalogEntry {
    pub fn new(child_id: impl Into<String>, mode: Mode) -> CatalogEntry {
        CatalogEntry {
            child_id: child_id.into(),
            mode,
            label: None,
            name: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> CatalogEntry {
        self.name = Some(name.into());
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> CatalogEntry {
        self.label = Some(label.into());
        self
    }

    pub fn to_payload(&self) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("version".into(), json!(SUBAGENT_CATALOG_VERSION));
        payload.insert("childId".into(), json!(self.child_id));
        payload.insert("mode".into(), json!(self.mode.as_str()));
        if let Some(label) = &self.label {
            payload.insert("label".into(), json!(label));
        }
        if let Some(name) = &self.name {
            payload.insert("name".into(), json!(name));
        }
        Value::Object(payload)
    }

    /// The record to append on the parent's log.
    pub fn event(&self, parent_session: impl Into<String>) -> Event {
        Event::new(
            parent_session,
            EventType::SUBAGENT_CATALOG,
            Some(self.to_payload()),
        )
    }

    fn from_payload(value: &Value) -> Option<CatalogEntry> {
        let object = value.as_object()?;
        if object.get("version").and_then(Value::as_u64)? != SUBAGENT_CATALOG_VERSION as u64 {
            return None;
        }
        let child_id = object.get("childId").and_then(Value::as_str)?.to_string();
        let mode = Mode::parse(object.get("mode").and_then(Value::as_str)?)?;
        let string = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        };
        Some(CatalogEntry {
            child_id,
            mode,
            label: string("label"),
            name: string("name"),
        })
    }
}

/// Every child row on a log, oldest first. A row at an unknown version or with a
/// missing required member is skipped, not fatal: the catalog is data.
pub fn fold(events: &[Event]) -> Vec<CatalogEntry> {
    let mut out = Vec::new();
    for event in events {
        if event.typ != EventType::SUBAGENT_CATALOG.as_str() {
            continue;
        }
        if let Some(entry) = event.payload.as_ref().and_then(CatalogEntry::from_payload) {
            out.push(entry);
        }
    }
    out
}

/// The set of names already committed, for [`crate::names::reserve`].
pub fn committed_names(events: &[Event]) -> BTreeSet<String> {
    fold(events)
        .into_iter()
        .filter_map(|entry| entry.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_round_trips_through_its_payload() {
        let entry = CatalogEntry::new("s_child", Mode::Continuable)
            .with_name("agent-1")
            .with_label("research");
        let payload = entry.to_payload();
        assert_eq!(payload["version"], json!(3));
        assert_eq!(payload["childId"], json!("s_child"));
        assert_eq!(payload["mode"], json!("continuable"));
        assert_eq!(payload["name"], json!("agent-1"));
        assert_eq!(CatalogEntry::from_payload(&payload), Some(entry));
    }

    #[test]
    fn fold_keeps_only_catalog_rows_in_order() {
        let a = CatalogEntry::new("s_a", Mode::OneShot).with_name("agent-1");
        let b = CatalogEntry::new("s_b", Mode::Continuable).with_name("agent-2");
        let mut events = vec![
            Event::new("s_parent", EventType::SESSION_START, None),
            a.event("s_parent"),
            Event::new(
                "s_parent",
                EventType::HOOK_REWRITE,
                Some(json!({"phase": "pre-step"})),
            ),
            b.event("s_parent"),
        ];
        for (index, event) in events.iter_mut().enumerate() {
            event.seq = index as u64 + 1;
        }
        let rows = fold(&events);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].child_id, "s_a");
        assert_eq!(rows[1].child_id, "s_b");
    }

    #[test]
    fn committed_names_collects_only_named_rows() {
        let named = CatalogEntry::new("s_a", Mode::OneShot).with_name("agent-1");
        let unnamed = CatalogEntry::new("s_b", Mode::OneShot);
        let mut events = vec![named.event("p"), unnamed.event("p")];
        for (index, event) in events.iter_mut().enumerate() {
            event.seq = index as u64 + 1;
        }
        let names = committed_names(&events);
        assert!(names.contains("agent-1"));
        assert_eq!(names.len(), 1);
    }
}
