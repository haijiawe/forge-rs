//! The board's contract: compare-and-set, readiness, and the graph defects that
//! would otherwise be silent deadlocks.
//!
//! The race test is the important one. Everything else here is arithmetic; the
//! race is the failure this crate exists to prevent, and a test that only ran
//! one thread claiming would pass even if `claim` ignored `expected_revision`
//! entirely.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use forge_core::ErrorKind;
use forge_team::{Board, NewTask, ProblemKind, Status, TaskPatch};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    // The nanosecond stamp matters: a process id is recycled by the OS, so a
    // name built from it alone can land on a directory an earlier run left
    // behind — and then a test reads that run's board and believes it is new.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "forge-team-board-{}-{}-{}-{}",
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
fn a_new_task_starts_pending_at_revision_one() {
    let board = board("new");
    let task = board
        .add_task(NewTask::new("write the tests").id("t1"))
        .expect("add task");
    assert_eq!(task.status, Status::Pending);
    assert_eq!(task.revision, 1);
    assert_eq!(task.owner, None);
    // Adding a task is itself a board transition, so the board revision moved.
    assert_eq!(board.revision(), 1);
}

#[test]
fn two_threads_claiming_with_the_same_revision_produce_exactly_one_winner() {
    let board = Arc::new(board("race"));
    board
        .add_task(NewTask::new("contended").id("t1"))
        .expect("add task");

    // Both threads read revision 1 before either writes, which is exactly the
    // stale-snapshot the CAS must catch. The barrier makes that deterministic.
    let gate = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for owner in ["alpha", "beta"] {
        let board = Arc::clone(&board);
        let gate = Arc::clone(&gate);
        handles.push(std::thread::spawn(move || {
            gate.wait();
            board.claim("t1", 1, owner)
        }));
    }
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("join claim thread"))
        .collect();

    let winners = results.iter().filter(|result| result.is_ok()).count();
    let conflicts = results
        .iter()
        .filter(|result| {
            matches!(
                result,
                Err(error) if error.kind() == ErrorKind::Conflict
            )
        })
        .count();
    assert_eq!(winners, 1, "exactly one claim may win: {results:?}");
    assert_eq!(conflicts, 1, "the loser must see a conflict: {results:?}");

    let task = board.task("t1").expect("task still exists");
    assert_eq!(task.status, Status::InProgress);
    assert_eq!(task.revision, 2, "one applied transition, not two");
    let winner = results
        .iter()
        .find_map(|result| match result {
            Ok(task) => task.owner.clone(),
            Err(_) => None,
        })
        .expect("the winning claim names an owner");
    assert!(
        winner == "alpha" || winner == "beta",
        "the owner must be one of the two claimants, not {winner:?}"
    );
    assert_eq!(task.owner.as_deref(), Some(winner.as_str()));
}

#[test]
fn a_stale_revision_is_a_conflict_not_a_silent_overwrite() {
    let board = board("stale");
    board
        .add_task(NewTask::new("one").id("t1"))
        .expect("add task");
    board.claim("t1", 1, "alpha").expect("first claim");
    let error = board
        .claim("t1", 1, "beta")
        .expect_err("stale claim must fail");
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(
        error.message().contains("revision 2"),
        "the message must name the current revision: {}",
        error.message()
    );
    assert_eq!(
        board.task("t1").expect("task").owner.as_deref(),
        Some("alpha")
    );
}

#[test]
fn a_completed_task_cannot_be_claimed_again() {
    let board = board("completed");
    board
        .add_task(NewTask::new("one").id("t1"))
        .expect("add task");
    board.claim("t1", 1, "alpha").expect("claim");
    board.complete("t1", 2).expect("complete");
    let error = board
        .claim("t1", 3, "beta")
        .expect_err("completed task is not claimable");
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(error.message().contains("completed"));
}

#[test]
fn a_dependency_holds_a_task_until_its_blocker_completes() {
    let board = board("dependency");
    board.add_task(NewTask::new("a").id("a")).expect("add a");
    board
        .add_task(NewTask::new("b").id("b").blocked_by(["a"]))
        .expect("add b");

    assert_eq!(ready_ids(&board), vec!["a"], "b must not be ready yet");
    // Not ready means not claimable-by-the-dispatcher, but the strict claim is
    // what a caller uses; the dependency is about readiness, not about a lock.
    board.claim("a", 1, "alpha").expect("claim a");
    board.complete("a", 2).expect("complete a");
    assert_eq!(
        ready_ids(&board),
        vec!["b"],
        "b becomes ready once a is done"
    );
}

