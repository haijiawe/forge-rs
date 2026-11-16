//! The store handle: one writer, several concurrent readers, no hidden state.
//!
//! Every public call takes the gate lock. That makes [`Store::close`] wait for
//! in-flight work and guarantees no connection is dropped while it is borrowed.
//! The writer connection is the only connection allowed to commit; readers are
//! `query_only`, so an accidental write fails instead of corrupting the index.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use forge_core::{Error, ErrorKind, Result};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection};

use crate::cursor::{decode_cursor, encode_cursor};
use crate::schema::{
    configure_reader, configure_writer, ensure_schema, META_BYTES_KEY, READER_CONNS,
};

const DEFAULT_LIMIT: i32 = 50;
const MAX_LIMIT: i32 = 200;
pub(crate) const MAX_TITLE_BYTES: usize = 120;

const ERR_CLOSED: &str = "store: closed";
const ERR_BAD_CURSOR: &str = "store: malformed cursor";

/// Options accepted by [`Store::open`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Path of `index.db`. The caller places it beside `sessions/`, never inside.
    pub path: PathBuf,
    /// Clock injected by tests; `None` uses the system clock (milliseconds).
    pub now: Option<fn() -> i64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            now: None,
        }
    }
}

/// One row of the session index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub workspace: String,
    pub model: String,
    pub title: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub turns: i64,
    pub last_seq: u64,
}

/// One FTS hit. `snippet` is at most 200 bytes, cut on a UTF-8 boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hit {
    pub session_id: String,
    pub seq: u64,
    pub turn: u32,
    pub event_type: String,
    pub snippet: String,
}

/// Aggregate counters for the index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub sessions: i64,
    pub events: i64,
    pub bytes: i64,
    pub workspaces: i64,
}

/// Filters and paging options for [`Store::list`].
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// 0 (or negative) means 50; values above 200 are clamped.
    pub limit: i32,
    /// Opaque token returned by a previous `list` call.
    pub cursor: String,
    pub status: String,
    pub workspace: String,
}

/// A rebuildable session index.
pub struct Store {
    path: PathBuf,
    writer: Mutex<Option<Connection>>,
    readers: Vec<Mutex<Option<Connection>>>,
    next_reader: AtomicUsize,
    gate: RwLock<()>,
    rebuild_gate: Mutex<()>,
    closed: AtomicBool,
    now: Option<fn() -> i64>,
}

pub(crate) fn store_err(context: impl std::fmt::Display, source: rusqlite::Error) -> Error {
    Error::new(ErrorKind::Io, format!("store: {context}: {source}")).with_source(source)
}

pub(crate) fn err_closed() -> Error {
    Error::new(ErrorKind::Refused, ERR_CLOSED)
}

/// Go used `errors.Is(err, ErrClosed)`; `forge_core::Error` has no downcast, so
/// callers identify the sentinel by kind and message prefix instead.
pub fn is_closed(error: &Error) -> bool {
    error.kind() == ErrorKind::Refused && error.message().starts_with(ERR_CLOSED)
}

pub(crate) fn err_bad_cursor(detail: &str) -> Error {
    Error::new(ErrorKind::Invalid, format!("{ERR_BAD_CURSOR}: {detail}"))
}

