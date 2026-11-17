//! The registry against a real directory. The temp root is per test and per
//! process, and nothing here reads a clock: every instant is the constant the
//! test was written with.

use std::path::PathBuf;

use forge_core::ErrorKind;
use forge_schedule::{Registry, Task};

// 2026-11-17T00:00:00Z
const TUE_0000: i64 = 1_794_873_600_000;
// 2026-11-17T09:00:00Z
const TUE_0900: i64 = 1_794_906_000_000;
// 2026-11-18T09:00:00Z
const WED_0900: i64 = 1_794_992_400_000;
// 2026-11-24T09:00:00Z, a week after TUE_0900.
const WEEK_LATER: i64 = 1_795_510_800_000;

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir()
        .join("forge-schedule-it")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn task(rule: &str, start_ms: i64) -> Task {
    Task::new(rule, "say hello", start_ms, start_ms)
}

#[test]
fn a_missing_file_is_an_empty_registry() {
    let root = scratch("empty");
    let registry = Registry::open(&root);
    assert_eq!(registry.load().expect("loads"), Vec::new());
    assert_eq!(registry.due(TUE_0000).expect("due works"), Vec::new());
    assert!(
        !registry.path().exists(),
        "reading must not create the file"
    );
}

#[test]
fn tasks_round_trip_and_keep_their_rule_text() {
    let root = scratch("roundtrip");
    let registry = Registry::open(&root);
    let mut task = task("FREQ=DAILY;BYHOUR=9", TUE_0000);
    task.target = Some("s_01jya7c9q3r5t7v9w1x3y5z7".into());
    let id = task.id.clone();
    registry.add(task.clone()).expect("add");

    let loaded = registry.get(&id).expect("get works").expect("present");
    assert_eq!(loaded, task, "the rule text survives verbatim");
    assert_eq!(loaded.runs, 0);
    assert_eq!(loaded.last_run_ms, None);

    registry.remove(&id).expect("remove");
    assert_eq!(registry.load().expect("loads"), Vec::new());
    assert!(registry.get(&id).expect("get works").is_none());
}

#[test]
fn a_duplicate_id_is_a_conflict_and_a_missing_one_is_not_found() {
    let root = scratch("ids");
    let registry = Registry::open(&root);
    let task = task("FREQ=DAILY", TUE_0000);
    registry.add(task.clone()).expect("first add");
    let error = registry.add(task).expect_err("duplicate refused");
    assert!(error.is(ErrorKind::Conflict), "{error:?}");

    let error = registry.remove("nope").expect_err("missing refused");
    assert!(error.is(ErrorKind::NotFound), "{error:?}");
}

#[test]
fn a_due_task_fires_once_and_then_waits_for_the_next_occurrence() {
    let root = scratch("due");
    let registry = Registry::open(&root);
    let task = task("FREQ=DAILY;BYHOUR=9", TUE_0000);
    let id = task.id.clone();
    registry.add(task).expect("add");

    // 09:00 on the anchor day is the first occurrence.
    let due = registry.due(TUE_0900).expect("due works");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].at_ms, TUE_0900);
    assert_eq!(due[0].task.id, id);

    // Not marked as run: still the same occurrence, not a second one.
    assert_eq!(
        registry.due(TUE_0900).expect("due works")[0].at_ms,
        TUE_0900
    );

    registry.mark_run(&id, TUE_0900).expect("mark_run");
    assert!(
        registry.due(TUE_0900).expect("due works").is_empty(),
        "the same occurrence must not be delivered twice"
    );

    // The next day it fires again, with the counter bumped.
    let tomorrow = registry.due(WED_0900).expect("due works");
    assert_eq!(tomorrow.len(), 1);
    assert_eq!(tomorrow[0].at_ms, WED_0900);
    assert_eq!(registry.get(&id).expect("get").expect("present").runs, 1);
}