#[test]
fn a_dependency_on_a_missing_task_is_named_as_a_problem() {
    let board = board("dangling");
    board
        .add_task(NewTask::new("b").id("b").blocked_by(["ghost"]))
        .expect("add b");

    // The task is unready forever, and readiness alone cannot say why.
    assert!(ready_ids(&board).is_empty());
    let problems = board.board_problems();
    assert_eq!(problems.len(), 1, "one problem, not a silent deadlock");
    assert_eq!(problems[0].kind, ProblemKind::DanglingDependency);
    assert_eq!(
        problems[0].tasks,
        vec!["b".to_string(), "ghost".to_string()]
    );
    assert!(
        problems[0].message.contains("ghost"),
        "the problem must name the phantom id: {}",
        problems[0].message
    );
}

#[test]
fn a_dependency_ring_is_reported_once_per_ring() {
    let board = board("cycle");
    board
        .add_task(NewTask::new("a").id("a").blocked_by(["b"]))
        .expect("add a");
    board
        .add_task(NewTask::new("b").id("b").blocked_by(["a"]))
        .expect("add b");
    let problems = board.board_problems();
    assert_eq!(problems.len(), 1, "one ring, one problem: {problems:?}");
    assert_eq!(problems[0].kind, ProblemKind::DependencyCycle);
    assert_eq!(problems[0].tasks, vec!["a".to_string(), "b".to_string()]);
    assert!(ready_ids(&board).is_empty());
}

#[test]
fn a_self_dependency_is_its_own_problem() {
    let board = board("self");
    board
        .add_task(NewTask::new("a").id("a").blocked_by(["a"]))
        .expect("add a");
    let problems = board.board_problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].kind, ProblemKind::SelfDependency);
    assert!(ready_ids(&board).is_empty());
}

