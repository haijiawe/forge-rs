//! Per-Lead transaction order, appended beside the board.
//!
//! A team root holds one shared board (compare-and-set, see [`crate::board`])
//! and, per Lead, one append-only journal. They answer two different questions,
//! and neither replaces the other:
//!
//! * the board's CAS decides **who wins** when two writers touch the same task
//!   at once — a stale `expected_revision` is refused (`conflict`), so a lost
//!   update cannot overwrite a won one;
//! * the journal records **the order in which one Lead's operations happened**.
//!   Two operations by the same Lead can both be valid and still have an order
//!   that matters; the CAS has nothing to say about that order, because the
//!   second write does not conflict with the first.
//!
//! Confusing the two is the failure this module exists to prevent. Swapping the
//! journal for the board's CAS would let a Lead's later operation be reordered
//! ahead of an earlier one — both "succeed", and the record of what happened is
//! wrong. Swapping CAS for the journal would let two workers overwrite each
//! other's task claim as long as the entries are merely numbered in order.
//! **CAS prevents concurrent writes; the journal fixes order. They do not
//! substitute for each other.**
//!
//! # Layout
//!
//! `<root>/journal/<lead>.jsonl`, one JSON [`Entry`] per line, append-only, one
//! file per Lead. A journal is a team file, not a session log: no session event
//! is written here, and `hearth-session` never opens it.
//!
//! # Ordering guarantee
//!
//! Within one process, every operation of a single Lead passes through one
//! per-Lead lock, so `seq` is strictly increasing and reflects call order. A
//! reopen continues from the highest `seq` already on disk. Across processes the
//! append is not arbitrated — the board makes the same promise — so two
//! processes recording for one Lead at the same instant can pick the same `seq`.
//! That is a real limitation, stated rather than hidden.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hearth_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};

use crate::{lock, now_millis, valid_name};

/// Directory under the team root holding every Lead's journal.
pub const JOURNAL_DIR: &str = "journal";

/// One recorded operation by one Lead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    /// The Lead this operation belongs to; equals the journal's file stem.
    pub lead: String,
    /// Strictly increasing per Lead, starting at 1.
    pub seq: u64,
    /// A short verb such as `"claim"`, `"complete"` or `"reassign"`.
    pub op: String,
    /// Free-form context for a human. Empty when the operation needs none.
    #[serde(default)]
    pub detail: String,
    /// Epoch milliseconds, or `0` when the clock could not be read.
    pub ts: i64,
}

/// What one read of a journal returned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalRead {
    pub entries: Vec<Entry>,
    /// Complete lines that were not valid entries, and were therefore not
    /// returned. Counted, not thrown: a corrupt line is a fact about the file.
    pub skipped: usize,
    /// A final line without its newline: a torn write, skipped by design and
    /// re-read once the writer commits it.
    pub torn_tail: bool,
}

type LeadLocks = Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>;

/// The journal directory for one team root.
///
/// Cloning is cheap and shares the same per-Lead locks, so a dispatcher can hand
/// a clone to each Lead's thread without coordinating writes.
#[derive(Debug, Clone)]
pub struct Journal {
    root: PathBuf,
    locks: LeadLocks,
}

