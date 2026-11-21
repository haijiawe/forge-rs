//! The contract documents and this crate must agree, and a test should say so.
//!
//! `EventType::ALL` used to carry a comment claiming "a test asserts it matches
//! the contract document". There was no such test: the numbers in the docs were
//! typed by hand, and they had drifted (the catalogue table grew while
//! `docs/contracts/README.md` still said fifteen types). A comment that describes
//! a check nobody wrote is worse than no comment, because it stops the next
//! person from looking.
//!
//! These tests read the documents and compare them with the code. They are
//! deliberately about *agreement*, not about wording: they parse the one table
//! whose rows are the catalogue, and the handful of counts that prose repeats.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use hearth_core::EventType;

/// The repository root, found from this crate's manifest directory.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// The event type names in the catalogue table of `events-v1.md` §4.
///
/// The table starts at the row naming `type` and `Emitted when`, and ends at the
/// first line that is not a table row, which is what keeps the tables in the
/// §4.0.x subsections out of this list.
fn catalogue(document: &str) -> Vec<String> {
    let mut lines = document.lines();
    let mut found = Vec::new();
    for line in lines.by_ref() {
        if line.starts_with('|') && line.contains("Emitted when") {
            break;
        }
    }
    for line in lines {
        if !line.starts_with('|') {
            break;
        }
        // `| `name` | ... |` — the first cell is the type, in backticks.
        let cell = line.split('|').nth(1).unwrap_or("").trim();
        let Some(name) = cell
            .strip_prefix('`')
            .and_then(|rest| rest.split('`').next())
        else {
            continue;
        };
        // The header separator row is all dashes; it has no backticks at all, so
        // it never reaches here.
        if !name.is_empty() {
            found.push(name.to_string());
        }
    }
    found
}

/// Spell a number the way prose does, for the counts that appear as words.
fn spelled(n: usize) -> String {
    const UNITS: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 10] = [
        "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if n < 20 {
        return UNITS[n].to_string();
    }
    if n < 100 {
        let tens = TENS[n / 10];
        return match n % 10 {
            0 => tens.to_string(),
            unit => format!("{tens}-{}", UNITS[unit]),
        };
    }
    n.to_string()
}

#[test]
fn the_catalogue_table_lists_exactly_the_types_the_code_knows() {
    let document = read("docs/contracts/events-v1.md");
    let documented: BTreeSet<String> = catalogue(&document).into_iter().collect();
    let implemented: BTreeSet<String> = EventType::ALL
        .iter()
        .map(|typ| typ.as_str().to_string())
        .collect();

    let missing_from_document: Vec<&String> = implemented.difference(&documented).collect();
    let missing_from_code: Vec<&String> = documented.difference(&implemented).collect();

    assert!(
        missing_from_document.is_empty() && missing_from_code.is_empty(),
        "the catalogue and the code disagree.\n\
         in the code, absent from events-v1.md: {missing_from_document:?}\n\
         in events-v1.md, absent from the code: {missing_from_code:?}"
    );
    // Both sides non-empty, so a parsing bug on either side cannot pass this test
    // by making both sets empty.
    assert!(documented.len() > 20, "parsed {} rows", documented.len());
}

#[test]
fn the_counted_number_of_types_matches_the_table() {
    let document = read("docs/contracts/events-v1.md");
    let rows = catalogue(&document).len();
    let expected = spelled(rows);
    let sentence = document
        .lines()
        .find(|line| line.contains("types have producers"))
        .unwrap_or_else(|| panic!("no sentence about the number of types in events-v1.md"));
    // The document's own style is inconsistent about the hyphen: some revisions
    // write "thirty-three", others "thirty three". The number is the fact this
    // test defends, not the punctuation, so both spellings are accepted.
    let unpunctuated = expected.replace('-', " ");
    assert!(
        sentence.contains(&expected) || sentence.contains(&unpunctuated),
        "the prose says {:?} but the table has {rows} rows (which spells {expected:?})",
        sentence.trim()
    );
}

#[test]
fn the_corpus_counts_in_the_docs_index_match_the_files() {
    let document = read("docs/README.md");
    let count = |directory: &str, extension: &str| -> usize {
        let path = root().join(directory);
        fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("cannot list {}: {error}", path.display()))
            .flatten()
            .filter(|entry| {
                entry.path().is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|found| found == extension)
            })
            .count()
    };
    let events = count("tests/conformance/cases/events", "json");
    let http = count("tests/conformance/cases/http", "json");
    let ws = count("tests/conformance/cases/ws", "json");

    // Two spacings are acceptable, because prose inserts one only when the label
    // starts with a Latin word ("34 个 HTTP 用例" but "34 个事件用例").
    for (label, actual) in [("事件用例", events), ("HTTP 用例", http), ("WS 用例", ws)] {
        let tight = format!("{actual} 个{label}");
        let loose = format!("{actual} 个 {label}");
        assert!(
            document.contains(&tight) || document.contains(&loose),
            "docs/README.md does not say {tight:?} (or {loose:?}); the corpus has {actual} {label}"
        );
    }
}
