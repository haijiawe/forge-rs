//! The subagent tree and name resolution, exercised on hand-built session logs.
//!
//! These logs are written here on purpose. The interesting cases are the ones a
//! healthy run never produces — a child whose log is missing, corrupt, or from a
//! build that wrote another descriptor version — because the walk promises that
//! one bad child costs one node, not the whole tree.

mod common;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use common::TempDir;
use forge_core::event::{Event, EventType};
use forge_core::ErrorKind;
use forge_session::{Options as StoreOptions, Store};
use forge_subagent::catalog::CatalogEntry;
use forge_subagent::child::child_log_path;
use forge_subagent::descriptor::{Descriptor, Mode};
use forge_subagent::list::{self, Diagnostic};
use serde_json::json;

/// A data dir with the `sessions/` directory the store writes into.
fn data_dir(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    std::fs::create_dir_all(dir.sessions()).expect("sessions dir");
    dir
}

fn write_log(path: &Path, events: Vec<Event>) {
    let store = Store::open(
        path,
        StoreOptions {
            no_fsync: true,
            ..Default::default()
        },
    )
    .expect("open store");
    for event in events {
        store.append(&event).expect("append event");
    }
    store.close().expect("close store");
}

fn head(session: &str, parent: Option<&str>, depth: u32) -> Event {
    let mut payload = json!({
        "workspace": "/ws",
        "model": "local",
        "startedBy": "user",
        "delegationDepth": depth,
    });
    if let Some(parent) = parent {
        payload["parent"] = json!(parent);
    }
    Event::new(session, EventType::SESSION_START, Some(payload))
}

/// A child log: the head, its descriptor, then any catalog rows it owns.
fn child_log(dir: &Path, id: &str, descriptor: Event, rows: Vec<CatalogEntry>) {
    let mut events = vec![head(id, Some("s_root"), 1), descriptor];
    for row in rows {
        events.push(row.event(id));
    }
    write_log(&child_log_path(dir, id), events);
}

fn healthy(dir: &Path, id: &str, rows: Vec<CatalogEntry>) {
    child_log(dir, id, Descriptor::one_shot("local").event(id), rows);
}

fn flatten(nodes: &[list::Node], out: &mut Vec<(String, usize)>) {
    for node in nodes {
        out.push((node.path.clone(), node.depth));
        flatten(&node.children, out);
    }
}

fn flat(nodes: &[list::Node]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    flatten(nodes, &mut out);
    out
}

/// lead → agent-1 → agent-2, and lead → agent-3, with the root rows written out
/// of order so the sort is what puts them right.
fn three_levels(dir: &Path) -> Vec<list::Node> {
    write_log(
        &child_log_path(dir, "s_root"),
        vec![
            head("s_root", None, 0),
            CatalogEntry::new("s_a3", Mode::OneShot)
                .with_name("agent-3")
                .event("s_root"),
            CatalogEntry::new("s_a1", Mode::Continuable)
                .with_name("agent-1")
                .event("s_root"),
        ],
    );
    child_log(
        dir,
        "s_a1",
        Descriptor::continuable("local", "a1").event("s_a1"),
        vec![CatalogEntry::new("s_a2", Mode::OneShot).with_name("agent-2")],
    );
    healthy(dir, "s_a2", Vec::new());
    healthy(dir, "s_a3", Vec::new());
    list::descendants(dir, "s_root").expect("walk")
}

#[test]
fn a_three_level_tree_walks_in_pre_order_with_depth() {
    let dir = data_dir("tree-order");
    let nodes = three_levels(dir.path());

    assert_eq!(
        flat(&nodes),
        vec![
            ("lead/agent-1".to_string(), 1),
            ("lead/agent-1/agent-2".to_string(), 2),
            ("lead/agent-3".to_string(), 1),
        ],
        "pre-order, siblings sorted by name"
    );
    assert_eq!(nodes.len(), 2, "two direct children of the root");
    assert_eq!(nodes[0].children.len(), 1);
    assert_eq!(nodes[0].entry.child_id, "s_a1");
    assert_eq!(nodes[0].children[0].entry.child_id, "s_a2");
    for node in &nodes {
        assert!(node.diagnostic.is_none(), "{:?}", node.diagnostic);
    }

    assert_eq!(
        list::render_tree(&nodes),
        "  agent-1  continuable  s_a1\n    agent-2  one-shot  s_a2\n  \
         agent-3  one-shot  s_a3\n",
        "two spaces per depth, then mode and session id"
    );
}

