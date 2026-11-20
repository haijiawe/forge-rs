//! One mailbox per worker, append-only, drained through a per-worker cursor.
//!
//! Layout: `<root>/inbox/<worker>.jsonl`, one JSON message per line. Append-only
//! in the same sense the session log is: a sender never rewrites, and a receiver
//! never deletes. That gives two properties a "pop from a queue" file cannot:
//!
//! * A message sent before the worker starts is still there when it starts. The
//!   mailbox is a file the worker owns, not a channel that only exists while its
//!   reader does — so a plan can mail a worker its brief up front, and a worker
//!   that crashed can be restarted without its inbox having been consumed by
//!   nobody.
//! * A drain is a read plus an advance of a cursor that is kept in memory and
//!   also returned to the caller. If the worker dies before using the messages,
//!   the next drain with the old cursor sees them again. At-least-once, never
//!   at-most-once: losing a message is silent, repeating one is visible.
//!
//! The torn-tail rule is the same one `hearth_session::Store` uses, for the same
//! reason: a writer can die between the bytes of a line and its newline. A line
//! without its terminating newline was never committed, so it is skipped rather
//! than parsed, and it must not make the rest of the file unreadable. A line
//! that *is* newline-terminated but is not valid JSON is different — that is a
//! corrupt committed record, and it is counted rather than skipped silently,
//! because a writer producing garbage is a fact the caller should see.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hearth_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};

use crate::{lock, now_millis, valid_name};

/// Directory under the team root holding every worker's inbox.
pub const INBOX_DIR: &str = "inbox";

/// One message. `from` may be empty for a message written by the operator; `to`
/// is always the inbox's worker, so a message copied into the wrong file is
/// detectable by comparing the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    pub from: String,
    pub to: String,
    pub text: String,
    /// Epoch milliseconds, or `0` when the clock could not be read. A message
    /// without a usable timestamp is still a message.
    pub ts: i64,
}

/// What one drain returned.
///
/// The cursor is explicit rather than hidden: two drains of the same mailbox
/// from two places should not each see half the messages, and a caller that
/// wants to replay after a crash needs the number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drain {
    pub messages: Vec<Message>,
    /// The byte offset to pass to the next drain.
    pub cursor: u64,
    /// Complete lines that were not valid JSON, and were therefore not
    /// delivered. Counted, not thrown: a corrupt line is a fact about the file.
    pub skipped: usize,
    /// A final line without its newline: a torn write, skipped by design.
    pub torn_tail: bool,
}

type WorkerLocks = Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>;
type Cursors = Arc<Mutex<BTreeMap<String, u64>>>;

/// The mailbox directory for one team root.
///
/// Cloning is cheap and shares the same cursors and locks, so a dispatcher can
/// hand a clone to each worker thread without coordinating reads.
#[derive(Debug, Clone)]
pub struct Mailbox {
    root: PathBuf,
    locks: WorkerLocks,
    cursors: Cursors,
}