#[test]
fn a_long_gap_owes_the_oldest_occurrence_first() {
    // A machine asleep for a week owes seven deliveries, and it owes them in
    // order: the tick after a week still hands over the first one, so no
    // occurrence is silently dropped in favour of the newest.
    let root = scratch("gap");
    let registry = Registry::open(&root);
    let task = task("FREQ=DAILY;BYHOUR=9", TUE_0000);
    let id = task.id.clone();
    registry.add(task).expect("add");

    let due = registry.due(WEEK_LATER).expect("due works");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].at_ms, TUE_0900, "the oldest owed occurrence");

    // Marking it delivered moves the task on to the second of the seven.
    registry.mark_run(&id, due[0].at_ms).expect("mark_run");
    let next = registry.due(WEEK_LATER).expect("due works");
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].at_ms, WED_0900);
}

#[test]
fn nothing_is_due_before_the_first_occurrence() {
    let root = scratch("early");
    let registry = Registry::open(&root);
    registry
        .add(task("FREQ=DAILY;BYHOUR=9", TUE_0000))
        .expect("add");
    assert_eq!(registry.due(TUE_0000).expect("due works"), Vec::new());
}

#[test]
fn an_unreadable_rule_is_kept_but_never_due() {
    let root = scratch("badrule");
    let registry = Registry::open(&root);
    // Written by a version that knew more parts than this one: the task stays
    // in the file (it is still the operator's), it just does not fire.
    let task = task("FREQ=DAILY;BYMONTH=3", TUE_0000);
    registry.add(task.clone()).expect("add");
    assert_eq!(registry.due(TUE_0000).expect("due works"), Vec::new());
    assert_eq!(task.next_after_ms(), None);
    assert_eq!(
        registry.get(&task.id).expect("get").expect("present").rule,
        task.rule
    );
}

#[test]
fn a_task_with_no_target_is_still_a_task() {
    let root = scratch("notarget");
    let registry = Registry::open(&root);
    let task = task("FREQ=DAILY", TUE_0000);
    registry.add(task.clone()).expect("add");
    let loaded = registry.get(&task.id).expect("get").expect("present");
    assert_eq!(loaded.target, None, "a fresh session per occurrence");
}

#[test]
fn the_saved_file_is_complete_json_at_every_moment() {
    // `save` writes beside the file and renames, so a reader either sees the
    // old list or the new one. This pins the absence of a stray temp file.
    let root = scratch("atomic");
    let registry = Registry::open(&root);
    registry.add(task("FREQ=DAILY", TUE_0000)).expect("add");
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .expect("dir exists")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, vec!["tasks.json".to_string()], "{leftovers:?}");
    let text = std::fs::read_to_string(registry.path()).expect("readable");
    serde_json::from_str::<Vec<Task>>(&text).expect("valid JSON");
}

#[test]
fn a_truncated_file_is_an_error_rather_than_an_empty_schedule() {
    // Half a JSON document must not read as "no tasks": the next save would
    // then erase every schedule the operator had.
    let root = scratch("truncated");
    let registry = Registry::open(&root);
    std::fs::create_dir_all(&root).expect("dir");
    std::fs::write(registry.path(), b"[{\"id\": ").expect("write");
    let error = registry.load().expect_err("refused");
    assert!(error.is(ErrorKind::Json), "{error:?}");
    assert!(
        error.message().contains("tasks.json"),
        "{}",
        error.message()
    );
}

#[test]
fn a_task_without_an_id_cannot_be_added() {
    let root = scratch("noid");
    let registry = Registry::open(&root);
    let mut task = task("FREQ=DAILY", TUE_0000);
    task.id.clear();
    let error = registry.add(task).expect_err("refused");
    assert!(error.is(ErrorKind::Invalid), "{error:?}");
}

#[test]
fn due_is_sorted_by_instant_then_id() {
    let root = scratch("order");
    let registry = Registry::open(&root);
    registry
        .add(task("FREQ=DAILY;BYHOUR=18", TUE_0000))
        .expect("add");
    registry
        .add(task("FREQ=DAILY;BYHOUR=9", TUE_0000))
        .expect("add");
    let due = registry.due(WED_0900).expect("due works");
    assert_eq!(due.len(), 2);
    assert_eq!(due[0].at_ms, TUE_0900, "09:00 first");
    assert_eq!(due[1].at_ms, 1_794_938_400_000, "then 18:00");
    assert!(due[0].at_ms < due[1].at_ms);
}
