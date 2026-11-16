//! The conformance suite: the language-neutral half of the port's acceptance.
//!
//! These cases were written against the Go implementation and are not
//! rewritten here — a case file that changes to fit a new reader proves
//! nothing. Each file names what it feeds the reader, what the reader must
//! answer, and (for accepted logs) the exact canonical bytes to reproduce.
//!
//! Files live in `tests/conformance/cases/events/`, copied verbatim from
//! `forge-v2/tests/conformance/cases/events/`. See docs/08-rust-port.md §5.

use forge_core::{event::parse_log, json::encode, ErrorKind};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    description: String,
    lines: Vec<serde_json::Value>,
    expect: String,
    #[serde(default, rename = "errorContains")]
    error_contains: String,
    #[serde(default)]
    canonical: Vec<serde_json::Value>,
}

fn case_dir() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is crates/core; the oracle sits at the workspace root
    // so both the Go and the Rust tree can read the same bytes.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("conformance")
        .join("cases")
        .join("events")
}

fn cases() -> Vec<Case> {
    let dir = case_dir();
    let mut out = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let case: Case = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()));
        out.push(case);
    }
    out
}

fn log_text(case: &Case) -> String {
    // A case line is either an object (a record, to be encoded) or a string
    // (literal bytes, verbatim). The string form is how a case can express a
    // torn write, a blank line or a syntax error that no encoder would produce.
    let mut text = String::new();
    for line in &case.lines {
        match line {
            serde_json::Value::String(raw) => text.push_str(raw),
            other => text.push_str(&serde_json::to_string(other).expect("case line serializes")),
        }
        text.push('\n');
    }
    text
}

#[test]
fn every_case_file_is_present() {
    // 21 cases in the Go suite: 9 accepted, 12 refused. A file that silently
    // disappears would otherwise show up as "everything passes".
    let found = cases();
    assert_eq!(
        found.len(),
        21,
        "expected 21 conformance cases, found {}: {:?}",
        found.len(),
        found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn accepted_logs_parse_and_reproduce_their_canonical_bytes() {
    for case in cases().into_iter().filter(|c| c.expect == "ok") {
        let text = log_text(&case);
        let events = parse_log(&text).unwrap_or_else(|e| {
            panic!("{}: expected the log to be accepted, got: {e}\n{}", case.name, case.description)
        });
        assert!(!events.is_empty(), "{}: the log parsed to nothing", case.name);
        if case.canonical.is_empty() {
            // The case promises acceptance only, and says so in its own
            // description (an envelope member this build does not model is
            // dropped on re-encode). Assert the property that is still true
            // and still worth having: encoding is a fixed point.
            for event in &events {
                let once = event.canonical().expect("re-encoding works");
                let twice = parse_log(&format!("{once}\n"))
                    .unwrap_or_else(|e| {
                        panic!("{}: re-encoded record no longer parses: {e}", case.name)
                    })
                    .remove(0)
                    .canonical()
                    .expect("re-encoding works");
                assert_eq!(once, twice, "{}: encoding is not a fixed point", case.name);
            }
            continue;
        }
        assert_eq!(
            events.len(),
            case.canonical.len(),
            "{}: parsed {} records, the case expects {}",
            case.name,
            events.len(),
            case.canonical.len()
        );
        for (got, want) in events.iter().zip(case.canonical.iter()) {
            let got_bytes = got.canonical().expect("re-encoding works");
            let want_bytes = encode(want);
            assert_eq!(
                got_bytes, want_bytes,
                "{}: canonical bytes differ\n  case: {}\n  got:  {}\n  want: {}",
                case.name, case.description, got_bytes, want_bytes
            );
        }
    }
}

#[test]
fn refused_logs_are_refused_for_the_documented_reason() {
    for case in cases().into_iter().filter(|c| c.expect == "error") {
        let text = log_text(&case);
        let err = parse_log(&text).unwrap_err_or_else(&case);
        let message = err.to_string();
        assert!(
            message.contains(&case.error_contains),
            "{}: expected an error containing {:?}, got {:?}\n  case: {}",
            case.name,
            case.error_contains,
            message,
            case.description
        );
        // A refusal must be typed, not just worded: the CLI turns the kind into
        // an exit code and the HTTP layer into a status.
        assert_ne!(
            err.kind(),
            ErrorKind::Io,
            "{}: a bad record is not an I/O failure",
            case.name
        );
    }
}

/// Small helper so a test that panics on `Ok` can name the case it was on.
trait UnwrapErrOrElse<E> {
    fn unwrap_err_or_else(self, case: &Case) -> E;
}

impl<E> UnwrapErrOrElse<E> for Result<Vec<forge_core::Event>, E> {
    fn unwrap_err_or_else(self, case: &Case) -> E {
        match self {
            Ok(events) => panic!(
                "{}: expected the log to be refused, but it parsed as {} records\n  case: {}",
                case.name,
                events.len(),
                case.description
            ),
            Err(err) => err,
        }
    }
}
