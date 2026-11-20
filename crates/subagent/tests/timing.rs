//! Time projection tests: `subagentTiming` folded out of real child logs.
//!
//! Every fixture is a log written through [`hearth_session::Store`] with `ts` set
//! explicitly — `Store::append` only stamps a timestamp when the caller left it
//! zero — so each duration asserted here is exact rather than measured, and the
//! tests do not depend on how fast this box happens to run.

use std::path::{Path, PathBuf};

use hearth_core::{ErrorKind, Event, EventType};
use hearth_session::{Options as StoreOptions, Store};
use hearth_subagent::catalog::CatalogEntry;
use hearth_subagent::descriptor::Mode;
use hearth_subagent::timing::{children, of_child, of_log, Timing};
use serde_json::json;

/// A data dir with a `sessions/` child that is removed when the guard drops.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!(
            "hearth-subagent-timing-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sessions")).expect("create scratch sessions dir");
        Scratch { root }
    }

    fn data_dir(&self) -> &Path {
        &self.root
    }

    fn log(&self, id: &str) -> PathBuf {
        self.root.join("sessions").join(format!("{id}.jsonl"))
    }

    fn store(&self, id: &str) -> Store {
        Store::open(
            self.log(id),
            StoreOptions {
                no_fsync: true,
                ..StoreOptions::default()
            },
        )
        .expect("open scratch log")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn at(typ: EventType, ts: i64) -> Event {
    let mut event = Event::new("s_child", typ, None);
    event.ts = ts;
    event
}

fn start(ts: i64, index: Option<u64>) -> Event {
    let mut event = at(EventType::TURN_START, ts);
    if let Some(index) = index {
        event.payload = Some(json!({ "index": index }));
    }
    event
}

fn end(ts: i64, index: Option<u64>) -> Event {
    let mut event = at(EventType::TURN_END, ts);
    if let Some(index) = index {
        event.payload = Some(json!({ "index": index }));
    }
    event
}

#[test]
fn two_closed_turns_sum_and_the_last_end_wins() {
    let events = vec![
        start(1_000, Some(1)),
        end(1_250, Some(1)),
        start(2_000, Some(2)),
        end(2_400, Some(2)),
    ];
    let timing = of_log("20260101-aaaa0001", &events);
    assert_eq!(timing.child_id, "20260101-aaaa0001");
    assert_eq!(timing.settled_ms, 250 + 400, "both stretches are banked");
    assert_eq!(timing.turns, 2);
    assert!(!timing.active, "nothing is open");
    assert_eq!(
        timing.last_turn_completed,
        Some(2_400),
        "the later end is the last completion"
    );
}

#[test]
fn an_open_turn_is_active_and_only_the_closed_one_settles() {
    let events = vec![
        start(1_000, Some(1)),
        end(1_150, Some(1)),
        start(2_000, Some(2)),
    ];
    let timing = of_log("20260101-aaaa0002", &events);
    assert_eq!(timing.settled_ms, 150, "the open turn has not run down yet");
    assert_eq!(timing.turns, 1);
    assert!(timing.active, "turn 2 opened and never closed");
    assert_eq!(timing.last_turn_completed, Some(1_150));
}

#[test]
fn a_backwards_clock_contributes_zero_and_never_a_negative() {
    let events = vec![
        start(1_000, Some(1)),
        end(500, Some(1)),
        start(2_000, Some(2)),
        end(2_300, Some(2)),
    ];
    let timing = of_log("20260101-aaaa0003", &events);
    assert_eq!(
        timing.settled_ms, 300,
        "the backwards turn is a zero, not -500"
    );
    assert!(timing.settled_ms >= 0);
    assert_eq!(timing.turns, 2, "a paired turn still counts as a turn");
    assert_eq!(timing.last_turn_completed, Some(2_300));
}

#[test]
fn without_an_index_turns_pair_in_arrival_order() {
    let events = vec![
        end(400, None),
        start(1_000, None),
        start(2_000, None),
        end(1_500, None),
        end(2_600, None),
    ];
    let timing = of_log("20260101-aaaa0004", &events);
    // First start with first end (500), second start with second end (600).
    // Pairing the other way round would give 1_600, so this sum decides it.
    assert_eq!(timing.settled_ms, 1_100);
    assert_eq!(timing.turns, 2);
    assert_eq!(timing.last_turn_completed, Some(2_600));
    assert!(!timing.active, "the orphan end at 400 closes nothing");
}

#[test]
fn session_end_closes_a_turn_the_log_left_open() {
    let events = vec![
        start(1_000, Some(1)),
        end(1_100, Some(1)),
        at(EventType::SESSION_END, 1_200),
        start(1_300, Some(2)),
    ];
    let timing = of_log("20260101-aaaa0005", &events);
    assert_eq!(timing.settled_ms, 100);
    assert_eq!(timing.turns, 1);
    assert!(!timing.active, "an ended session has no running turn");

    let ended_before_any_turn = vec![at(EventType::SESSION_END, 1_200), start(1_300, Some(1))];
    let timing = of_log("20260101-aaaa0005", &ended_before_any_turn);
    assert!(!timing.active);
    assert_eq!(timing.settled_ms, 0);
    assert_eq!(timing.turns, 0);
    assert_eq!(timing.last_turn_completed, None);
}

#[test]
fn of_child_separates_a_bad_id_from_a_missing_log() {
    let scratch = Scratch::new("child-errors");
    let missing = "20260101-deadbeef";
    let err = of_child(scratch.data_dir(), missing).expect_err("no log for this child");
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(err.to_string().contains(missing), "{err}");

    let escape = "../../etc/passwd";
    let err = of_child(scratch.data_dir(), escape).expect_err("not a child id");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    assert!(err.to_string().contains("../../etc/passwd"), "{err}");

    // A log that exists but recorded no turn is a child with zero timing, not a
    // missing child: the row is real, its numbers are simply zero.
    let quiet = "20260101-cccc0003";
    let store = scratch.store(quiet);
    store
        .append(&at(EventType::SESSION_START, 900))
        .expect("append head");
    let timing = of_child(scratch.data_dir(), quiet).expect("read");
    assert_eq!(
        timing,
        Timing {
            child_id: quiet.to_string(),
            settled_ms: 0,
            active: false,
            last_turn_completed: None,
            turns: 0,
        }
    );
}

#[test]
fn children_lists_every_catalog_row_sorted_and_survives_a_missing_log() {
    let scratch = Scratch::new("children");
    let parent = "s_parent_timing";
    let (first, absent, third) = (
        "20260101-aaaa0001",
        "20260101-bbbb0002",
        "20260101-cccc0003",
    );

    // Catalog rows are written out of order on purpose: the answer must be
    // sorted by child_id, not by the order the parent happened to create them.
    let parent_store = scratch.store(parent);
    for (index, id) in [third, first, absent].into_iter().enumerate() {
        let row = CatalogEntry::new(id, Mode::OneShot).with_name(format!("agent-{}", index + 1));
        parent_store
            .append(&row.event(parent))
            .expect("append catalog row");
    }

    let first_store = scratch.store(first);
    first_store
        .append(&at(EventType::SESSION_START, 950))
        .expect("head");
    first_store.append(&start(1_000, Some(1))).expect("start");
    first_store.append(&end(1_600, Some(1))).expect("end");

    let third_store = scratch.store(third);
    third_store
        .append(&at(EventType::SESSION_START, 1_900))
        .expect("head");
    third_store.append(&start(2_000, Some(1))).expect("start");
    third_store.append(&end(2_100, Some(1))).expect("end");
    third_store
        .append(&start(3_000, Some(2)))
        .expect("open start");

    let rows = children(scratch.data_dir(), parent).expect("read children");
    let ids: Vec<&str> = rows.iter().map(|row| row.child_id.as_str()).collect();
    assert_eq!(ids, vec![first, absent, third], "sorted by child_id");

    assert_eq!(rows[0].settled_ms, 600);
    assert_eq!(rows[0].turns, 1);
    assert_eq!(rows[0].last_turn_completed, Some(1_600));
    assert!(!rows[0].active);

    // Recorded on the parent, no log on this disk: the row stays, with zeroes.
    assert_eq!(
        rows[1],
        Timing {
            child_id: absent.to_string(),
            settled_ms: 0,
            active: false,
            last_turn_completed: None,
            turns: 0,
        }
    );

    assert_eq!(rows[2].settled_ms, 100);
    assert_eq!(rows[2].turns, 1);
    assert!(rows[2].active, "the third child left a turn open");

    // Same data, same bytes: a second read cannot disagree with the first.
    let again = children(scratch.data_dir(), parent).expect("read children again");
    assert_eq!(again, rows);
    assert_eq!(format!("{again:?}"), format!("{rows:?}"));

    // A parent with no log at all has no children; that is an answer, not a
    // failure.
    let none = children(scratch.data_dir(), "s_parent_never_ran").expect("read absent parent");
    assert!(none.is_empty(), "no catalog, no rows");
}
