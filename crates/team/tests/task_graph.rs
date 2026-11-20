//! The dependency graph's contract: a bad plan is refused before it runs, and
//! every refusal names both tasks so a human can break the edge it reports.
//!
//! The "names both tasks" assertions are the point. A validator that only
//! returned "cycle" would be technically correct and useless: the caller would
//! still have to find the ring. These tests fail if the two ids are not carried
//! in the error.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use forge_core::ErrorKind;
use forge_team::{
    validate_board, validate_plan, Board, NewTask, Plan, TeamTaskGraphError, TeamTaskGraphViolation,
};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "forge-team-graph-{}-{}-{}-{}",
        std::process::id(),
        stamp,
        NEXT.fetch_add(1, Ordering::SeqCst),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn board(label: &str) -> Board {
    Board::open(temp_dir(label)).expect("open board")
}

#[test]
fn a_diamond_graph_passes() {
    let board = board("diamond");
    board.add_task(NewTask::new("root").id("a")).expect("add a");
    board
        .add_task(NewTask::new("left").id("b").blocked_by(["a"]))
        .expect("add b");
    board
        .add_task(NewTask::new("right").id("c").blocked_by(["a"]))
        .expect("add c");
    board
        .add_task(NewTask::new("join").id("d").blocked_by(["b", "c"]))
        .expect("add d");
    assert!(validate_board(&board).is_ok());
}

#[test]
fn a_missing_blocker_is_refused_and_names_both_tasks() {
    let board = board("missing");
    board
        .add_task(NewTask::new("blocked").id("b").blocked_by(["ghost"]))
        .expect("add b");

    let error = validate_board(&board).expect_err("missing blocker must be refused");
    assert_eq!(error.violation(), TeamTaskGraphViolation::Missing);
    match &error {
        TeamTaskGraphError::Missing {
            dependent,
            dependency,
        } => {
            assert_eq!(dependent, "b");
            assert_eq!(dependency, "ghost");
        }
        other => panic!("expected Missing, got {other:?}"),
    }
    assert_eq!(error.tasks(), ["b", "ghost"]);
    let message = error.message();
    assert!(
        message.contains("\"b\""),
        "message must name the dependent: {message}"
    );
    assert!(
        message.contains("\"ghost\""),
        "message must name the missing task: {message}"
    );
}

#[test]
fn a_duplicate_blocker_is_refused_and_names_the_repeated_task() {
    let board = board("duplicate");
    board.add_task(NewTask::new("base").id("a")).expect("add a");
    // `Board::add_task` does not de-duplicate `blocked_by`, so this reaches disk
    // exactly as written.
    board
        .add_task(NewTask::new("waits twice").id("b").blocked_by(["a", "a"]))
        .expect("add b");

    let error = validate_board(&board).expect_err("duplicate blocker must be refused");
    assert_eq!(error.violation(), TeamTaskGraphViolation::Duplicate);
    match &error {
        TeamTaskGraphError::Duplicate {
            dependent,
            dependency,
        } => {
            assert_eq!(dependent, "b");
            assert_eq!(dependency, "a");
        }
        other => panic!("expected Duplicate, got {other:?}"),
    }
    assert_eq!(error.tasks(), ["b", "a"]);
}

#[test]
fn a_two_task_cycle_is_refused_and_names_adjacent_tasks() {
    let board = board("cycle2");
    board
        .add_task(NewTask::new("a").id("a").blocked_by(["b"]))
        .expect("add a");
    board
        .add_task(NewTask::new("b").id("b").blocked_by(["a"]))
        .expect("add b");

    let error = validate_board(&board).expect_err("cycle must be refused");
    assert_eq!(error.violation(), TeamTaskGraphViolation::Cycle);
    let [from, to] = error.tasks();
    assert_ne!(from, to, "a two-task cycle names two distinct tasks");
    assert!(
        matches!((from, to), ("a", "b") | ("b", "a")),
        "pair on the ring: {from}->{to}"
    );
    // The reported pair is a real edge: `from` is blocked by `to`.
    let dependent = board.task(from).expect("from exists");
    assert!(
        dependent.blocked_by.iter().any(|id| id == to),
        "{from} must actually depend on {to}"
    );
    let message = error.message();
    assert!(
        message.contains(from) && message.contains(to),
        "message names both: {message}"
    );
}

#[test]
fn a_self_dependency_is_refused_as_a_cycle_naming_the_task_twice() {
    let board = board("self");
    board
        .add_task(NewTask::new("solo").id("solo").blocked_by(["solo"]))
        .expect("add solo");

    let error = validate_board(&board).expect_err("self dependency must be refused");
    assert_eq!(error.violation(), TeamTaskGraphViolation::Cycle);
    assert_eq!(error.tasks(), ["solo", "solo"]);
}

#[test]
fn a_three_task_ring_names_two_adjacent_tasks_on_the_ring() {
    let board = board("cycle3");
    board
        .add_task(NewTask::new("a").id("a").blocked_by(["b"]))
        .expect("add a");
    board
        .add_task(NewTask::new("b").id("b").blocked_by(["c"]))
        .expect("add b");
    board
        .add_task(NewTask::new("c").id("c").blocked_by(["a"]))
        .expect("add c");

    let error = validate_board(&board).expect_err("ring must be refused");
    let [from, to] = error.tasks();
    let ring = ["a", "b", "c"];
    assert!(
        ring.contains(&from) && ring.contains(&to),
        "both on the ring: {from}->{to}"
    );
    let dependent = board.task(from).expect("from exists");
    assert!(
        dependent.blocked_by.iter().any(|id| id == to),
        "reported pair must be a real edge {from}->{to}"
    );
}

#[test]
fn validate_plan_opens_the_plan_board_and_refuses_with_invalid_kind() {
    let root = temp_dir("plan-cycle");
    let board = Board::open(&root).expect("open board");
    board
        .add_task(NewTask::new("a").id("a").blocked_by(["b"]))
        .expect("add a");
    board
        .add_task(NewTask::new("b").id("b").blocked_by(["a"]))
        .expect("add b");

    let plan = Plan::new(&root).member("w1", "do it");
    let error = validate_plan(&plan).expect_err("plan over a cyclic board must be refused");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(
        error.message().contains("\"a\"") && error.message().contains("\"b\""),
        "mapped error keeps both ids: {}",
        error.message()
    );
}

#[test]
fn validate_plan_accepts_a_sound_board() {
    let root = temp_dir("plan-ok");
    let board = Board::open(&root).expect("open board");
    board.add_task(NewTask::new("a").id("a")).expect("add a");
    board
        .add_task(NewTask::new("b").id("b").blocked_by(["a"]))
        .expect("add b");

    let plan = Plan::new(&root).member("w1", "do it");
    assert!(validate_plan(&plan).is_ok());
}
