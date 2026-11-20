//! Interrupt authority: who is allowed to stop a child.
//!
//! These build real session logs, because the ancestor chain is not stored
//! anywhere but in each session's `session/start` `parent` member. Nothing here
//! writes a `.cancel` marker or runs a child: the module under test is pure
//! policy over the logs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use hearth_core::event::{Event, EventType};
use hearth_core::ErrorKind;
use hearth_session::Store;
use hearth_subagent::authority::{ancestry, check, describe_ancestry, Caller, Verdict};
use serde_json::{json, Value};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch data directory, removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "hearth-authority-{}-{}-{}-{}",
            label,
            std::process::id(),
            n,
            hearth_core::new_id().unwrap_or_else(|_| "noid".into())
        ));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn sessions(&self) -> PathBuf {
        self.path.join("sessions")
    }

    /// Write a session log whose `session/start` carries `parent`.
    fn session(&self, id: &str, parent: Option<&str>) {
        write_head(&self.sessions(), id, parent);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_head(sessions: &Path, id: &str, parent: Option<&str>) {
    std::fs::create_dir_all(sessions).expect("sessions dir");
    let store = Store::open(
        sessions.join(format!("{id}.jsonl")),
        hearth_session::Options {
            no_fsync: true,
            ..Default::default()
        },
    )
    .expect("open log");
    let mut payload = serde_json::Map::new();
    payload.insert("workspace".into(), json!("w"));
    payload.insert("model".into(), json!("m"));
    payload.insert("startedBy".into(), json!("subagent"));
    payload.insert("delegationDepth".into(), json!(1));
    if let Some(parent) = parent {
        payload.insert("parent".into(), json!(parent));
    }
    store
        .append(&Event::new(
            id,
            EventType::SESSION_START,
            Some(Value::Object(payload)),
        ))
        .expect("append session/start");
}

/// The tree used by most tests:
///
/// ```text
/// root-0001
/// ├── parent-0001
/// │   └── child-0001
/// │       └── grand-0001
/// └── sibling-0001
/// ```
fn tree(label: &str) -> Scratch {
    let dir = Scratch::new(label);
    dir.session("root-0001", None);
    dir.session("parent-0001", Some("root-0001"));
    dir.session("child-0001", Some("parent-0001"));
    dir.session("grand-0001", Some("child-0001"));
    dir.session("sibling-0001", Some("root-0001"));
    dir
}

fn deny_reason(verdict: Verdict) -> String {
    match verdict {
        Verdict::Deny(reason) => reason,
        Verdict::Allow => panic!("expected a denial, got Allow"),
    }
}

#[test]
fn the_operator_may_always_interrupt_even_with_no_log() {
    let dir = Scratch::new("operator");
    // No sessions directory at all, no log for the target: the operator is still
    // allowed. The identity is the authority, not the log.
    assert_eq!(
        check(dir.path(), &Caller::User, "never-written-0001"),
        Verdict::Allow
    );
}

#[test]
fn the_direct_parent_may_interrupt_its_child() {
    let dir = tree("parent");
    assert_eq!(
        check(
            dir.path(),
            &Caller::Session("parent-0001".into()),
            "child-0001"
        ),
        Verdict::Allow
    );
}

#[test]
fn a_grandparent_may_interrupt_any_descendant() {
    let dir = tree("grandparent");
    // Two links up, and three: authority flows down the whole chain.
    assert_eq!(
        check(
            dir.path(),
            &Caller::Session("root-0001".into()),
            "child-0001"
        ),
        Verdict::Allow
    );
    assert_eq!(
        check(
            dir.path(),
            &Caller::Session("root-0001".into()),
            "grand-0001"
        ),
        Verdict::Allow
    );
    assert_eq!(
        check(
            dir.path(),
            &Caller::Session("parent-0001".into()),
            "grand-0001"
        ),
        Verdict::Allow
    );
}

#[test]
fn a_session_may_not_interrupt_itself() {
    let dir = tree("self");
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("child-0001".into()),
        "child-0001",
    ));
    assert!(reason.contains("yourself"), "{reason}");
}

#[test]
fn a_child_may_not_interrupt_its_ancestor() {
    let dir = tree("descendant");
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("grand-0001".into()),
        "child-0001",
    ));
    assert!(reason.contains("ancestor"), "{reason}");
    // And the whole chain above it is equally off limits.
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("grand-0001".into()),
        "root-0001",
    ));
    assert!(reason.contains("ancestor"), "{reason}");
}

#[test]
fn an_unrelated_session_is_refused_and_both_sides_are_named() {
    let dir = tree("unrelated");
    // Same tree, different branch: a sibling is not an ancestor.
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("sibling-0001".into()),
        "child-0001",
    ));
    assert!(reason.contains("only an ancestor"), "{reason}");
    assert!(reason.contains("child-0001"), "names the target: {reason}");
    assert!(
        reason.contains("sibling-0001"),
        "names the caller: {reason}"
    );

    // A different tree entirely is refused the same way: both chains resolve,
    // and neither contains the other.
    let other = Scratch::new("unrelated-other");
    other.session("other-root-0001", None);
    other.session("stranger-0001", Some("other-root-0001"));
    other.session("child-0001", Some("other-root-0001"));
    let reason = deny_reason(check(
        other.path(),
        &Caller::Session("stranger-0001".into()),
        "child-0001",
    ));
    assert!(reason.contains("only an ancestor"), "{reason}");
    assert!(reason.contains("stranger-0001"), "{reason}");
}

