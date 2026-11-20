//! The files the model handed to the operator, read back as a wire value.
//!
//! Layer L3. `present` (`crates/tool/src/builtin/present.rs`) does not copy a
//! file and does not put it in the conversation: it appends one JSON line to
//! `<dataDir>/presented.jsonl`, and the operator's interface is what turns that
//! line into "open this file". Nothing read that manifest before this module
//! existed, so the tool's whole purpose was invisible. This is the read side:
//! `GET /api/presents` answers on it, on a box that has never run a turn.
//!
//! The line shape belongs to the tool and is authoritative here:
//! `{"ts": <unix millis>, "path": <string>, "size": <bytes>, "description": <string>}`.
//! Only `<dataDir>/presented.jsonl` is served. The
//! `<workspace>/.hearth/presented.jsonl` fallback the tool writes when no data
//! directory is configured is a different path owned by a different directory,
//! so answering from it would report handoffs the configured data directory
//! never saw — and merge two operators' lists into one.
//!
//! Four rules for what belongs here:
//!
//! 1. **Read, do not create.** The manifest is opened for reading and nothing
//!    else: no `create`, no `append`, no `create_dir_all`, no lock. A missing
//!    manifest is an empty list, not a file to open for writing. If a `GET`
//!    created the manifest, "has anything been presented here" would become
//!    unanswerable: the first question would manufacture its own answer.
//! 2. **Newest first.** The manifest is appended, so the newest handoff is the
//!    last line. The answer reverses it: an operator opening this list wants
//!    the file the model just produced at the top, not 200 entries deep.
//! 3. **A bad line is data.** A line that is not a JSON object, or whose `path`
//!    is missing or empty, is skipped and counted in `skipped`; a member of the
//!    wrong type, or a missing `ts`/`size`/`description`, degrades to a default.
//!    `present` appends with `OpenOptions::append`, so a crash mid-write can
//!    leave one torn line; one torn byte must never hide the other nine
//!    handoffs, and it is never a `500`.
//! 4. **Bounded.** At most [`MAX_BYTES`] of manifest is read and at most
//!    [`MAX_ENTRIES`] entries are returned, and when either limit cuts the
//!    answer short `truncated` says so — with a `200`, because a partial list
//!    is a useful answer. The read is allocated from the cap, never from the
//!    file's own size, so an enormous manifest cannot become an enormous
//!    allocation.
//!
//! What it cannot say, it does not guess: an entry this build cannot read is
//! counted, not invented, and a manifest that is not there is an empty list
//! rather than an error, because "nothing has been presented" is a fact.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The manifest `present` appends to, one JSON object per line, in the data
/// directory. Spelled once here so the route and the tool cannot drift apart.
pub const MANIFEST_NAME: &str = "presented.jsonl";

/// How many entries one answer carries. A list of handoffs is a glance, not a
/// search index: the newest 200 are what an operator can act on, and the older
/// lines are still in the manifest for whoever asks next.
pub const MAX_ENTRIES: usize = 200;

/// How many manifest bytes one answer reads. 8 MiB is the cap `present` already
/// puts on a single handed-over file, so a manifest written by the tool stays
/// within what one request is willing to read; a larger one is read from its
/// tail, where the newest lines are, and reported `truncated`.
pub const MAX_BYTES: u64 = 8 << 20;

/// One handoff, as the browser sees it. The member names are the manifest's own
/// words: all four are single words, so there is no camelCase to add.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// When the file was handed over, in unix milliseconds. `0` when the line
    /// has no usable stamp (missing, negative, or not an integer): a stamp the
    /// reader cannot read is not a reason to drop a handoff the operator can
    /// still open.
    #[serde(default)]
    pub ts: u64,
    /// The absolute path `present` recorded. Never empty: a line without a
    /// usable path is counted in `skipped`, because an entry the interface
    /// cannot open is not an entry.
    #[serde(default)]
    pub path: String,
    /// The size the tool recorded, in bytes; `0` when missing or negative. This
    /// is what the file measured when it was presented, not what it measures
    /// now: re-`stat`ing it would make the read depend on the workspace, and a
    /// deleted file would silently vanish from the list instead of showing that
    /// it was handed over.
    #[serde(default)]
    pub size: u64,
    /// The one line the model wrote about the file; `""` when it wrote none.
    #[serde(default)]
    pub description: String,
}