/// Companion to [`is_closed`] for the cursor sentinel.
pub fn is_bad_cursor(error: &Error) -> bool {
    error.kind() == ErrorKind::Invalid && error.message().starts_with(ERR_BAD_CURSOR)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn clip_rune_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// First user message is the title: trimmed, whitespace runs collapsed, and
/// clipped to 120 bytes without cutting a UTF-8 character in half.
pub(crate) fn clip_title(value: &str) -> String {
    let trimmed = value.trim();
    let mut collapsed = String::with_capacity(trimmed.len());
    let mut in_space = false;
    for ch in trimmed.chars() {
        if matches!(ch, '\n' | '\r' | '\t') {
            if !in_space {
                collapsed.push(' ');
                in_space = true;
            }
        } else {
            collapsed.push(ch);
            in_space = false;
        }
    }
    clip_rune_boundary(&collapsed, MAX_TITLE_BYTES).to_owned()
}

fn system_now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

impl Store {
    /// Open (and if needed create) the index at `options.path`.
    pub fn open(options: Options) -> Result<Self> {
        if options.path.as_os_str().is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "store: empty path"));
        }
        let text = options.path.to_string_lossy();
        // A named in-memory database would be a *different* database for every
        // connection in the reader pool. Refuse it rather than hand back an
        // index whose readers silently see no rows.
        if text == ":memory:" || text.starts_with("file::memory:") {
            return Err(Error::new(
                ErrorKind::Refused,
                "store: in-memory database would split the reader pool",
            ));
        }
        if let Some(parent) = options.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let mut writer =
            Connection::open(&options.path).map_err(|e| store_err("open writer", e))?;
        configure_writer(&writer)?;
        ensure_schema(&mut writer)?;

        let mut readers = Vec::with_capacity(READER_CONNS);
        for index in 0..READER_CONNS {
            let conn = Connection::open_with_flags(
                &options.path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| store_err(format!("open reader {index}"), e))?;
            configure_reader(&conn)?;
            readers.push(Mutex::new(Some(conn)));
        }

        Ok(Self {
            path: options.path,
            writer: Mutex::new(Some(writer)),
            readers,
            next_reader: AtomicUsize::new(0),
            gate: RwLock::new(()),
            rebuild_gate: Mutex::new(()),
            closed: AtomicBool::new(false),
            now: options.now,
        })
    }

    /// Path of the index file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Idempotent; drops every connection so the index files can be deleted on
    /// Windows. The gate makes this wait until in-flight calls have finished.
    pub fn close(&self) -> Result<()> {
        let _gate = self.gate.write().unwrap_or_else(|p| p.into_inner());
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let writer = lock(&self.writer).take();
        for reader in &self.readers {
            let _connection = lock(reader).take();
        }
        drop(writer);
        Ok(())
    }

    pub(crate) fn read_gate(&self) -> Result<RwLockReadGuard<'_, ()>> {
        Ok(self.gate.read().unwrap_or_else(|p| p.into_inner()))
    }

    pub(crate) fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(err_closed());
        }
        Ok(())
    }

    pub(crate) fn writer_conn(&self) -> Result<MutexGuard<'_, Option<Connection>>> {
        Ok(lock(&self.writer))
    }

    pub(crate) fn read_conn(&self) -> Result<MutexGuard<'_, Option<Connection>>> {
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        Ok(lock(&self.readers[index]))
    }

    pub(crate) fn rebuild_lock(&self) -> MutexGuard<'_, ()> {
        lock(&self.rebuild_gate)
    }

    fn now_ms(&self) -> i64 {
        self.now.map_or_else(system_now_ms, |clock| clock())
    }

    fn normalize(&self, mut value: Session) -> Session {
        if value.status.is_empty() {
            value.status = forge_protocol::status::IDLE.to_owned();
        }
        if value.created_at == 0 && value.updated_at == 0 {
            let now = self.now_ms();
            value.created_at = now;
            value.updated_at = now;
        }
        if value.created_at == 0 {
            value.created_at = value.updated_at;
        }
        if value.updated_at == 0 {
            value.updated_at = value.created_at;
        }
        if value.turns < 0 {
            value.turns = 0;
        }
        value.title = clip_title(&value.title);
        value
    }

    /// Insert or update a session row. Older timestamps never move a row
    /// backwards; an equal timestamp still applies (the Go oracle uses `>=`).
    pub fn put(&self, value: &Session) -> Result<()> {
        let _gate = self.read_gate()?;
        self.ensure_open()?;
        if value.id.is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "store: empty session id"));
        }
        let value = self.normalize(value.clone());
        let guard = self.writer_conn()?;
        let conn = guard.as_ref().ok_or_else(err_closed)?;
        let last_seq = i64::try_from(value.last_seq).unwrap_or(i64::MAX);
        conn.execute(
            "INSERT INTO sessions
                (id, workspace, model, title, status, created_at, updated_at, turns, last_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                workspace=excluded.workspace, model=excluded.model, title=excluded.title,
                status=excluded.status, created_at=excluded.created_at,
                updated_at=excluded.updated_at, turns=excluded.turns, last_seq=excluded.last_seq
             WHERE excluded.updated_at >= sessions.updated_at",
            params![
                value.id,
                value.workspace,
                value.model,
                value.title,
                value.status,
                value.created_at,
                value.updated_at,
                value.turns,
                last_seq,
            ],
        )
        .map_err(|e| store_err("put", e))?;
        Ok(())
    }

    /// Delete a session and its indexed messages. A missing id is a no-op.
    pub fn delete(&self, id: &str) -> Result<()> {
        let _gate = self.read_gate()?;
        self.ensure_open()?;
        if id.is_empty() {
            return Ok(());
        }
        let mut guard = self.writer_conn()?;
        let conn = guard.as_mut().ok_or_else(err_closed)?;
        let tx = conn
            .transaction()
            .map_err(|e| store_err("begin delete", e))?;
        tx.execute("DELETE FROM messages WHERE session_id = ?1", params![id])
            .map_err(|e| store_err("delete messages", e))?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(|e| store_err("delete session", e))?;
        tx.commit().map_err(|e| store_err("commit delete", e))
    }

    /// Keyset-paginated list: `(updated_at DESC, id ASC)` with an opaque cursor.
    /// Keyset, not OFFSET, so concurrent updates cannot repeat or skip rows.
    pub fn list(&self, query: Query) -> Result<(Vec<Session>, String)> {
        let _gate = self.read_gate()?;
        self.ensure_open()?;
        let limit = if query.limit <= 0 {
            DEFAULT_LIMIT
        } else {
            query.limit.min(MAX_LIMIT)
        };
        let (cursor_updated, cursor_id) = if query.cursor.is_empty() {
            (None, None)
        } else {
            let decoded = decode_cursor(&query.cursor)?;
            (Some(decoded.updated_at), Some(decoded.id))
        };

        let mut sql = String::from(
            "SELECT id, workspace, model, title, status, created_at, updated_at, turns, last_seq \
             FROM sessions",
        );
        let mut conditions: Vec<&str> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        if !query.status.is_empty() {
            conditions.push("status = ?");
            args.push(Value::Text(query.status));
        }
        if !query.workspace.is_empty() {
            conditions.push("workspace = ?");
            args.push(Value::Text(query.workspace));
        }
        if let (Some(updated), Some(id)) = (cursor_updated, cursor_id) {
            conditions.push("(updated_at < ? OR (updated_at = ? AND id > ?))");
            args.push(Value::Integer(updated));
            args.push(Value::Integer(updated));
            args.push(Value::Text(id));
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY updated_at DESC, id ASC LIMIT ?");
        args.push(Value::Integer(i64::from(limit) + 1));

        let guard = self.read_conn()?;
        let conn = guard.as_ref().ok_or_else(err_closed)?;
        let mut statement = conn
            .prepare(&sql)
            .map_err(|e| store_err("list prepare", e))?;
        let rows = statement
            .query_map(params_from_iter(args.iter()), |row| {
                let last_seq: i64 = row.get(8)?;
                Ok(Session {
                    id: row.get(0)?,
                    workspace: row.get(1)?,
                    model: row.get(2)?,
                    title: row.get(3)?,
                    status: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    turns: row.get(7)?,
                    // Negative values from a hand-edited index are impossible
                    // from our writer, but clamp anyway: the Go oracle does.
                    last_seq: u64::try_from(last_seq).unwrap_or(0),
                })
            })
            .map_err(|e| store_err("list", e))?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.map_err(|e| store_err("list row", e))?);
        }
        let next_cursor = if sessions.len() > limit as usize {
            sessions.truncate(limit as usize);
            let last = sessions.last().ok_or_else(err_closed)?;
            encode_cursor(last.updated_at, &last.id)?
        } else {
            String::new()
        };
        Ok((sessions, next_cursor))
    }

    /// Index-wide counters. `bytes` is summed by Rebuild from the log files.
    pub fn stats(&self) -> Result<Stats> {
        let _gate = self.read_gate()?;
        self.ensure_open()?;
        let guard = self.read_conn()?;
        let conn = guard.as_ref().ok_or_else(err_closed)?;
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(last_seq), 0),
                    (SELECT COALESCE(MAX(value), 0) FROM meta WHERE key = ?1),
                    COUNT(DISTINCT NULLIF(workspace, ''))
             FROM sessions",
            params![META_BYTES_KEY],
            |row| {
                Ok(Stats {
                    sessions: row.get(0)?,
                    events: row.get(1)?,
                    bytes: row.get(2)?,
                    workspaces: row.get(3)?,
                })
            },
        )
        .map_err(|e| store_err("stats", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_title_collapses_newlines_and_runes() {
        // Go collapses runs of \n\r\t, not ordinary spaces: "a", space,
        // newline, space, "b", tab, space, "c" keeps the surrounding spaces.
        assert_eq!(clip_title(" a \n b\t c "), "a   b  c");
        assert_eq!(clip_title("a\n\nb\tc"), "a b c");
        let long = "界".repeat(100);
        let clipped = clip_title(&long);
        assert_eq!(clipped.len(), 120); // 40 three-byte runes fit in 120 bytes
        assert!(clipped.chars().all(|c| c == '界'));
    }

    #[test]
    fn sentinels_are_kind_and_message_based() {
        assert!(is_closed(&err_closed()));
        assert!(!is_bad_cursor(&err_closed()));
        let bad = err_bad_cursor("test");
        assert!(is_bad_cursor(&bad));
        assert!(!is_closed(&bad));
    }
}
