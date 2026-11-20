//! Authoritative rebuild from the event logs.
//!
//! The index is a cache: every fact in it can be regenerated from
//! `<dataDir>/sessions/*.jsonl`. Rebuild therefore clears the whole index in
//! one transaction and commits only after every log has been folded. A crash
//! leaves the previous index intact — never a half-built one.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

use hearth_core::{Error, ErrorKind, Event, Result};
use hearth_session::Projection;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Transaction};
use serde_json::Value as JsonValue;

use crate::schema::{trigger_statements, META_BYTES_KEY};
use crate::store::{clip_title, err_closed, store_err, Session, Store};

const MAX_SCAN_WORKERS: usize = 8;
const MESSAGE_BATCH_ROWS: usize = 200;

#[derive(Debug, Clone)]
struct MessageRow {
    session_id: String,
    seq: u64,
    turn: u32,
    event_type: String,
    text: String,
}

#[derive(Debug)]
struct Scanned {
    session: Session,
    messages: Vec<MessageRow>,
    bytes: i64,
}

#[derive(Debug)]
enum ScanFailure {
    Unsupported,
    Other(Error),
}

type ScanOutcome = std::result::Result<Option<Scanned>, ScanFailure>;

#[derive(Debug)]
struct ScanJob {
    id: String,
    path: PathBuf,
}

fn valid_session_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 || id.contains("..") {
        return false;
    }
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn field_string(payload: Option<&JsonValue>, names: &[&str]) -> String {
    let Some(object) = payload.and_then(JsonValue::as_object) else {
        return String::new();
    };
    for name in names {
        if let Some(text) = object.get(*name).and_then(JsonValue::as_str) {
            if !text.is_empty() {
                return text.to_owned();
            }
        }
    }
    String::new()
}

fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_default()
}

fn is_abort_like(reason: &str) -> bool {
    matches!(
        reason.trim().to_ascii_lowercase().as_str(),
        "abort"
            | "aborted"
            | "cancel"
            | "cancelled"
            | "canceled"
            | "error"
            | "failed"
            | "timeout"
            | "interrupt"
            | "interrupted"
    )
}

