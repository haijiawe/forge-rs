//! The read side of `GET /api/presents`, against real temp directories.
//!
//! What is under test is [`Report::read`]: the parsing, the ordering, the bounds
//! and — the one thing a read-only route may never do — the promise that opening
//! the manifest for reading creates neither the file nor its directory.
//!
//! The route module is private in `lib.rs` (`mod present_route;`, the same way
//! `goal_route` and `teams` are) and the lead owns that file and the HTTP
//! wiring, so this test compiles the module from its source instead of importing
//! it. That also keeps the test honest about its subject: a data directory and
//! no server. `allow(dead_code)` is for the items the library compiles and this
//! binary happens not to call.

#[path = "../src/present_route.rs"]
#[allow(dead_code)]
mod present_route;

use present_route::{Entry, Report, MANIFEST_NAME, MAX_BYTES, MAX_ENTRIES};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A scratch directory under the system temp dir, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hearth-present-route-{}-{}-{}",
            label,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create scratch dir");
        TempDir(path)
    }

    fn dir(&self) -> &Path {
        &self.0
    }

    fn manifest(&self) -> PathBuf {
        self.0.join(MANIFEST_NAME)
    }

    fn write(&self, text: &str) -> &TempDir {
        fs::write(self.manifest(), text).expect("write manifest");
        self
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One manifest line, spelled the way `present` writes it.
fn line(ts: u64, path: &str, size: u64, description: &str) -> String {
    format!(
        "{{\"ts\":{ts},\"path\":\"{path}\",\"size\":{size},\"description\":\"{description}\"}}\n"
    )
}

fn entry(ts: u64, path: &str, size: u64, description: &str) -> Entry {
    Entry {
        ts,
        path: path.to_string(),
        size,
        description: description.to_string(),
    }
}

#[test]
fn three_good_lines_come_back_newest_first() {
    let tree = TempDir::new("three");
    let mut text = String::new();
    text.push_str(&line(1732000000000, "reports/q1.xlsx", 10240, "first"));
    text.push_str(&line(1732000001000, "reports/q2.xlsx", 15360, "second"));
    text.push_str(&line(
        1732000002000,
        "reports/q3.xlsx",
        20480,
        "the numbers you asked for",
    ));
    tree.write(&text);

    let report = Report::read(tree.dir());
    assert_eq!(
        report.presents,
        vec![
            entry(
                1732000002000,
                "reports/q3.xlsx",
                20480,
                "the numbers you asked for"
            ),
            entry(1732000001000, "reports/q2.xlsx", 15360, "second"),
            entry(1732000000000, "reports/q1.xlsx", 10240, "first"),
        ],
        "the appended manifest must be reversed: the newest handoff is the last line"
    );
    assert_eq!(report.skipped, 0);
    assert!(!report.truncated);
    assert_eq!(report.note, None, "a clean manifest needs no diagnostic");
}

/// The wire shape the contract documents, byte for byte, so a client written
/// against `docs/contracts/api.md` §4.15 decodes what the route really sends.
#[test]
fn the_single_entry_answer_is_the_documented_json() {
    let tree = TempDir::new("wire");
    tree.write(&line(
        1732000000000,
        "reports/q3.xlsx",
        20480,
        "the numbers you asked for",
    ));

    let report = Report::read(tree.dir());
    assert_eq!(
        serde_json::to_string(&report).expect("encode report"),
        r#"{"presents":[{"ts":1732000000000,"path":"reports/q3.xlsx","size":20480,"description":"the numbers you asked for"}],"skipped":0,"truncated":false}"#
    );
}

#[test]
fn bad_lines_are_counted_and_the_readable_ones_still_answer() {
    let tree = TempDir::new("damaged");
    let mut text = String::new();
    text.push_str(&line(1732000000000, "reports/keep-one.md", 12, "kept"));
    text.push_str("this line is not JSON at all\n");
    text.push_str(&line(1732000003000, "reports/keep-two.md", 34, "kept too"));
    // A JSON object that is not a handoff: no `path`, so there is nothing an
    // interface could open.
    text.push_str(r#"{"ts":1732000002000,"size":5,"description":"no path here"}"#);
    text.push('\n');
    tree.write(&text);

    let report = Report::read(tree.dir());
    assert_eq!(
        report.presents,
        vec![
            entry(1732000003000, "reports/keep-two.md", 34, "kept too"),
            entry(1732000000000, "reports/keep-one.md", 12, "kept"),
        ],
        "two unreadable lines must not cost the two good ones"
    );
    assert_eq!(report.skipped, 2, "both bad lines are counted, not hidden");
    assert!(!report.truncated);
}

/// The rule that makes the route read-only in the strict sense: a missing
/// manifest is a fact, and asking for it must not bring it into being.
#[test]
fn a_missing_manifest_is_empty_and_is_still_missing() {
    let tree = TempDir::new("missing");
    assert!(tree.dir().is_dir(), "the data directory exists");
    assert!(!tree.manifest().exists(), "no manifest yet");

    let report = Report::read(tree.dir());
    assert!(report.presents.is_empty());
    assert_eq!(report.skipped, 0);
    assert!(!report.truncated);
    assert_eq!(report.note, None, "nothing presented is not an error");
    assert_eq!(
        serde_json::to_string(&report).expect("encode report"),
        r#"{"presents":[],"skipped":0,"truncated":false}"#
    );
    assert!(
        !tree.manifest().exists(),
        "the read created {}",
        tree.manifest().display()
    );
}

#[test]
fn a_missing_directory_is_empty_and_is_still_missing() {
    let tree = TempDir::new("missing-dir");
    let data = tree.dir().join("nested").join("data");
    assert!(!data.exists());

    let report = Report::read(&data);
    assert!(report.presents.is_empty());
    assert_eq!(report.skipped, 0);
    assert!(!report.truncated);
    assert_eq!(report.note, None);
    assert!(
        !data.exists(),
        "the read created the data directory {}",
        data.display()
    );
}

#[test]
fn a_line_without_a_stamp_a_size_or_a_description_still_counts() {
    let tree = TempDir::new("sparse");
    let mut text = String::new();
    // Every default at once.
    text.push_str("{\"path\":\"reports/bare.md\"}\n");
    // Wrong types: a negative size and stamp are not bytes or millis, and a
    // description that is not a string is not a description.
    text.push_str("{\"ts\":-1,\"path\":\"reports/wrong.md\",\"size\":-5,\"description\":7}\n");
    tree.write(&text);

    let report = Report::read(tree.dir());
    assert_eq!(
        report.presents,
        vec![
            entry(0, "reports/wrong.md", 0, ""),
            entry(0, "reports/bare.md", 0, ""),
        ],
        "a missing member degrades to a default; it does not drop the handoff"
    );
    assert_eq!(report.skipped, 0);
    assert!(!report.truncated);
}

#[test]
fn more_than_max_entries_keeps_the_newest_and_says_it_was_cut() {
    let tree = TempDir::new("many");
    let total = MAX_ENTRIES + 37;
    let mut text = String::new();
    for index in 0..total {
        text.push_str(&line(
            1732000000000 + index as u64,
            &format!("notes/{index}.md"),
            10,
            "a handoff",
        ));
    }
    tree.write(&text);

    let report = Report::read(tree.dir());
    assert_eq!(
        report.presents.len(),
        MAX_ENTRIES,
        "the answer is capped at {MAX_ENTRIES} entries"
    );
    assert!(report.truncated, "the cut must be reported");
    assert_eq!(report.skipped, 0);
    assert_eq!(
        report.presents[0].path,
        format!("notes/{}.md", total - 1),
        "the newest handoff is first"
    );
    assert_eq!(
        report.presents[MAX_ENTRIES - 1].path,
        format!("notes/{}.md", total - MAX_ENTRIES),
        "the cap drops the oldest entries, not the newest"
    );
}

/// A manifest over the byte cap: the newest lines must still answer, and the
/// read must not have anchored on the head of the file (which would report the
/// oldest handoffs and call them the newest).
#[test]
fn a_manifest_over_the_byte_cap_reads_its_newest_tail() {
    let tree = TempDir::new("bytes");
    let filler = "x".repeat(1000);
    let mut text = String::new();
    let mut index = 0u64;
    while (text.len() as u64) <= MAX_BYTES + 4096 {
        text.push_str(&line(
            index,
            &format!("old/{index}.md"),
            1,
            &format!("filler {filler}"),
        ));
        index += 1;
    }
    let newest = index;
    text.push_str(&line(
        1732000000000,
        "reports/newest.xlsx",
        20480,
        "the newest handoff",
    ));
    tree.write(&text);

    let report = Report::read(tree.dir());
    assert!(report.truncated, "a capped read must say it was capped");
    assert_eq!(report.skipped, 0, "the cut is the reader's, not the data's");
    assert_eq!(report.presents.len(), MAX_ENTRIES);
    assert_eq!(
        report.presents[0].path, "reports/newest.xlsx",
        "the tail of an append-only manifest holds the newest handoff (newest line was {newest})"
    );
    assert!(
        report.note.as_deref().unwrap_or("").contains("older"),
        "the note must say which end is missing: {:?}",
        report.note
    );
}

/// A manifest that exists but is not a regular file is a diagnostic, not a
/// panic and not a silent "nothing has been presented".
#[test]
fn a_manifest_that_is_not_a_file_answers_with_a_note() {
    let tree = TempDir::new("directory");
    fs::create_dir_all(tree.manifest()).expect("create a directory in the manifest's place");

    let report = Report::read(tree.dir());
    assert!(report.presents.is_empty());
    assert!(!report.truncated);
    assert!(
        report
            .note
            .as_deref()
            .unwrap_or("")
            .contains("regular file"),
        "the note says what is wrong: {:?}",
        report.note
    );
    assert!(tree.manifest().is_dir(), "the read did not replace it");
}