#[test]
fn a_broken_chain_is_refused_with_the_missing_link() {
    let dir = Scratch::new("broken");
    // The target's parent was never written: the chain stops before it can
    // prove anything, so the answer must be a denial.
    dir.session("orphan-0001", Some("gone-0001"));
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("root-0001".into()),
        "orphan-0001",
    ));
    assert!(reason.contains("incomplete"), "{reason}");
    assert!(
        reason.contains("gone-0001"),
        "names the missing link: {reason}"
    );

    let error = ancestry(dir.path(), "orphan-0001").expect_err("must refuse");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert!(error.to_string().contains("gone-0001"), "{error}");
}

#[test]
fn a_missing_target_log_is_refused_for_a_session() {
    let dir = Scratch::new("no-target");
    dir.session("root-0001", None);
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("root-0001".into()),
        "never-written-0001",
    ));
    assert!(reason.contains("incomplete"), "{reason}");
    assert!(reason.contains("never-written-0001"), "{reason}");
}

#[test]
fn a_cycle_is_refused_instead_of_followed() {
    let dir = Scratch::new("cycle");
    dir.session("a-0001", Some("b-0001"));
    dir.session("b-0001", Some("a-0001"));
    // Two-node cycle.
    let error = ancestry(dir.path(), "a-0001").expect_err("must refuse a cycle");
    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(error.to_string().contains("loops"), "{error}");
    // Self-parent is the one-node version.
    dir.session("s-0001", Some("s-0001"));
    let error = ancestry(dir.path(), "s-0001").expect_err("must refuse self-parent");
    assert_eq!(error.kind(), ErrorKind::Refused);
    // And `check` denies rather than hanging: reaching the assertion at all is
    // the proof that the walk terminated.
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("c-0001".into()),
        "a-0001",
    ));
    assert!(reason.contains("a-0001"), "{reason}");
    assert!(reason.contains("incomplete"), "{reason}");
}

#[test]
fn a_hostile_target_id_is_refused_before_any_path_is_built() {
    let dir = tree("hostile-target");
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("child-0001".into()),
        "../../etc/passwd",
    ));
    assert!(!reason.is_empty(), "{reason}");
    let error = ancestry(dir.path(), "../../etc/passwd").expect_err("must refuse");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    // Nothing escaped the scratch tree.
    assert!(!dir
        .path()
        .parent()
        .unwrap_or(dir.path())
        .join("etc")
        .exists());
}

#[test]
fn a_hostile_parent_recorded_in_a_log_is_refused() {
    let dir = Scratch::new("hostile-parent");
    dir.session("evil-0001", Some("../../etc/passwd"));
    let error = ancestry(dir.path(), "evil-0001").expect_err("must refuse a hostile parent");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("root-0001".into()),
        "evil-0001",
    ));
    assert!(reason.contains("incomplete"), "{reason}");
}

#[test]
fn ancestry_reads_the_chain_from_the_parent_links() {
    let dir = tree("chain");
    assert_eq!(
        ancestry(dir.path(), "grand-0001").expect("chain"),
        vec![
            "child-0001".to_string(),
            "parent-0001".to_string(),
            "root-0001".to_string(),
        ],
        "direct parent first, root last"
    );
    let chain = ancestry(dir.path(), "grand-0001").expect("chain");
    assert_eq!(describe_ancestry(&chain), "lead/parent-0001/child-0001");
}

#[test]
fn the_root_has_an_empty_chain() {
    let dir = tree("root");
    assert!(ancestry(dir.path(), "root-0001").expect("chain").is_empty());
    assert_eq!(describe_ancestry(&[]), "lead");
}

#[test]
fn a_log_without_session_start_is_refused() {
    let dir = Scratch::new("headless");
    let sessions = dir.sessions();
    std::fs::create_dir_all(&sessions).expect("sessions dir");
    let store = Store::open(
        sessions.join("headless-0001.jsonl"),
        hearth_session::Options {
            no_fsync: true,
            ..Default::default()
        },
    )
    .expect("open log");
    store
        .append(&Event::new(
            "headless-0001",
            EventType::TURN_END,
            Some(json!({"turns": 1})),
        ))
        .expect("append turn/end");
    let error = ancestry(dir.path(), "headless-0001").expect_err("no head");
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert!(error.to_string().contains("session/start"), "{error}");
    let reason = deny_reason(check(
        dir.path(),
        &Caller::Session("root-0001".into()),
        "headless-0001",
    ));
    assert!(reason.contains("incomplete"), "{reason}");
}