/// The answer: `{"presents":[...],"skipped":0,"truncated":false}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Newest first: the last line of the manifest comes first, because the
    /// manifest is appended and the operator wants what the model just made.
    #[serde(default)]
    pub presents: Vec<Entry>,
    /// Lines that were read and could not become an entry: not JSON, not an
    /// object, or no usable `path`. Counted, never fatal — a torn line is the
    /// manifest's data, not the route's error.
    #[serde(default)]
    pub skipped: u64,
    /// `true` when [`MAX_BYTES`] or [`MAX_ENTRIES`] cut the answer short. The
    /// route still answers `200`: a partial list plus this flag is more useful
    /// than an error that shows nothing.
    #[serde(default)]
    pub truncated: bool,
    /// Why this answer is not the whole truth, when `truncated` alone does not
    /// say it: the manifest could not be read at all, is not a regular file, or
    /// was cut by [`MAX_BYTES`] rather than by the entry count. Absent
    /// otherwise, so the ordinary answer is exactly
    /// `{"presents":[],"skipped":0,"truncated":false}` — and a client that
    /// ignores this member loses a diagnostic, not data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Report {
    /// Read `<data_dir>/presented.jsonl` and answer.
    ///
    /// Never fails: every reason the manifest cannot be read is a report with a
    /// `note` (or, for a manifest that simply does not exist, the plain empty
    /// answer). The caller owns the directory — `server.rs` builds it from
    /// config — and this function opens the manifest for reading only, so it
    /// creates neither the file nor its directory.
    pub fn read(data_dir: &Path) -> Report {
        let manifest = data_dir.join(MANIFEST_NAME);
        let metadata = match fs::metadata(&manifest) {
            Ok(metadata) => metadata,
            // No manifest means nothing has been presented yet. That is an
            // answer, not an error, and in particular not a reason to create
            // the file: a read that manufactures state answers its own
            // question.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Report::empty(),
            Err(error) => return Report::unreadable(&error),
        };
        if !metadata.is_file() {
            return Report {
                note: Some(
                    "the presented manifest is not a regular file; nothing is shown".to_string(),
                ),
                ..Report::empty()
            };
        }
        match read_window(&manifest) {
            Ok((text, cut)) => Report::parse(&text, cut),
            Err(error) => Report::unreadable(&error),
        }
    }

    /// The answer for "there is nothing to report".
    fn empty() -> Report {
        Report {
            presents: Vec::new(),
            skipped: 0,
            truncated: false,
            note: None,
        }
    }

    /// The answer for a manifest that exists but could not be read. The list is
    /// empty because nothing was read; `note` says why, since a silently empty
    /// list would read as "nothing has been presented", which is a different
    /// and possibly false claim.
    fn unreadable(error: &io::Error) -> Report {
        Report {
            note: Some(format!(
                "the presented manifest could not be read ({}); nothing is shown",
                kind_word(error.kind())
            )),
            ..Report::empty()
        }
    }

    /// Turn the bytes that were read into the answer.
    ///
    /// `cut` is whether [`read_window`] dropped part of the manifest; it only
    /// seeds `truncated`, because the entry cap can cut the answer too.
    fn parse(text: &str, cut: bool) -> Report {
        let mut presents = Vec::new();
        let mut skipped = 0u64;
        for line in text.lines() {
            match entry_in(line) {
                Some(entry) => presents.push(entry),
                // A blank line counts too: it is a line that is not a JSON
                // object. `present` never writes one, so a blank line in the
                // manifest is evidence of damage, and hiding it would make
                // `skipped` understate what the reader had to pass over.
                None => skipped += 1,
            }
        }

        // The manifest is append-only, so the newest handoff is the last line
        // read. The entries are dropped from the **front** — the oldest — not
        // truncated at the back: under the cap the list must keep the newest
        // handoffs, which is the end of an append-only file. Then it is
        // reversed for the answer, because newest comes first.
        let mut truncated = cut;
        if presents.len() > MAX_ENTRIES {
            let excess = presents.len() - MAX_ENTRIES;
            presents.drain(..excess);
            truncated = true;
        }
        presents.reverse();

        let note = if cut {
            // `truncated` says "this is not everything"; a client that shows
            // only that cannot tell whether the oldest handoffs are missing or
            // the newest were capped, and those need different wording.
            Some(format!(
                "the manifest is larger than {MAX_BYTES} bytes; only its newest {MAX_BYTES} bytes were read, so older handoffs are missing from this answer"
            ))
        } else {
            None
        };

        Report {
            presents,
            skipped,
            truncated,
            note,
        }
    }
}

