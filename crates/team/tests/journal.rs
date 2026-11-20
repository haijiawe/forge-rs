//! The journal's contract: one Lead's operations land on disk in order, can be
//! read back exactly, and different Leads never interfere.
//!
//! The ordering test is the important one. A journal that stored the right
//! entries in the wrong order would still "pass" a naive containment check, so
//! these tests compare the sequence of `seq` and `op` values, not just the set.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use forge_core::ErrorKind;
use forge_team::{Journal, JOURNAL_DIR};

fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "forge-team-journal-{}-{}-{}-{}",
        std::process::id(),
        stamp,
        NEXT.fetch_add(1, Ordering::SeqCst),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn one_lead_operations_are_recorded_in_order_and_read_back() {
    let journal = Journal::open(temp_dir("order")).expect("open journal");
    journal.record("lead-a", "claim", "t1").expect("claim");
    journal
        .record("lead-a", "complete", "t1")
        .expect("complete");
    journal
        .record("lead-a", "reassign", "t2")
        .expect("reassign");

    let entries = journal.entries("lead-a").expect("read back");
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.op.as_str())
            .collect::<Vec<_>>(),
        vec!["claim", "complete", "reassign"]
    );
    assert!(entries.iter().all(|entry| entry.lead == "lead-a"));
    assert_eq!(journal.last_seq("lead-a").expect("last seq"), 3);
}

#[test]
fn reopening_continues_the_sequence_from_disk() {
    let root = temp_dir("reopen");
    {
        let journal = Journal::open(&root).expect("open journal");
        journal.record("lead-a", "one", "").expect("one");
        journal.record("lead-a", "two", "").expect("two");
    }
    // A fresh handle over the same root must pick up where the file ended.
    let journal = Journal::open(&root).expect("reopen journal");
    let entry = journal.record("lead-a", "three", "").expect("three");
    assert_eq!(entry.seq, 3);
    assert_eq!(journal.entries("lead-a").expect("read back").len(), 3);
}

#[test]
fn different_leads_have_independent_files_and_sequences() {
    let journal = Journal::open(temp_dir("leads")).expect("open journal");
    journal.record("lead-a", "a1", "").expect("a1");
    journal.record("lead-b", "b1", "").expect("b1");
    journal.record("lead-a", "a2", "").expect("a2");

    assert_ne!(journal.path("lead-a"), journal.path("lead-b"));
    assert_eq!(
        journal.path("lead-a"),
        journal.root().join(JOURNAL_DIR).join("lead-a.jsonl")
    );

    let a: Vec<u64> = journal
        .entries("lead-a")
        .expect("lead-a")
        .iter()
        .map(|entry| entry.seq)
        .collect();
    let b: Vec<u64> = journal
        .entries("lead-b")
        .expect("lead-b")
        .iter()
        .map(|entry| entry.seq)
        .collect();
    assert_eq!(a, vec![1, 2], "lead-a keeps its own sequence");
    assert_eq!(b, vec![1], "lead-b starts at one, unaffected by lead-a");
    assert!(journal
        .entries("lead-b")
        .expect("lead-b")
        .iter()
        .all(|entry| entry.lead == "lead-b"));
}

#[test]
fn a_transaction_writes_all_entries_or_none() {
    let journal = Journal::open(temp_dir("txn")).expect("open journal");

    // Committed transaction: both entries appear, in staging order.
    journal
        .transact("lead-t", |txn| {
            txn.record("first", "")?;
            txn.record("second", "")?;
            Ok(())
        })
        .expect("committed transaction");
    let entries = journal.entries("lead-t").expect("read back");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.op.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );

    // Aborted transaction: the staged entry is discarded, nothing is appended.
    let refused: forge_core::Result<()> = journal.transact("lead-t", |txn| {
        txn.record("should-not-land", "")?;
        Err(forge_core::Error::new(ErrorKind::Refused, "abort"))
    });
    assert!(refused.is_err());
    assert_eq!(
        journal.entries("lead-t").expect("read back").len(),
        2,
        "an aborted transaction must not write anything"
    );
    assert_eq!(journal.last_seq("lead-t").expect("last seq"), 2);
}

#[test]
fn empty_operation_is_refused() {
    let journal = Journal::open(temp_dir("empty")).expect("open journal");
    let error = journal
        .record("lead-a", "   ", "")
        .expect_err("empty op refused");
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(journal.entries("lead-a").expect("read back").is_empty());
}

#[test]
fn invalid_lead_name_is_refused() {
    let journal = Journal::open(temp_dir("bad")).expect("open journal");
    let error = journal
        .record("../escape", "op", "")
        .expect_err("bad name refused");
    assert_eq!(error.kind(), ErrorKind::Invalid);
}

#[test]
fn concurrent_records_for_one_lead_get_unique_ordered_sequences() {
    let journal = Journal::open(temp_dir("race")).expect("open journal");
    let threads = 16u64;
    let mut handles = Vec::new();
    for index in 0..threads {
        let clone = journal.clone();
        handles.push(std::thread::spawn(move || {
            clone
                .record("lead-c", "op", &format!("{index}"))
                .expect("record")
                .seq
        }));
    }
    let mut seqs: Vec<u64> = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect();
    seqs.sort_unstable();
    assert_eq!(
        seqs,
        (1..=threads).collect::<Vec<_>>(),
        "no gaps, no repeats"
    );

    let entries = journal.entries("lead-c").expect("read back");
    assert_eq!(entries.len() as u64, threads);
    for window in entries.windows(2) {
        assert!(
            window[0].seq < window[1].seq,
            "file order must be strictly increasing: {:?}",
            entries.iter().map(|entry| entry.seq).collect::<Vec<_>>()
        );
    }
}