#[test]
fn a_child_without_a_log_is_unavailable_and_its_sibling_survives() {
    let dir = data_dir("tree-unavailable");
    write_log(
        &child_log_path(dir.path(), "s_root"),
        vec![
            head("s_root", None, 0),
            CatalogEntry::new("s_gone", Mode::OneShot)
                .with_name("agent-2")
                .event("s_root"),
            CatalogEntry::new("s_here", Mode::Continuable)
                .with_name("agent-1")
                .event("s_root"),
        ],
    );
    healthy(dir.path(), "s_here", Vec::new());

    let nodes = list::descendants(dir.path(), "s_root").expect("walk");
    assert_eq!(nodes.len(), 2, "the missing child is a node, not a hole");
    assert_eq!(nodes[0].path, "lead/agent-1");
    assert!(nodes[0].diagnostic.is_none());

    let gone = &nodes[1];
    assert_eq!(gone.path, "lead/agent-2");
    assert_eq!(gone.entry.child_id, "s_gone");
    assert!(gone.children.is_empty());
    match &gone.diagnostic {
        Some(Diagnostic::Unavailable(detail)) => {
            assert!(detail.contains("s_gone.jsonl"), "{detail}");
        }
        other => panic!("expected unavailable, got {other:?}"),
    }
    assert!(
        list::render_tree(&nodes).contains("[unavailable: "),
        "{}",
        list::render_tree(&nodes)
    );
}

#[test]
fn a_descriptor_at_an_unknown_version_is_unsupported() {
    let dir = data_dir("tree-unsupported");
    write_log(
        &child_log_path(dir.path(), "s_root"),
        vec![
            head("s_root", None, 0),
            CatalogEntry::new("s_future", Mode::OneShot)
                .with_name("agent-1")
                .event("s_root"),
            CatalogEntry::new("s_ok", Mode::OneShot)
                .with_name("agent-2")
                .event("s_root"),
        ],
    );
    let mut payload = Descriptor::one_shot("local").to_payload();
    payload["version"] = json!(99);
    child_log(
        dir.path(),
        "s_future",
        Event::new("s_future", EventType::SUBAGENT_DESCRIPTOR, Some(payload)),
        Vec::new(),
    );
    healthy(dir.path(), "s_ok", Vec::new());

    let nodes = list::descendants(dir.path(), "s_root").expect("walk");
    assert_eq!(nodes.len(), 2);
    match &nodes[0].diagnostic {
        Some(Diagnostic::Unsupported(detail)) => {
            assert!(detail.contains("version 99"), "{detail}");
        }
        other => panic!("expected unsupported, got {other:?}"),
    }
    assert!(nodes[0].children.is_empty());
    assert!(nodes[1].diagnostic.is_none(), "the sibling is still read");
}

#[test]
fn a_corrupt_line_is_reported_without_rewriting_the_log() {
    let dir = data_dir("tree-corrupt");
    write_log(
        &child_log_path(dir.path(), "s_root"),
        vec![
            head("s_root", None, 0),
            CatalogEntry::new("s_bad", Mode::OneShot)
                .with_name("agent-1")
                .event("s_root"),
            CatalogEntry::new("s_ok", Mode::OneShot)
                .with_name("agent-2")
                .event("s_root"),
        ],
    );
    let bad_path = child_log_path(dir.path(), "s_bad");
    write_log(
        &bad_path,
        vec![
            head("s_bad", Some("s_root"), 1),
            Descriptor::one_shot("local").event("s_bad"),
        ],
    );
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&bad_path)
            .expect("open for append");
        file.write_all(b"{\"v\":1,\"seq\":oops\n").expect("write");
    }
    healthy(dir.path(), "s_ok", Vec::new());

    let before = std::fs::read(&bad_path).expect("read bytes");
    let nodes = list::descendants(dir.path(), "s_root").expect("walk");
    let after = std::fs::read(&bad_path).expect("read bytes");

    assert_eq!(
        before, after,
        "a read must not truncate or rewrite a session log"
    );
    assert_eq!(nodes.len(), 2);
    match &nodes[0].diagnostic {
        Some(Diagnostic::Corrupt(detail)) => {
            assert!(detail.contains("line 3"), "{detail}");
        }
        other => panic!("expected corrupt, got {other:?}"),
    }
    assert!(nodes[0].children.is_empty());
    assert!(nodes[1].diagnostic.is_none(), "the sibling is still read");
}

