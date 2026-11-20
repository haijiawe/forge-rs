//! Durable identity (S1): a child records a descriptor on its own log, and the
//! parent log gets a catalog row with a sibling name the runner reserved.
//!
//! The runner never writes the parent log; it only reads it to reserve a name and
//! hands the row back through `ChildRun::catalog` (which the tool forwards in
//! `Result.meta`). These tests exercise the read-and-reserve half; the loop's
//! write is covered where the loop lives.

mod common;

use std::sync::Arc;

use common::*;
use hearth_core::event::{Event, EventType};
use hearth_subagent::{CallContext, CatalogEntry, Mode, Options, Runner};
use serde_json::json;

const FIXED_NOW_MS: i64 = 1_777_863_721_000;

fn runner_with_parent_log(dir: &TempDir, parent_log: Option<std::path::PathBuf>) -> Arc<Runner> {
    let provider = Arc::new(ScriptedProvider::new(vec![text_response("child ok")]));
    let mut opt = Options::new(
        provider,
        dir.path(),
        dir.path().join("workspace").to_string_lossy().into_owned(),
        "test-model",
    );
    opt.now = Arc::new(|| FIXED_NOW_MS);
    opt.clock = Arc::new(|| FIXED_NOW_MS);
    opt.parent_log = parent_log;
    Arc::new(Runner::new(opt).expect("runner"))
}

fn ctx() -> CallContext {
    CallContext {
        cancel: Default::default(),
        parent_session_id: "parent-1".into(),
        workspace: String::new(),
    }
}

#[test]
fn a_child_log_carries_a_descriptor_after_session_start() {
    let dir = TempDir::new("descriptor");
    let runner = runner_with_parent_log(&dir, None);
    let out = runner
        .run(&ctx(), "do the task", "", "")
        .expect("child run");

    let events = read_events_at(&dir.sessions().join(format!("{}.jsonl", out.id)));
    assert_dense(&events, &out.id);
    // Right after the head, before any turn: the descriptor is written first.
    assert_eq!(
        events[1].typ,
        EventType::SUBAGENT_DESCRIPTOR.as_str(),
        "second event is the descriptor: {:?}",
        events.iter().map(|e| e.typ.clone()).collect::<Vec<_>>()
    );
    let payload = payload(&events[1]);
    assert_eq!(payload["version"], json!(3));
    assert_eq!(payload["mode"], json!("one-shot"));

    // Without a parent log there is no name and no catalog row.
    assert!(out.name.is_none());
    assert!(out.catalog.is_none());
}

#[test]
fn a_parent_catalog_reserves_the_next_free_sibling_name() {
    let dir = TempDir::new("catalog");
    let parent = "parent-1";
    let log_path = dir.sessions().join(format!("{parent}.jsonl"));
    std::fs::create_dir_all(dir.sessions()).expect("sessions dir");

    // Seed the parent log with one child already named agent-1.
    {
        let store = open_store(&log_path);
        let head = Event::new(
            parent,
            EventType::SESSION_START,
            Some(json!({"workspace": "w", "model": "m"})),
        );
        store.append(&head).expect("append head");
        let row = CatalogEntry::new("old-child", Mode::OneShot)
            .with_name("agent-1")
            .event(parent);
        store.append(&row).expect("append catalog");
    }

    let runner = runner_with_parent_log(&dir, Some(log_path.clone()));
    let out = runner
        .run(&ctx(), "do the task", "", "")
        .expect("child run");

    assert_eq!(out.name.as_deref(), Some("agent-2"), "next free slot");
    let catalog = out.catalog.expect("catalog row");
    assert_eq!(catalog["childId"], json!(out.id));
    assert_eq!(catalog["name"], json!("agent-2"));
    assert_eq!(catalog["mode"], json!("one-shot"));
    assert_eq!(catalog["version"], json!(3));
}
