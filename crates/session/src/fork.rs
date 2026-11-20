//! Forking a session: copy a prefix of one log into a new session.
//!
//! A fork is a new log, not a pointer into the parent. That keeps the parent
//! immutable (its hash must not change because someone branched from it) and
//! means a child can be replayed, moved or deleted without consulting the
//! parent. The only relationship left is provenance recorded in the child's
//! `session/start` payload.
//!
//! The cut must land between turns. A fork in the middle of a turn would leave
//! the child with a `turn/start` and a dangling tool call that no completed
//! assistant step produced; the projection would have to invent state. Refusing
//! the request is honest, and `ErrForkPointMidTurn` tells the caller why.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hearth_core::{new_id, Error, ErrorKind, Event, EventType, Result};
use serde_json::{Map, Value};

use crate::projection::Projection;

/// Exact refusal texts. They are public so a caller can distinguish the
/// situations without parsing an error string, while the `ErrorKind` keeps the
/// usual branch (`NotFound`, `Conflict`, `Invalid`).
pub const ERR_FORK_PARENT_MISSING: &str = "session: fork parent log does not exist";
pub const ERR_FORK_TARGET_EXISTS: &str = "session: fork target already exists";
pub const ERR_FORK_NO_POINT: &str = "session: fork needs a completed turn to cut at";
pub const ERR_FORK_POINT_MISSING: &str = "session: fork point is not a seq in the parent log";
pub const ERR_FORK_POINT_MID_TURN: &str = "session: fork point falls inside a turn";
pub const ERR_FORK_PARENT_INVALID: &str = "session: fork parent log is not a copyable v1 log";
pub const ERR_FORK_INVALID_REQUEST: &str = "session: invalid fork request";

#[derive(Debug, Clone, Default)]
pub struct ForkOptions {
    pub parent_path: PathBuf,
    pub target_path: PathBuf,
    pub target_id: String,
    /// Cut after the event with this seq. `0` means "after the last completed
    /// turn", which is the only cut that never splits a turn by construction.
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkResult {
    pub session_id: String,
    pub parent_id: String,
    pub forked_at_seq: u64,
    pub events: usize,
    pub bytes: u64,
}

/// Copy `parent_path` into `target_path` under `target_id`.
///
/// Accepts either an owned [`ForkOptions`] or a borrowed one; the Go reference
/// takes the value, while Rust callers often keep the request around to log it.
pub fn fork<O>(options: O) -> Result<ForkResult>
where
    O: std::borrow::Borrow<ForkOptions>,
{
    let options = options.borrow();
    if options.target_id.is_empty()
        || options.parent_path.as_os_str().is_empty()
        || options.target_path.as_os_str().is_empty()
    {
        return Err(Error::new(ErrorKind::Invalid, ERR_FORK_INVALID_REQUEST));
    }
    let expected_name = format!("{}.jsonl", options.target_id);
    match options.target_path.file_name().and_then(|n| n.to_str()) {
        Some(name) if name == expected_name => {}
        _ => {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("{ERR_FORK_INVALID_REQUEST}: target file must be named {expected_name}"),
            ))
        }
    }
    if options.target_path.exists() {
        return Err(Error::new(ErrorKind::Conflict, ERR_FORK_TARGET_EXISTS));
    }

    let parent = read_fork_parent(&options.parent_path)?;
    let parent_id = parent
        .first()
        .map(|event| event.session.clone())
        .ok_or_else(|| Error::new(ErrorKind::Invalid, ERR_FORK_PARENT_INVALID))?;

    let cut = fork_cut(&parent, options.at)?;
    let prefix = &parent[..cut];

    // Fold the prefix before writing anything: a fork that cannot be projected
    // (mid-turn point, malformed history) must not leave a target file behind.
    let mut projection = Projection::new();
    for event in prefix {
        projection.apply(event)?;
    }
    let snapshot = projection.snapshot();
    if snapshot.turns.iter().any(|turn| turn.end_seq == 0) {
        return Err(Error::new(ErrorKind::Invalid, ERR_FORK_POINT_MID_TURN));
    }

    let at_seq = prefix.last().map(|event| event.seq).unwrap_or(0);
    let now = now_millis();
    let mut output: Vec<Event> = Vec::with_capacity(prefix.len() + 1);

    // The branch value is the index of the first parent record the child still
    // has to copy: 1 when the parent's own session/start became the child's
    // head, 0 when a fresh head was synthesised in front of every parent record.
    let copy_from = if prefix[0].typ == EventType::SESSION_START.as_str() {
        let mut head = prefix[0].clone();
        head.seq = 1;
        // The id is copied, not minted: events-v1 §4.1 allows exactly two
        // envelope changes per copied record (session and seq), and says why —
        // a reader must be able to correlate a copied record with the one in
        // the parent. Uniqueness is a per-log invariant (§9), so an id that
        // appears in two logs is not a collision.
        head.session = options.target_id.clone();
        head.payload = Some(fork_head_payload(
            &prefix[0].payload,
            &parent_id,
            at_seq,
            now,
        )?);
        output.push(head);
        1
    } else {
        // A parent that starts with a turn is still a readable v1 log (the
        // projection synthesises a turn for `turn/end`), but a child needs a
        // session/start so later readers know its model and workspace. The
        // values come from the fold, never from guesswork.
        let mut head = Event::new(
            options.target_id.clone(),
            EventType::SESSION_START,
            Some(Value::Object(Map::from_iter([
                ("workspace".to_string(), Value::String(snapshot.cwd.clone())),
                ("model".to_string(), Value::String(snapshot.model.clone())),
                (
                    "profile".to_string(),
                    Value::String(snapshot.profile.clone()),
                ),
                ("startedBy".to_string(), Value::String("fork".to_string())),
                ("forkedFrom".to_string(), Value::String(parent_id.clone())),
                ("forkedAtSeq".to_string(), Value::Number(at_seq.into())),
                ("forkedAt".to_string(), Value::Number(now.into())),
            ]))),
        );
        head.seq = 1;
        head.ts = now;
        head.id = new_id()?;
        output.push(head);
        // No record was consumed for the head: every parent record is history
        // the child must keep.
        0
    };

    for event in &prefix[copy_from..] {
        let mut copied = event.clone();
        copied.seq = output.len() as u64 + 1;
        // Two envelope changes only: session and seq. The id, timestamp, turn,
        // step, type and payload are the parent's bytes — that is what makes a
        // forked record attributable to the record it came from.
        copied.session = options.target_id.clone();
        output.push(copied);
    }

    let bytes = write_fork_log(&options.target_path, &output)?;
    Ok(ForkResult {
        session_id: options.target_id.clone(),
        parent_id,
        forked_at_seq: at_seq,
        events: output.len(),
        bytes,
    })
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Read a parent log. A torn final line is not an error: it was never a
/// record, so the copy simply stops before it. A version that is not v1 is an
/// error and nothing is written — that is the one case where guessing would
/// corrupt a child.
fn read_fork_parent(path: &Path) -> Result<Vec<Event>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::new(ErrorKind::NotFound, ERR_FORK_PARENT_MISSING))
        }
        Err(e) => return Err(e.into()),
    };
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    let mut events: Vec<Event> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut start = 0usize;
    while start < data.len() {
        let Some(relative) = data[start..].iter().position(|&b| b == b'\n') else {
            // Unterminated final line: a writer died mid-record. Everything
            // before it is a valid prefix; the fragment was never an event.
            break;
        };
        let end = start + relative;
        let line = &data[start..end];
        start = end + 1;
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let text = match std::str::from_utf8(line) {
            Ok(text) => text,
            Err(_) => break,
        };
        let event = match Event::parse_line(text) {
            Ok(event) => event,
            Err(e) if e.kind() == ErrorKind::UnsupportedVersion => return Err(e),
            Err(_) => break,
        };
        if event.seq != events.len() as u64 + 1 {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "{ERR_FORK_PARENT_INVALID}: seq {} is not dense at index {}",
                    event.seq,
                    events.len()
                ),
            ));
        }
        if !seen_ids.insert(event.id.clone()) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("{ERR_FORK_PARENT_INVALID}: duplicate event id {}", event.id),
            ));
        }
        events.push(event);
    }
    if events.is_empty() {
        return Err(Error::new(ErrorKind::Invalid, ERR_FORK_PARENT_INVALID));
    }
    Ok(events)
}