#[test]
fn a_torn_final_record_is_not_called_corrupt() {
    // A writer that dies mid-record leaves a line with no terminating newline.
    // That is an unfinished write, not bad data: the store's own recovery drops
    // it, and a read-only walk must reach the same verdict without truncating.
    let dir = data_dir("tree-torn");
    write_log(
        &child_log_path(dir.path(), "s_root"),
        vec![
            head("s_root", None, 0),
            CatalogEntry::new("s_torn", Mode::OneShot)
                .with_name("agent-1")
                .event("s_root"),
        ],
    );
    let torn_path = child_log_path(dir.path(), "s_torn");
    healthy(dir.path(), "s_torn", Vec::new());
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&torn_path)
            .expect("open for append");
        file.write_all(b"{\"v\":1,\"seq\":3,\"id\":\"e3\",\"typ\":\"turn/start\"")
            .expect("write");
    }

    let before = std::fs::read(&torn_path).expect("read bytes");
    let nodes = list::descendants(dir.path(), "s_root").expect("walk");
    let after = std::fs::read(&torn_path).expect("read bytes");

    assert_eq!(
        before, after,
        "the torn tail is dropped in memory, not on disk"
    );
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].path, "lead/agent-1");
    assert!(
        nodes[0].diagnostic.is_none(),
        "a torn tail is an unfinished write: {:?}",
        nodes[0].diagnostic
    );
    let resolved = list::resolve_name(dir.path(), "s_root", "agent-1").expect("resolve");
    assert_eq!(resolved, "s_torn");
}

#[test]
fn a_name_path_and_the_same_path_under_lead_resolve_alike() {
    let dir = data_dir("tree-resolve");
    three_levels(dir.path());

    assert_eq!(
        list::resolve_name(dir.path(), "s_root", "lead/agent-1/agent-2").expect("absolute"),
        "s_a2"
    );
    assert_eq!(
        list::resolve_name(dir.path(), "s_root", "agent-1/agent-2").expect("relative"),
        "s_a2"
    );
    assert_eq!(
        list::resolve_name(dir.path(), "s_root", "lead").expect("root"),
        "s_root"
    );
    assert_eq!(
        list::resolve_name(dir.path(), "s_root", "agent-1").expect("direct"),
        "s_a1"
    );
    assert_eq!(
        list::resolve_name(dir.path(), "s_root", " lead/agent-3 ").expect("trimmed"),
        "s_a3"
    );
}

#[test]
fn an_unknown_name_is_not_found_and_names_the_candidates() {
    let dir = data_dir("tree-not-found");
    three_levels(dir.path());

    let error = list::resolve_name(dir.path(), "s_root", "lead/agent-1/agent-3")
        .expect_err("no such sibling");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    let message = error.message().to_string();
    assert!(message.contains("no child named \"agent-3\""), "{message}");
    assert!(
        message.contains("under lead/agent-1"),
        "the walked path is part of the message: {message}"
    );
    assert!(
        message.contains("(have: agent-2)"),
        "the candidates are part of the message: {message}"
    );

    let bare = list::resolve_name(dir.path(), "s_root", "agent-9").expect_err("no such child");
    assert_eq!(bare.kind(), ErrorKind::NotFound);
    assert!(
        bare.message().contains("(have: agent-1, agent-3)"),
        "{}",
        bare.message()
    );
}

#[test]
fn an_illegal_segment_is_invalid_not_missing() {
    let dir = data_dir("tree-invalid");
    three_levels(dir.path());

    for bad in ["Lead/agent-1", "agent-1/", "agent 1", ""] {
        let error = list::resolve_name(dir.path(), "s_root", bad)
            .expect_err(&format!("{bad:?} is not a name"));
        assert_eq!(
            error.kind(),
            ErrorKind::Invalid,
            "{bad:?}: {}",
            error.message()
        );
    }
}

#[test]
fn an_empty_tree_renders_the_same_sentence_as_the_flat_list() {
    assert_eq!(
        list::render_tree(&[]),
        "no subagents in this session\n",
        "the tree phrasing must match list::render"
    );
    let dir = data_dir("tree-empty");
    write_log(
        &child_log_path(dir.path(), "s_root"),
        vec![head("s_root", None, 0)],
    );
    let nodes = list::descendants(dir.path(), "s_root").expect("walk");
    assert!(nodes.is_empty());
    assert_eq!(list::render_tree(&nodes), "no subagents in this session\n");

    let missing = list::descendants(dir.path(), "s_never_ran");
    assert!(
        missing
            .expect("a missing root is empty, not an error")
            .is_empty(),
        "a session that never ran a turn has no catalog"
    );
}
