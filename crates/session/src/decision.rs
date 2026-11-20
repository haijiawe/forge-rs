//! Watching a log for the answer to one approval request.
//!
//! The approval request and its decision travel through the same append-only
//! log as everything else. A caller (the CLI or the HTTP layer) knows the call
//! id it is waiting for, so this watcher only has to do one thing well: read
//! forward without losing a line and without reporting the same decision twice.
//!
//! The dangerous case is a decision line that is still being written. It must
//! be re-read on the next poll, and it must be re-read *only* — a watcher that
//! both buffers the partial bytes and rewinds the file offset applies the same
//! decision twice. The rule here is therefore simple: the file offset advances
//! only past complete lines, and no bytes are kept in memory.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use hearth_core::{Error, ErrorKind, Result};
use serde::Deserialize;
use serde_json::Value;

/// The part of an `approval/decision` this watcher exposes. The decision is
/// left as the writer's word ("allow", "deny", ...): the watcher's contract is
/// identity, not policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub call_id: String,
    pub decision: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DecisionPayload {
    #[serde(rename = "callId")]
    call_id_camel: String,
    id: String,
    decision: String,
}

impl DecisionPayload {
    fn call_id(&self) -> String {
        if !self.call_id_camel.is_empty() {
            return self.call_id_camel.clone();
        }
        // `id` is what the original writers put on a decision. It is accepted
        // only as a fallback: a decision that names its call is unambiguous,
        // while an `id` could in principle be the approval's own id.
        self.id.clone()
    }
}

/// Polls one log for an `approval/decision` carrying a given call id.
///
/// `new` does not touch the filesystem: a session may legitimately have no log
/// yet when the watcher is constructed. The first `poll` opens the file at
/// offset zero and reads forward.
pub struct DecisionWatcher {
    path: PathBuf,
    offset: u64,
    closed: bool,
}

impl DecisionWatcher {
    pub fn new(path: impl AsRef<Path>) -> DecisionWatcher {
        DecisionWatcher {
            path: path.as_ref().to_path_buf(),
            offset: 0,
            closed: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Look for the first decision addressed to `call_id`.
    ///
    /// Decisions for other calls are consumed: they are answered and this
    /// watcher will never be asked about them again. A file that does not
    /// exist yet is not an error; it just has nothing to report. A torn final
    /// line stays at `offset` and is retried by the next poll.
    pub fn poll(&mut self, call_id: impl AsRef<str>) -> Result<Option<String>> {
        Ok(self.poll_full(call_id)?.map(|decision| decision.decision))
    }

    /// Same scan as [`poll`](Self::poll), but keeps the call id that the
    /// decision named. `poll` is the common case; this exists for callers that
    /// need to log which alias the writer used.
    pub fn poll_full(&mut self, call_id: impl AsRef<str>) -> Result<Option<Decision>> {
        let call_id = call_id.as_ref();
        if self.closed {
            return Err(Error::new(
                ErrorKind::Conflict,
                "session: decision watcher is closed",
            ));
        }
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            // The log was truncated or replaced underneath us. Starting over
            // is the only way to see the current contents; a stale offset would
            // read past EOF forever.
            self.offset = 0;
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        let mut pos = 0usize;
        let mut found = None;
        while let Some(relative) = data[pos..].iter().position(|&b| b == b'\n') {
            let end = pos + relative + 1;
            let line = &data[pos..end - 1];
            pos = end;
            // A complete line is consumed even when it turns out to be
            // malformed: the bytes cannot change later, and leaving the offset
            // behind would report the same parse error on every poll.
            self.offset += (relative + 1) as u64;
            let Ok(text) = std::str::from_utf8(line) else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let tree: Value = match serde_json::from_str(text) {
                Ok(tree) => tree,
                // A writer that is mid-line can leave nonsense for one poll.
                // The line is complete, though, so it cannot become valid
                // later; skipping it matches the projection's tolerance for
                // unknown data while keeping the offset moving.
                Err(_) => continue,
            };
            if tree.get("type").and_then(Value::as_str) != Some("approval/decision") {
                continue;
            }
            let payload: DecisionPayload = match tree.get("payload") {
                // An absent payload is an event with nothing to decode, the
                // same way core's `DecodePayload` treats an empty payload.
                None | Some(Value::Null) => DecisionPayload::default(),
                Some(value) => serde_json::from_value(value.clone()).map_err(|e| {
                    Error::new(
                        ErrorKind::Invalid,
                        format!("session: decision: malformed approval/decision payload: {e}"),
                    )
                    .with_source(e)
                })?,
            };
            let decision = Decision {
                call_id: payload.call_id(),
                decision: payload.decision,
            };
            // An empty decision word is not an answer; keep scanning. The
            // vocabulary belongs to the contract, but an absent word does not
            // mean "allow".
            if decision.call_id == call_id && !decision.decision.is_empty() {
                found = Some(decision);
                break;
            }
        }
        Ok(found)
    }

    /// Stop polling. A closed watcher refuses further polls rather than
    /// silently reporting "no decision", which would look like a timeout.
    pub fn close(&mut self) {
        self.closed = true;
    }
}
