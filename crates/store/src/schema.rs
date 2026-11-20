//! SQLite schema and connection pragmas.
//!
//! The index is disposable. A schema version we do not recognise is **never**
//! migrated: we drop every object and rebuild from the logs. That is the only
//! safe response to an unknown layout because the log, not the index, is the
//! truth.

use hearth_core::{Error, ErrorKind, Result};
use rusqlite::Connection;

use crate::store::store_err;

pub(crate) const SCHEMA_VERSION: i64 = 1;
pub(crate) const BUSY_TIMEOUT_MS: i64 = 5_000;
pub(crate) const READER_CONNS: usize = 4;
pub(crate) const META_BYTES_KEY: &str = "bytes";

const CREATE_STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS sessions (
        id           TEXT PRIMARY KEY,
        workspace    TEXT NOT NULL DEFAULT '',
        model        TEXT NOT NULL DEFAULT '',
        title        TEXT NOT NULL DEFAULT '',
        status       TEXT NOT NULL DEFAULT '',
        created_at   INTEGER NOT NULL DEFAULT 0,
        updated_at   INTEGER NOT NULL DEFAULT 0,
        turns        INTEGER NOT NULL DEFAULT 0,
        last_seq     INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE INDEX IF NOT EXISTS sessions_updated ON sessions(updated_at DESC, id ASC)",
    "CREATE INDEX IF NOT EXISTS sessions_status ON sessions(status, updated_at DESC, id ASC)",
    "CREATE INDEX IF NOT EXISTS sessions_workspace ON sessions(workspace, updated_at DESC, id ASC)",
    "CREATE TABLE IF NOT EXISTS messages (
        id         INTEGER PRIMARY KEY,
        session_id TEXT NOT NULL,
        seq        INTEGER NOT NULL,
        turn       INTEGER NOT NULL DEFAULT 0,
        type       TEXT NOT NULL DEFAULT '',
        text       TEXT NOT NULL DEFAULT '',
        UNIQUE(session_id, seq)
    )",
    "CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, seq)",
    // External-content FTS5: `messages` stays the only copy of the text. The
    // tokenizer mirrors the Go oracle; the trigger set below is what keeps the
    // index in step with ordinary Put/Delete writes.
    "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
        text,
        content='messages',
        content_rowid='id',
        tokenize='unicode61 remove_diacritics 2'
    )",
    "CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END",
    "CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
    END",
    "CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
        INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END",
    "CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value INTEGER NOT NULL
    )",
];

const TRIGGER_STATEMENTS_RANGE: std::ops::Range<usize> = 6..9;

/// The three FTS maintenance triggers, in creation order. Rebuild drops them
/// before bulk loading and restores them from this same slice, so there is one
/// definition of the trigger bodies in the crate.
pub(crate) fn trigger_statements() -> &'static [&'static str] {
    &CREATE_STATEMENTS[TRIGGER_STATEMENTS_RANGE]
}

const DROP_STATEMENTS: &[&str] = &[
    // The external-content FTS table must go first: it reads `messages` to
    // delete its own shadow entries, so dropping `messages` first would leave
    // dangling FTS state behind.
    "DROP TABLE IF EXISTS messages_fts",
    "DROP TRIGGER IF EXISTS messages_ai",
    "DROP TRIGGER IF EXISTS messages_ad",
    "DROP TRIGGER IF EXISTS messages_au",
    "DROP TABLE IF EXISTS messages",
    "DROP TABLE IF EXISTS sessions",
    "DROP TABLE IF EXISTS meta",
];

pub(crate) fn configure_writer(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))
        .map_err(|e| store_err("busy_timeout", e))?;
    // WAL gives readers a stable snapshot while the single writer is busy.
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(|e| store_err("journal_mode=WAL", e))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::new(
            ErrorKind::Refused,
            format!("store: journal_mode={mode}, want wal"),
        ));
    }
    // NORMAL is intentional: the event log is the durable truth; this index is
    // rebuildable, so we do not pay FULL fsync cost on every commit.
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| store_err("synchronous=NORMAL", e))
}

pub(crate) fn configure_reader(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))
        .map_err(|e| store_err("busy_timeout", e))?;
    conn.pragma_update(None, "query_only", "ON")
        .map_err(|e| store_err("query_only=ON", e))
}

pub(crate) fn ensure_schema(conn: &mut Connection) -> Result<()> {
    let tx = conn
        .transaction()
        .map_err(|e| store_err("begin ensure_schema", e))?;
    let version: i64 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| store_err("PRAGMA user_version", e))?;
    if version != SCHEMA_VERSION {
        for sql in DROP_STATEMENTS {
            tx.execute_batch(sql)
                .map_err(|e| store_err("drop schema", e))?;
        }
    }
    for sql in CREATE_STATEMENTS {
        tx.execute_batch(sql)
            .map_err(|e| store_err("create schema", e))?;
    }
    if version != SCHEMA_VERSION {
        // Stamp only after every object exists, so an interrupted creation
        // leaves version 0 and the next open starts over from scratch.
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|e| store_err("stamp user_version", e))?;
    }
    tx.commit()
        .map_err(|e| store_err("commit ensure_schema", e))
}