#[test]
fn overlapping_write_scopes_warn_without_blocking_the_claim() {
    let board = board("overlap");
    board
        .add_task(NewTask::new("a").id("a").scope("crates/team/src"))
        .expect("add a");
    board
        .add_task(NewTask::new("b").id("b").scope("crates/team/src/board.rs"))
        .expect("add b");

    // Both claims succeed: a warning is a hint, not a lock. If this ever starts
    // failing, the crate has turned advice into a gate and callers will learn to
    // ignore it.
    board.claim("a", 1, "alpha").expect("claim a");
    board.claim("b", 1, "beta").expect("claim b");

    let warnings = board.warned_scopes();
    assert_eq!(warnings.len(), 1, "one warning per task pair: {warnings:?}");
    assert_eq!(warnings[0].tasks, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(
        warnings[0].scopes,
        vec![
            "crates/team/src".to_string(),
            "crates/team/src/board.rs".to_string()
        ]
    );
}

#[test]
fn sibling_names_that_only_share_a_prefix_do_not_warn() {
    let board = board("siblings");
    board
        .add_task(NewTask::new("a").id("a").scope("src/a"))
        .expect("add a");
    board
        .add_task(NewTask::new("b").id("b").scope("src/ab"))
        .expect("add b");
    board.claim("a", 1, "alpha").expect("claim a");
    board.claim("b", 1, "beta").expect("claim b");
    assert!(
        board.warned_scopes().is_empty(),
        "src/a and src/ab are different directories, not a prefix overlap"
    );
}

#[test]
fn only_in_progress_tasks_are_compared_for_scope_overlap() {
    let board = board("overlap-status");
    board
        .add_task(NewTask::new("a").id("a").scope("src"))
        .expect("add a");
    board
        .add_task(NewTask::new("b").id("b").scope("src"))
        .expect("add b");
    board.claim("a", 1, "alpha").expect("claim a");
    assert!(
        board.warned_scopes().is_empty(),
        "a pending task is not competing with anyone yet"
    );
    board.claim("b", 1, "beta").expect("claim b");
    assert_eq!(board.warned_scopes().len(), 1);
    board.complete("b", 2).expect("complete b");
    assert!(
        board.warned_scopes().is_empty(),
        "a completed task has stopped editing"
    );
}

#[test]
fn every_transition_advances_both_revisions_by_exactly_one() {
    let board = board("revisions");
    board
        .add_task(NewTask::new("one").id("t1"))
        .expect("add task");
    assert_eq!(board.revision(), 1);
    assert_eq!(board.task("t1").expect("task").revision, 1);

    board.claim("t1", 1, "alpha").expect("claim");
    assert_eq!(board.revision(), 2);
    assert_eq!(board.task("t1").expect("task").revision, 2);

    board
        .edit("t1", 2, TaskPatch::new().describe("more detail"))
        .expect("edit");
    assert_eq!(board.revision(), 3);
    assert_eq!(board.task("t1").expect("task").revision, 3);

    board
        .set_dependencies("t1", 3, vec!["x".into(), "x".into()])
        .expect("set dependencies");
    assert_eq!(board.revision(), 4);
    let task = board.task("t1").expect("task");
    assert_eq!(task.revision, 4);
    assert_eq!(task.blocked_by, vec!["x".to_string()], "deduplicated");
}

#[test]
fn a_reopened_task_is_pending_and_unowned() {
    let board = board("reopen");
    board
        .add_task(NewTask::new("one").id("t1"))
        .expect("add task");
    board.claim("t1", 1, "alpha").expect("claim");
    let task = board.reopen("t1", 2).expect("reopen");
    assert_eq!(task.status, Status::Pending);
    assert_eq!(task.owner, None);
    // Reopening an already-pending task is a conflict: it would be a transition
    // that changed nothing, and the caller is clearly acting on stale state.
    assert_eq!(
        board.reopen("t1", 3).expect_err("no-op reopen").kind(),
        ErrorKind::Conflict
    );
}

#[test]
fn the_board_survives_a_reopen_from_disk() {
    let dir = temp_dir("persist");
    {
        let board = Board::open(&dir).expect("open");
        board
            .add_task(NewTask::new("one").id("t1"))
            .expect("add task");
        board.claim("t1", 1, "alpha").expect("claim");
    }
    let reopened = Board::open(&dir).expect("reopen");
    let task = reopened.task("t1").expect("task survived");
    assert_eq!(task.status, Status::InProgress);
    assert_eq!(task.owner.as_deref(), Some("alpha"));
    assert_eq!(task.revision, 2);
    assert_eq!(reopened.revision(), 2);
}

#[test]
fn the_board_file_is_valid_json_with_the_documented_shape() {
    let board = board("shape");
    board
        .add_task(NewTask::new("one").id("t1").scope("src"))
        .expect("add task");
    let text = std::fs::read_to_string(board.path()).expect("read board");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(value["version"], 1);
    assert_eq!(value["revision"], 1);
    let task = &value["tasks"][0];
    assert_eq!(task["id"], "t1");
    assert_eq!(task["status"], "pending");
    assert_eq!(task["revision"], 1);
    assert_eq!(task["blockedBy"], serde_json::json!([]));
    assert_eq!(task["writeScopes"], serde_json::json!(["src"]));
}

#[test]
fn a_newer_board_version_is_refused_rather_than_silently_downgraded() {
    let dir = temp_dir("version");
    std::fs::write(
        dir.join("board.json"),
        r#"{"version": 2, "revision": 0, "tasks": []}"#,
    )
    .expect("write board");
    let error = Board::open(&dir).expect_err("newer version must be refused");
    assert_eq!(error.kind(), ErrorKind::UnsupportedVersion);
}

#[test]
fn a_bad_worker_name_is_refused_before_it_becomes_a_path() {
    let board = board("names");
    // An empty id is not in this list on purpose: it means "mint one", which is
    // the board's documented convenience rather than an unusable name.
    for bad in ["../escape", "a/b", "con", ".hidden", "trailing."] {
        let error = board
            .add_task(NewTask::new("one").id(bad))
            .expect_err("unusable id must be refused");
        assert_eq!(error.kind(), ErrorKind::Invalid, "id {bad:?}");
    }
    let long = "x".repeat(65);
    let error = board
        .add_task(NewTask::new("one").id(long.as_str()))
        .expect_err("an over-long id must be refused");
    assert_eq!(error.kind(), ErrorKind::Invalid, "id too long");
}

fn ready_ids(board: &Board) -> Vec<String> {
    board.ready().into_iter().map(|task| task.id).collect()
}