impl Mailbox {
    /// Open (or create) `<root>/inbox`.
    pub fn open(root: impl AsRef<Path>) -> Result<Mailbox> {
        let root = root.as_ref().to_path_buf();
        let inbox = root.join(INBOX_DIR);
        std::fs::create_dir_all(&inbox).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("team: cannot create inbox dir {}: {error}", inbox.display()),
            )
        })?;
        Ok(Mailbox {
            root,
            locks: Arc::new(Mutex::new(BTreeMap::new())),
            cursors: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// The team root this mailbox lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/inbox/<worker>.jsonl`.
    pub fn path(&self, worker: &str) -> PathBuf {
        self.root.join(INBOX_DIR).join(format!("{worker}.jsonl"))
    }

    /// Append one message for `to`.
    ///
    /// The append is one `write_all` of one complete line: a sender cannot
    /// produce a torn line by itself, only by dying mid-write. A missing inbox
    /// file is created, which is why a message to a worker that has not started
    /// yet is not an error.
    pub fn send(&self, from: &str, to: &str, text: &str) -> Result<Message> {
        if !from.is_empty() && !valid_name(from) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: sender name {from:?} is not a usable worker name"),
            ));
        }
        if !valid_name(to) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: recipient {to:?} is not a usable worker name"),
            ));
        }
        if text.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: refusing to send an empty message to {to:?}"),
            ));
        }
        let message = Message {
            from: from.to_string(),
            to: to.to_string(),
            text: text.to_string(),
            ts: now_millis(),
        };
        let mut line = serde_json::to_string(&message).map_err(|error| {
            Error::new(
                ErrorKind::Json,
                format!("team: cannot encode a message: {error}"),
            )
        })?;
        line.push('\n');

        let guard = self.worker_lock(to);
        let _held = guard.lock().unwrap_or_else(|poison| poison.into_inner());
        let path = self.path(to);
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
                    format!("team: cannot open inbox {}: {error}", path.display()),
                )
            })?;
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(message)
    }

    /// Deliver every complete message after `cursor`.
    ///
    /// The returned cursor points past the last *complete* line read, so a torn
    /// tail is never marked consumed: when the writer finishes that line, the
    /// next drain from the same cursor picks it up.
    pub fn drain_from(&self, worker: &str, cursor: u64) -> Result<Drain> {
        if !valid_name(worker) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: {worker:?} is not a usable worker name"),
            ));
        }
        let guard = self.worker_lock(worker);
        let _held = guard.lock().unwrap_or_else(|poison| poison.into_inner());
        let path = self.path(worker);
        let mut file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A worker that has never been sent anything has an empty
                // inbox, not an error. Nothing running has to exist for that.
                return Ok(Drain {
                    cursor,
                    ..Drain::default()
                });
            }
            Err(error) => {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("team: cannot open inbox {}: {error}", path.display()),
                ))
            }
        };
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        let start = usize::try_from(cursor)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let mut out = Drain {
            cursor,
            ..Drain::default()
        };
        let mut position = start;
        while position < data.len() {
            let Some(relative) = data[position..].iter().position(|&byte| byte == b'\n') else {
                // No newline left: the final line was never committed. Leave the
                // cursor before it so the bytes are re-read once the writer
                // commits, and say so instead of pretending the inbox ended
                // cleanly.
                out.torn_tail = true;
                break;
            };
            let end = position + relative;
            let line = &data[position..end];
            position = end + 1;
            out.cursor = position as u64;
            if line.iter().all(|byte| byte.is_ascii_whitespace()) {
                continue;
            }
            let text = match std::str::from_utf8(line) {
                Ok(text) => text,
                Err(_) => {
                    // A committed line that is not UTF-8 cannot be a message
                    // this crate wrote; count it and keep the rest readable.
                    out.skipped += 1;
                    continue;
                }
            };
            match serde_json::from_str::<Message>(text.trim()) {
                Ok(message) => out.messages.push(message),
                Err(_) => out.skipped += 1,
            }
        }
        Ok(out)
    }

    /// Deliver every message not yet drained by *this mailbox's cursor*.
    ///
    /// The cursor is in-memory and shared by clones, so the common case ("read
    /// my mail") needs no bookkeeping from the caller; a caller that needs
    /// replay uses [`Mailbox::drain_from`] with the cursor it kept.
    pub fn drain(&self, worker: &str) -> Result<Vec<Message>> {
        let cursor = lock(&self.cursors).get(worker).copied().unwrap_or(0);
        let drained = self.drain_from(worker, cursor)?;
        lock(&self.cursors).insert(worker.to_string(), drained.cursor);
        Ok(drained.messages)
    }

    fn worker_lock(&self, worker: &str) -> Arc<Mutex<()>> {
        let mut locks = lock(&self.locks);
        locks
            .entry(worker.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}