/// Number of records to copy. `at == 0` uses the last turn/end, which is the
/// only default that cannot split a turn.
fn fork_cut(events: &[Event], at: u64) -> Result<usize> {
    if at == 0 {
        for (index, event) in events.iter().enumerate().rev() {
            if event.typ == EventType::TURN_END.as_str() {
                return Ok(index + 1);
            }
        }
        return Err(Error::new(ErrorKind::Invalid, ERR_FORK_NO_POINT));
    }
    for (index, event) in events.iter().enumerate() {
        if event.seq == at {
            return Ok(index + 1);
        }
    }
    Err(Error::new(ErrorKind::Invalid, ERR_FORK_POINT_MISSING))
}

fn fork_head_payload(
    payload: &Option<Value>,
    parent_id: &str,
    at_seq: u64,
    now: i64,
) -> Result<Value> {
    let mut map = match payload {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("{ERR_FORK_PARENT_INVALID}: session/start payload is not an object"),
            ))
        }
    };
    map.insert(
        "forkedFrom".to_string(),
        Value::String(parent_id.to_string()),
    );
    map.insert("forkedAtSeq".to_string(), Value::Number(at_seq.into()));
    map.insert("forkedAt".to_string(), Value::Number(now.into()));
    Ok(Value::Object(map))
}

fn write_fork_log(path: &Path, events: &[Event]) -> Result<u64> {
    let file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Error::new(ErrorKind::Conflict, ERR_FORK_TARGET_EXISTS))
        }
        Err(e) => return Err(e.into()),
    };
    let result = write_all_lines(file, events);
    if result.is_err() {
        // A half-written child would be indistinguishable from a torn log that
        // was once valid. Remove it so the caller can retry cleanly.
        let _ = fs::remove_file(path);
    }
    result
}

fn write_all_lines(file: File, events: &[Event]) -> Result<u64> {
    let mut writer = BufWriter::new(file);
    let mut bytes = 0u64;
    for event in events {
        let line = event.canonical()?;
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        bytes += line.len() as u64 + 1;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(bytes)
}