fn line_number(message: &str) -> Option<usize> {
    let rest = message.strip_prefix("line ")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Keep the good prefix of a log. A final line without a newline is a torn
/// append; an unparseable record ends the prefix at that line. A log from a
/// different format generation is refused whole, not partly indexed.
fn good_prefix(text: &str) -> Result<Vec<Event>> {
    let content = if !text.is_empty() && !text.ends_with('\n') {
        match text.rfind('\n') {
            Some(index) => &text[..index],
            None => "",
        }
    } else {
        text
    };
    match hearth_core::event::parse_log(content) {
        Ok(events) => Ok(events),
        Err(error) if error.kind() == ErrorKind::UnsupportedVersion => Err(error),
        Err(error) => {
            let Some(line) = line_number(error.message()) else {
                return Err(error);
            };
            let lines: Vec<&str> = content.lines().collect();
            let keep = line.saturating_sub(1).min(lines.len());
            hearth_core::event::parse_log(&lines[..keep].join("\n"))
        }
    }
}

#[derive(Default)]
struct FoldState {
    created_at: i64,
    updated_at: i64,
    first_ts: i64,
    last_seq_seen: u64,
    workspace: String,
    turn_open: bool,
    running: u32,
    status: String,
    pending: HashSet<String>,
    answered: HashSet<String>,
    title: String,
    title_seen: bool,
    messages: Vec<MessageRow>,
    saw_event: bool,
}

impl FoldState {
    fn apply_extras(&mut self, event: &Event) {
        if event.ts > 0 {
            if self.created_at == 0 || event.ts < self.created_at {
                self.created_at = event.ts;
            }
            if event.ts > self.updated_at {
                self.updated_at = event.ts;
            }
        }
        if self.first_ts == 0 && event.ts > 0 {
            self.first_ts = event.ts;
        }
        if event.seq > self.last_seq_seen {
            self.last_seq_seen = event.seq;
        }
        let payload = event.payload.as_ref();
        match event.typ.as_str() {
            "session/start" => {
                let workspace = field_string(payload, &["workspace", "cwd"]);
                if !workspace.is_empty() {
                    self.workspace = workspace;
                }
            }
            "turn/start" => {
                self.turn_open = true;
                self.running = event.turn;
            }
            "turn/end" => {
                if event.turn == self.running || self.running == 0 {
                    self.turn_open = false;
                    self.running = 0;
                }
            }
            "session/end" => {
                let reason = field_string(payload, &["reason"]);
                self.status = if is_abort_like(&reason) {
                    hearth_protocol::status::ABORTED
                } else {
                    hearth_protocol::status::ENDED
                }
                .to_owned();
            }
            "user/message" => {
                let text = message_text(payload);
                if !self.title_seen {
                    self.title_seen = true;
                    self.title = clip_title(&text);
                }
                self.add_message(event, text);
            }
            "assistant/message" => {
                self.add_message(event, message_text(payload));
            }
            "approval/request" => {
                let id = first_non_empty(&[
                    field_string(payload, &["approvalId"]),
                    field_string(payload, &["id", "approval_id"]),
                ]);
                if !id.is_empty() && !self.answered.contains(&id) {
                    self.pending.insert(id);
                }
            }
            "approval/decision" => {
                let id =
                    first_non_empty(&[field_string(payload, &["approvalId", "id", "approval_id"])]);
                if !id.is_empty() {
                    self.answered.insert(id.clone());
                    self.pending.remove(&id);
                }
            }
            _ => {}
        }
    }

    fn add_message(&mut self, event: &Event, text: String) {
        if text.trim().is_empty() {
            return;
        }
        if let Some(last) = self.messages.last() {
            if event.seq <= last.seq {
                return;
            }
        }
        self.messages.push(MessageRow {
            session_id: String::new(),
            seq: event.seq,
            turn: event.turn,
            event_type: event.typ.clone(),
            text,
        });
    }

    fn final_status(&self, snapshot: &hearth_session::Snapshot) -> String {
        if snapshot.ended && is_abort_like(&snapshot.end_reason) {
            return hearth_protocol::status::ABORTED.to_owned();
        }
        if snapshot.ended || self.status == hearth_protocol::status::ENDED {
            return hearth_protocol::status::ENDED.to_owned();
        }
        if self.status == hearth_protocol::status::ABORTED {
            return hearth_protocol::status::ABORTED.to_owned();
        }
        if !self.pending.is_empty() {
            return hearth_protocol::status::AWAITING_APPROVAL.to_owned();
        }
        if self.turn_open {
            return hearth_protocol::status::RUNNING.to_owned();
        }
        hearth_protocol::status::IDLE.to_owned()
    }
}

fn message_text(payload: Option<&JsonValue>) -> String {
    field_string(payload, &["text", "content"])
}

fn scan_session_log(id: &str, path: &Path) -> std::result::Result<Option<Scanned>, ScanFailure> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None), // directories and unreadable files are skipped
    };
    let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let text = String::from_utf8_lossy(&bytes);
    let events = match good_prefix(&text) {
        Ok(events) => events,
        Err(error) if error.kind() == ErrorKind::UnsupportedVersion => {
            return Err(ScanFailure::Unsupported)
        }
        Err(error) => return Err(ScanFailure::Other(error)),
    };

    let mut state = FoldState::default();
    let mut projection = Projection::new();
    for event in &events {
        // A malformed payload of a known type costs that event, not the whole
        // session. Any other projection error means the log is unusable.
        match projection.apply(event) {
            Ok(()) => {}
            Err(error)
                if error.kind() == ErrorKind::Invalid
                    && error.message().contains("malformed payload") => {}
            Err(error) => return Err(ScanFailure::Other(error)),
        }
        state.saw_event = true;
        state.apply_extras(event);
    }
    if !state.saw_event {
        return Ok(None);
    }
    let snapshot = projection.snapshot();
    let turns = if snapshot.counters.turns == 0 {
        snapshot.turns.len()
    } else {
        snapshot.counters.turns
    };
    let workspace = if state.workspace.is_empty() {
        snapshot.cwd.clone()
    } else {
        state.workspace.clone()
    };
    let created_at = if state.created_at == 0 {
        state.first_ts
    } else {
        state.created_at
    };
    let updated_at = if state.updated_at == 0 {
        created_at
    } else {
        state.updated_at
    };
    let status = state.final_status(&snapshot);
    let mut session = Session {
        id: id.to_owned(),
        workspace,
        model: snapshot.model,
        title: state.title,
        status,
        created_at,
        updated_at,
        turns: i64::try_from(turns).unwrap_or(i64::MAX),
        last_seq: state.last_seq_seen.max(snapshot.last_seq),
    };
    // Put's normalization rules also apply to Rebuild, so rows are identical
    // whichever path produced them.
    if session.status.is_empty() {
        session.status = hearth_protocol::status::IDLE.to_owned();
    }
    for message in &mut state.messages {
        message.session_id = id.to_owned();
    }
    Ok(Some(Scanned {
        session,
        messages: state.messages,
        bytes: size,
    }))
}