impl Journal {
    /// Open (or create) `<root>/journal`.
    pub fn open(root: impl AsRef<Path>) -> Result<Journal> {
        let root = root.as_ref().to_path_buf();
        let dir = root.join(JOURNAL_DIR);
        std::fs::create_dir_all(&dir).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("team: cannot create journal dir {}: {error}", dir.display()),
            )
        })?;
        Ok(Journal {
            root,
            locks: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// The team root this journal lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The leads that have journalled anything, sorted.
    ///
    /// The journal has no index file on purpose (one file per lead means one
    /// lock per lead), so the directory listing is the index. A missing
    /// directory is an empty journal, not an error: a board written by a run
    /// that never journalled is still a board.
    pub fn leads(&self) -> Result<Vec<String>> {
        let dir = self.root.join(JOURNAL_DIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("team: cannot read journal dir {}: {error}", dir.display()),
                ))
            }
        };
        let mut leads = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                leads.push(stem.to_string());
            }
        }
        leads.sort();
        Ok(leads)
    }

    /// `<root>/journal/<lead>.jsonl`.
    pub fn path(&self, lead: &str) -> PathBuf {
        self.root.join(JOURNAL_DIR).join(format!("{lead}.jsonl"))
    }

    /// Record one operation for `lead` as its own transaction.
    ///
    /// Equivalent to a one-entry [`Journal::transact`]; use `transact` when
    /// several entries must land together or not at all.
    pub fn record(&self, lead: &str, op: &str, detail: &str) -> Result<Entry> {
        self.transact(lead, |txn| txn.record(op, detail))
    }

    /// Run `body` as one transaction for `lead`, holding that Lead's lock.
    ///
    /// The per-Lead lock is what serialises this Lead's operations, so `seq` is
    /// assigned and written in call order. Entries staged through
    /// [`LeadTxn::record`] are buffered and written in a single append only if
    /// `body` returns `Ok`; if `body` returns `Err`, nothing is written, so a
    /// failed operation never leaves journal entries claiming it happened.
    ///
    /// `body` must not call back into this journal for the same `lead`: the lock
    /// is held for its whole duration and the lock is not reentrant.
    pub fn transact<T, F>(&self, lead: &str, body: F) -> Result<T>
    where
        F: FnOnce(&mut LeadTxn<'_>) -> Result<T>,
    {
        if !valid_name(lead) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: lead name {lead:?} is not a usable worker name"),
            ));
        }
        let handle = self.lead_lock(lead);
        let _held = handle.lock().unwrap_or_else(|poison| poison.into_inner());
        let mut txn = LeadTxn {
            lead,
            next_seq: self.last_seq_unlocked(lead)? + 1,
            pending: Vec::new(),
        };
        let value = body(&mut txn)?;
        self.append(lead, &txn.pending)?;
        Ok(value)
    }

    /// Every entry recorded for `lead`, in `seq` order.
    pub fn entries(&self, lead: &str) -> Result<Vec<Entry>> {
        Ok(self.read(lead)?.entries)
    }

    /// Read `lead`'s journal, reporting skipped corrupt lines and a torn tail
    /// instead of hiding them.
    pub fn read(&self, lead: &str) -> Result<JournalRead> {
        if !valid_name(lead) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: lead name {lead:?} is not a usable worker name"),
            ));
        }
        let handle = self.lead_lock(lead);
        let _held = handle.lock().unwrap_or_else(|poison| poison.into_inner());
        self.read_entries(lead)
    }

    /// The highest `seq` recorded for `lead`, or `0` when the journal is empty.
    pub fn last_seq(&self, lead: &str) -> Result<u64> {
        if !valid_name(lead) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: lead name {lead:?} is not a usable worker name"),
            ));
        }
        let handle = self.lead_lock(lead);
        let _held = handle.lock().unwrap_or_else(|poison| poison.into_inner());
        self.last_seq_unlocked(lead)
    }

    fn last_seq_unlocked(&self, lead: &str) -> Result<u64> {
        Ok(self
            .read_entries(lead)?
            .entries
            .last()
            .map_or(0, |entry| entry.seq))
    }

    /// Append every staged entry in one `write_all`.
    ///
    /// One concatenated buffer rather than a write per entry: a crash can still
    /// tear the final line, but it cannot leave a transaction half-written
    /// between two complete entries.
    fn append(&self, lead: &str, entries: &[Entry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut buffer = String::new();
        for entry in entries {
            let line = serde_json::to_string(entry).map_err(|error| {
                Error::new(
                    ErrorKind::Json,
                    format!("team: cannot encode a journal entry: {error}"),
                )
            })?;
            buffer.push_str(&line);
            buffer.push('\n');
        }
        let path = self.path(lead);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| {
                Error::new(
                    ErrorKind::Io,
                    format!("team: cannot open journal {}: {error}", path.display()),
                )
            })?;
        file.write_all(buffer.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    /// Read entries from disk. No locking: callers hold the Lead's lock.
    fn read_entries(&self, lead: &str) -> Result<JournalRead> {
        let path = self.path(lead);
        let mut file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A Lead that has never recorded anything has an empty journal,
                // not an error. Nothing has to have run for that.
                return Ok(JournalRead::default());
            }
            Err(error) => {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("team: cannot open journal {}: {error}", path.display()),
                ))
            }
        };
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        let mut out = JournalRead::default();
        let mut position = 0;
        while position < data.len() {
            let Some(relative) = data[position..].iter().position(|&byte| byte == b'\n') else {
                // No newline left: the final line was never committed. Skip it
                // and say so, rather than parse a partial entry.
                out.torn_tail = true;
                break;
            };
            let end = position + relative;
            let line = &data[position..end];
            position = end + 1;
            if line.iter().all(|byte| byte.is_ascii_whitespace()) {
                continue;
            }
            let text = match std::str::from_utf8(line) {
                Ok(text) => text,
                Err(_) => {
                    out.skipped += 1;
                    continue;
                }
            };
            match serde_json::from_str::<Entry>(text.trim()) {
                Ok(entry) => out.entries.push(entry),
                Err(_) => out.skipped += 1,
            }
        }
        Ok(out)
    }

    fn lead_lock(&self, lead: &str) -> Arc<Mutex<()>> {
        let mut locks = lock(&self.locks);
        locks
            .entry(lead.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// The staging area handed to a [`Journal::transact`] body.
///
/// Entries recorded here get their `seq` assigned immediately, in order, but are
/// only written to disk when the transaction body returns `Ok`.
pub struct LeadTxn<'a> {
    lead: &'a str,
    next_seq: u64,
    pending: Vec<Entry>,
}

impl<'a> LeadTxn<'a> {
    /// Stage one operation and return the entry that will be written.
    ///
    /// An empty (or whitespace-only) `op` is refused: a journal line that does
    /// not say what happened is worse than no line.
    pub fn record(&mut self, op: &str, detail: &str) -> Result<Entry> {
        if op.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "team: journal operation name must not be empty".to_string(),
            ));
        }
        let entry = Entry {
            lead: self.lead.to_string(),
            seq: self.next_seq,
            op: op.to_string(),
            detail: detail.to_string(),
            ts: now_millis(),
        };
        self.next_seq += 1;
        self.pending.push(entry.clone());
        Ok(entry)
    }

    /// The `seq` the next [`LeadTxn::record`] will use.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
}