/// One manifest line as an [`Entry`], or `None` when it cannot become one.
///
/// An unreadable line is data the reader does not understand, not a reason to
/// refuse the file (AGENTS.md rule 2), and never a panic: every member is
/// fetched with a fallible accessor and defaults rather than unwrapped.
fn entry_in(line: &str) -> Option<Entry> {
    let line = line.trim();
    let Value::Object(record) = serde_json::from_str::<Value>(line).ok()? else {
        // Not JSON at all, or JSON that is not an object: an array or a bare
        // number is not a handoff and has no path to offer.
        return None;
    };
    // `path` is the whole point of the line: without one, the interface has
    // nothing to open, so the line is counted as skipped instead of being
    // returned as an entry with an empty path the UI would have to special-case.
    let path = record.get("path").and_then(Value::as_str)?;
    if path.trim().is_empty() {
        return None;
    }
    Some(Entry {
        // `as_u64` is `None` for a negative number and for a float, and a
        // negative millisecond stamp is not a time any clock produced; `0` is
        // the honest "no stamp" for all three cases.
        ts: record.get("ts").and_then(Value::as_u64).unwrap_or(0),
        path: path.to_string(),
        size: record.get("size").and_then(Value::as_u64).unwrap_or(0),
        description: record
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// The readable text of the manifest, bounded by [`MAX_BYTES`], plus whether
/// part of the file was left unread.
///
/// The window is the **tail**, not the head: the newest handoffs live at the
/// end of an append-only file, so a tail window is what keeps "newest first"
/// true when the manifest is too large to read whole. Two partial lines are
/// then dropped — the one the window starts in the middle of and the one it
/// ends in the middle of — because a half line is not a record: counting it as
/// skipped would blame the data for a cut this function made, and `truncated`
/// already reports the cut.
fn read_window(manifest: &Path) -> io::Result<(String, bool)> {
    let mut file = fs::File::open(manifest)?;
    let len = file.metadata()?.len();
    let cut = len > MAX_BYTES;
    if cut {
        file.seek(SeekFrom::Start(len - MAX_BYTES))?;
    }
    // Allocated from the cap, never from `len`: the point of the bound is that
    // a manifest of any size costs one bounded buffer, so a runaway or
    // hand-edited file cannot turn a `GET` into an allocation attack.
    let mut buffer = Vec::with_capacity(len.min(MAX_BYTES) as usize);
    let mut window = file.take(MAX_BYTES);
    window.read_to_end(&mut buffer)?;

    let start = if cut {
        buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1)
    } else {
        0
    };
    let end = buffer
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(start, |index| index + 1)
        .max(start);
    // Lossy on purpose: a torn multi-byte character is one skipped line, not a
    // failed read. The replacement character survives to the JSON parser, which
    // rejects that line like any other damage.
    let bytes = buffer.get(start..end).unwrap_or(&[]);
    Ok((String::from_utf8_lossy(bytes).into_owned(), cut))
}

/// An [`io::ErrorKind`] in the words a client can show. `ErrorKind`'s own
/// `Display` is English too, but naming the handful that matter keeps the note
/// stable across Rust versions and free of the OS's localized text.
fn kind_word(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::NotFound => "not found",
        io::ErrorKind::PermissionDenied => "permission denied",
        io::ErrorKind::InvalidData => "invalid data",
        io::ErrorKind::UnexpectedEof => "unexpected end of file",
        io::ErrorKind::TimedOut => "timed out",
        io::ErrorKind::OutOfMemory => "out of memory",
        _ => "an I/O error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module ships its own unit tests for the parse-only paths, which need
    /// no filesystem; the file-side behaviour (create-nothing, bounds) is
    /// covered by `crates/server/tests/present_route.rs` against real temp
    /// directories.
    #[test]
    fn a_full_line_parses_into_the_documented_fields() {
        let entry = entry_in(
            r#"{"ts":1732000000000,"path":"reports/q3.xlsx","size":20480,"description":"the numbers you asked for"}"#,
        )
        .expect("one entry");
        assert_eq!(entry.ts, 1_732_000_000_000);
        assert_eq!(entry.path, "reports/q3.xlsx");
        assert_eq!(entry.size, 20_480);
        assert_eq!(entry.description, "the numbers you asked for");
    }

    #[test]
    fn damage_is_skipped_and_defaults_are_not_damage() {
        assert_eq!(entry_in(""), None);
        assert_eq!(entry_in("not json"), None);
        assert_eq!(entry_in(r#"["path"]"#), None);
        assert_eq!(entry_in(r#"{"ts":1,"size":2}"#), None);
        assert_eq!(entry_in(r#"{"path":"   "}"#), None);

        let sparse = entry_in(r#"{"path":"a.txt"}"#).expect("path is enough");
        assert_eq!(sparse.ts, 0);
        assert_eq!(sparse.size, 0);
        assert_eq!(sparse.description, "");

        let wrong = entry_in(r#"{"ts":-1,"path":"a.txt","size":-5,"description":7}"#)
            .expect("a wrong-typed member is not a bad line");
        assert_eq!(wrong.ts, 0);
        assert_eq!(wrong.size, 0);
        assert_eq!(wrong.description, "");
    }

    #[test]
    fn the_empty_report_encodes_without_a_note() {
        assert_eq!(
            serde_json::to_string(&Report::empty()).expect("encode"),
            r#"{"presents":[],"skipped":0,"truncated":false}"#
        );
    }
}