fn flush_messages(tx: &Transaction<'_>, rows: &[MessageRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut sql = String::from("INSERT INTO messages (session_id, seq, turn, type, text) VALUES ");
    for index in 0..rows.len() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str("(?, ?, ?, ?, ?)");
    }
    let mut values = Vec::with_capacity(rows.len() * 5);
    for row in rows {
        values.push(Value::Text(row.session_id.clone()));
        values.push(Value::Integer(i64::try_from(row.seq).unwrap_or(i64::MAX)));
        values.push(Value::Integer(i64::from(row.turn)));
        values.push(Value::Text(row.event_type.clone()));
        values.push(Value::Text(row.text.clone()));
    }
    tx.execute(&sql, params_from_iter(values.iter()))
        .map_err(|e| store_err("rebuild: insert messages", e))?;
    Ok(())
}

impl Store {
    /// Replace the entire index with the contents of the event logs and return
    /// the number of sessions indexed. Unreadable logs are skipped, never
    /// repaired: Rebuild only reads.
    pub fn rebuild(&self, data_dir: impl AsRef<Path>) -> Result<usize> {
        let _gate = self.read_gate()?;
        let _rebuild = self.rebuild_lock();
        self.ensure_open()?;
        let data_dir = data_dir.as_ref();
        if data_dir.as_os_str().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "store: rebuild: empty data directory",
            ));
        }
        let sessions_dir = data_dir.join("sessions");
        let mut jobs = Vec::new();
        match fs::read_dir(&sessions_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => continue,
                    };
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !name.ends_with(".jsonl") {
                        continue;
                    }
                    let id = name.trim_end_matches(".jsonl").to_owned();
                    if !valid_session_id(&id) {
                        continue;
                    }
                    jobs.push(ScanJob {
                        id,
                        path: entry.path(),
                    });
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        jobs.sort_by(|left, right| left.id.cmp(&right.id));

        // Folding is CPU-bound and independent per file; the single writer is
        // still deterministic. Without this fan-out a 100k-event rebuild is
        // dominated by JSON parsing rather than SQLite.
        let next = AtomicUsize::new(0);
        let outcomes: Mutex<Vec<(String, ScanOutcome)>> =
            Mutex::new(Vec::with_capacity(jobs.len()));
        let workers = MAX_SCAN_WORKERS.min(jobs.len().max(1));
        thread::scope(|scope| {
            for _ in 0..workers {
                let next = &next;
                let outcomes = &outcomes;
                let jobs = &jobs;
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(index) else {
                        break;
                    };
                    let scanned = scan_session_log(&job.id, &job.path);
                    let mut collected = outcomes.lock().unwrap_or_else(|p| p.into_inner());
                    collected.push((job.id.clone(), scanned));
                });
            }
        });
        let mut collected = outcomes.into_inner().unwrap_or_else(|p| p.into_inner());
        collected.sort_by(|left, right| left.0.cmp(&right.0));

        let mut guard = self.writer_conn()?;
        let connection = guard.as_mut().ok_or_else(err_closed)?;
        let tx = connection
            .transaction()
            .map_err(|e| store_err("rebuild: begin", e))?;

        // Drop before clearing. Otherwise every previously indexed message
        // fires an FTS delete trigger just to throw the index away; three
        // back-to-back rebuilds of the same corpus degraded from ~0.9 s to
        // 6.5 s in the Go oracle when the drop came after the clear.
        for name in ["messages_ai", "messages_ad", "messages_au"] {
            tx.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))
                .map_err(|e| store_err("rebuild: pause FTS triggers", e))?;
        }
        for statement in [
            "DELETE FROM messages",
            "DELETE FROM sessions",
            "DELETE FROM meta",
        ] {
            tx.execute_batch(statement)
                .map_err(|e| store_err("rebuild: clear index", e))?;
        }

        let mut insert_session = tx
            .prepare(
                "INSERT INTO sessions
                    (id, workspace, model, title, status, created_at, updated_at, turns, last_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .map_err(|e| store_err("rebuild: prepare session", e))?;
        let mut indexed = 0usize;
        let mut total_bytes = 0i64;
        let mut batch: Vec<MessageRow> = Vec::new();
        for (id, outcome) in collected {
            let scanned = match outcome {
                Ok(Some(scanned)) => scanned,
                Ok(None) => continue,
                Err(ScanFailure::Unsupported) => {
                    eprintln!("store: skipping session log from another format version: {id}");
                    continue;
                }
                // One bad log is not allowed to cost the other thousand. There
                // is no logger in this tree, so say what was skipped on stderr.
                Err(ScanFailure::Other(error)) => {
                    eprintln!("store: skipping unreadable session log {id}: {error}");
                    continue;
                }
            };
            let session = scanned.session;
            let last_seq = i64::try_from(session.last_seq).unwrap_or(i64::MAX);
            insert_session
                .execute(params![
                    session.id,
                    session.workspace,
                    session.model,
                    session.title,
                    session.status,
                    session.created_at,
                    session.updated_at,
                    session.turns,
                    last_seq,
                ])
                .map_err(|e| store_err("rebuild: insert session", e))?;
            batch.extend(scanned.messages);
            if batch.len() >= MESSAGE_BATCH_ROWS {
                flush_messages(&tx, &batch)?;
                batch.clear();
            }
            indexed += 1;
            total_bytes = total_bytes.saturating_add(scanned.bytes);
        }
        drop(insert_session);
        flush_messages(&tx, &batch)?;

        // FTS5's own bulk-load path re-reads the content table in one pass.
        tx.execute_batch("INSERT INTO messages_fts (messages_fts) VALUES ('rebuild')")
            .map_err(|e| store_err("rebuild: index messages for search", e))?;
        for statement in trigger_statements() {
            tx.execute_batch(statement)
                .map_err(|e| store_err("rebuild: restore FTS triggers", e))?;
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![META_BYTES_KEY, total_bytes],
        )
        .map_err(|e| store_err("rebuild: record size", e))?;
        tx.commit().map_err(|e| store_err("rebuild: commit", e))?;
        Ok(indexed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(seq: u64, typ: &str) -> String {
        format!(
            "{{\"v\":1,\"seq\":{seq},\"id\":\"e{seq}\",\"ts\":1700000000000,\"session\":\"s1\",\"type\":\"{typ}\"}}"
        )
    }

    #[test]
    fn session_ids_match_the_list_api_shape() {
        assert!(valid_session_id("a"));
        assert!(valid_session_id("A.b_c-9"));
        assert!(!valid_session_id(".abc"));
        assert!(!valid_session_id("bad..id"));
        assert!(!valid_session_id(&"a".repeat(129)));
    }

    #[test]
    fn abort_reasons_are_case_insensitive() {
        assert!(is_abort_like("  Cancelled "));
        assert!(is_abort_like("timeout"));
        assert!(!is_abort_like("done"));
    }

    #[test]
    fn torn_final_line_is_dropped_without_touching_the_file() {
        let text = format!("{}\n{{\"v\":1,\"seq\":2", line(1, "session/start"));
        let events = good_prefix(&text).expect("prefix");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn approval_ids_prefer_camel_case() {
        let payload: JsonValue =
            serde_json::from_str("{\"approvalId\":\"camel\",\"id\":\"snake\"}").expect("json");
        assert_eq!(field_string(Some(&payload), &["approvalId"]), "camel");
        assert_eq!(field_string(Some(&payload), &["id"]), "snake");
    }
}
