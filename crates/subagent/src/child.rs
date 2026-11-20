//! Child session mechanics: fresh ids, the head/end records, and reading the
//! final answer back out of the log.
//!
//! A subagent child is a NEW session with a FRESH id. It never borrows event
//! ids from its parent (that rule belongs to `forge_session::fork`, not here).
//! The only parent link is the `parent` member of `session/start`.

use std::path::PathBuf;

use forge_core::{new_id, Error, ErrorKind, Event, EventType, Result};
use forge_session::Store;
use serde_json::{json, Map, Value};

use crate::descriptor::{Descriptor, Mode};
use crate::options::Options;

/// Outcome of one child run. Text and turns come from the log, never from the
/// aggregate provider result — the log is the only truth.
#[derive(Debug, Default)]
pub struct ChildRun {
    pub id: String,
    pub path: PathBuf,
    pub model: String,
    pub steps: usize,
    pub turns: usize,
    pub text: String,
    /// The object the child submitted through `structured_output`, recovered
    /// from its own log. `None` when the child never submitted one (or when no
    /// schema was requested for this delegation).
    pub structured: Option<Value>,
    /// Set when the agent loop started and then failed. Setup failures are
    /// returned as `Err` from `Runner::run` instead.
    pub error: Option<forge_core::Error>,
    pub cancelled: bool,
    /// The sibling name reserved in the parent catalog, when there was a parent
    /// log to reserve it against.
    pub name: Option<String>,
    /// The `subagent/catalog` payload the parent loop must append, if any. The
    /// runner fills this so the tool can hand it up in `Result.meta` — a tool
    /// never writes the parent log itself.
    pub catalog: Option<Value>,
    /// How this child was started: `OneShot` is terminal at the end of its run;
    /// `Continuable` stays open so [`crate::runner::Runner::resume`] can append
    /// another turn to the same log.
    pub mode: Mode,
    /// The delegation depth at which this child was created (0 for a child of the
    /// root). Read back from `session/start`'s `delegationDepth`, so a cold resume
    /// in a new process counts depth from the header, not from 0 — otherwise a
    /// resumed child could spawn grandchildren past `maxDepth`.
    pub depth: usize,
}

/// `<data_dir>/sessions/<child_id>.jsonl`.
pub fn child_log_path(data_dir: &std::path::Path, child_id: &str) -> PathBuf {
    data_dir.join("sessions").join(format!("{child_id}.jsonl"))
}

/// UTC civil timestamp `YYYYMMDD-HHMMSS` for an epoch-millisecond value.
/// Implemented locally because there is no chrono in the workspace budget.
/// Negative instants are clamped to the epoch; this only mints ids.
pub fn format_stamp(ms: i64) -> String {
    let secs = if ms <= 0 { 0 } else { ms / 1000 };
    let days = secs / 86_400;
    let sod = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Howard Hinnant's `civil_from_days`, valid for proleptic Gregorian dates.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y, m, d)
}

/// Mint a fresh child id: `<stamp>-<first 8 hex of a new random id>`. No
/// collision with the parent is possible because a new random suffix is drawn.
pub(crate) fn new_child_id(now_ms: i64) -> Result<String> {
    let id = new_id()?;
    let suffix: String = id.chars().take(8).collect();
    Ok(format!("{}-{}", format_stamp(now_ms), suffix))
}

/// Create `<data_dir>/sessions` and open a fresh log. Retries id minting on a
/// path collision, exactly like the Go implementation.
// The lineage (workspace/model/system/parent/depth) plus the creation mode and
// label is the full head a child must record; splitting it would only relocate
// the arguments.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_child_log(
    opt: &Options,
    workspace: &str,
    model: &str,
    system: &str,
    parent: &str,
    depth: usize,
    mode: Mode,
    label: Option<&str>,
) -> Result<(ChildRun, Store)> {
    let sessions = opt.data_dir.join("sessions");
    std::fs::create_dir_all(&sessions).map_err(|e| {
        forge_core::Error::new(
            forge_core::ErrorKind::Io,
            format!(
                "subagent: cannot create sessions dir {}: {e}",
                sessions.display()
            ),
        )
    })?;

    let mut last = None;
    for _ in 0..4 {
        let id = new_child_id((opt.now)())?;
        let path = child_log_path(&opt.data_dir, &id);
        if path.exists() {
            last = Some(forge_core::Error::new(
                forge_core::ErrorKind::Conflict,
                format!("subagent: child log id collision: {id}"),
            ));
            continue;
        }
        let store = forge_session::Store::open(
            &path,
            forge_session::Options {
                no_fsync: true,
                ..forge_session::Options::default()
            },
        )?;
        write_head(&store, &id, workspace, model, system, parent, depth)?;
        // The child's own record of what it was started as. Written here, right
        // after the head, so the descriptor precedes every turn: this process is
        // the child log's only writer until `Agent::run` takes over, then again
        // for `write_end`, so there is no race to sequence around.
        let descriptor = match mode {
            Mode::OneShot => Descriptor::one_shot("local"),
            Mode::Continuable => Descriptor::continuable("local", label.unwrap_or("")),
        };
        store.append(&descriptor.event(&id))?;
        return Ok((
            ChildRun {
                id,
                path,
                model: model.to_string(),
                mode,
                depth,
                ..ChildRun::default()
            },
            store,
        ));
    }
    Err(last.unwrap_or_else(|| {
        forge_core::Error::new(
            forge_core::ErrorKind::Io,
            "subagent: could not mint a unique child id",
        )
    }))
}

/// Write `session/start` with the child's lineage. Written before the agent
/// loop so that `Agent::ensure_session` is a no-op and cannot overwrite the
/// `startedBy`/`parent`/`depth` facts.
pub(crate) fn write_head(
    store: &Store,
    child_id: &str,
    workspace: &str,
    model: &str,
    system: &str,
    parent: &str,
    depth: usize,
) -> Result<()> {
    let mut payload = Map::new();
    payload.insert("workspace".into(), json!(workspace));
    payload.insert("model".into(), json!(model));
    payload.insert("startedBy".into(), json!("subagent"));
    payload.insert("depth".into(), json!(depth));
    // The header is the authoritative, monotone floor for depth (DSH's
    // `delegationDepthOf` takes `max(header, options)`). A resuming process must
    // read this rather than start counting from zero.
    payload.insert("delegationDepth".into(), json!(depth));
    if !parent.is_empty() {
        payload.insert("parent".into(), json!(parent));
    }
    if !system.is_empty() {
        let (kept, truncated) = forge_agent::clip_system(system, forge_agent::MAX_SYSTEM_BYTES);
        payload.insert("system".into(), json!(kept));
        if truncated {
            payload.insert("systemTruncated".into(), json!(true));
            payload.insert("systemBytes".into(), json!(system.len()));
        }
    }
    store.append(&Event::new(
        child_id,
        EventType::SESSION_START,
        Some(Value::Object(payload)),
    ))?;
    Ok(())
}

/// Write the single terminal `session/end` record for a delegation.
pub(crate) fn write_end(store: &Store, child_id: &str, reason: &str, turns: usize) -> Result<()> {
    store.append(&Event::new(
        child_id,
        EventType::SESSION_END,
        Some(json!({"reason": reason, "turns": turns})),
    ))?;
    Ok(())
}

/// Read the child's turn count and its LAST non-empty assistant text.
/// Unknown event types are data and are skipped; a read failure is non-fatal
/// here (the head/tail are already in the log and the caller still reports
/// the run) — mirrored from Go's `readChildFacts`.
pub(crate) fn read_child_facts(store: &Store) -> (usize, String) {
    let mut turns = 0usize;
    let mut text = String::new();
    let read = store.read_from(0, |event: &Event| {
        match event.typ.as_str() {
            t if t == EventType::TURN_END.as_str() => turns += 1,
            t if t == EventType::ASSISTANT_MESSAGE.as_str() => {
                if let Some(payload) = &event.payload {
                    if let Some(t) = payload.get("text").and_then(Value::as_str) {
                        if !t.trim().is_empty() {
                            text = t.to_string();
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    });
    if read.is_err() {
        return (turns, text);
    }
    (turns, text)
}

/// True when `id` is shaped like a child id (`<stamp>-<hex>`) and is safe to
/// join onto `<data_dir>/sessions`. Rejects separators and `..` before any path
/// is built, so a caller can never turn a resume into a directory escape.
pub fn is_child_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && id != "."
        && id != ".."
}

/// Reopen an existing child log for a cold resume. Fails when the id is not a
/// child id, the log is missing, the child was one-shot, or it already wrote a
/// terminal `session/end` — in each case with a different, honest reason.
pub(crate) fn reopen_child_log(opt: &Options, child_id: &str) -> Result<(ChildRun, Store)> {
    if !is_child_id(child_id) {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("subagent: not a child id: {child_id:?}"),
        ));
    }
    let path = child_log_path(&opt.data_dir, child_id);
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("subagent: no such child session: {}", path.display()),
        ));
    }
    let store = Store::open(
        &path,
        forge_session::Options {
            no_fsync: true,
            ..forge_session::Options::default()
        },
    )?;

    let mut events: Vec<Event> = Vec::new();
    store.read_from(0, |event: &Event| {
        events.push(event.clone());
        Ok(())
    })?;

    let mut model = String::new();
    let mut ended = false;
    let mut depth = 0usize;
    for event in &events {
        match event.typ.as_str() {
            t if t == EventType::SESSION_START.as_str() => {
                if let Some(payload) = &event.payload {
                    model = payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    // `delegationDepth` is the authoritative field; fall back to the
                    // older `depth` for logs written before it existed.
                    depth = payload
                        .get("delegationDepth")
                        .or_else(|| payload.get("depth"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                }
            }
            t if t == EventType::SESSION_END.as_str() => ended = true,
            _ => {}
        }
    }
    if ended {
        return Err(Error::new(
            ErrorKind::Refused,
            format!("subagent: child {child_id} already wrote session/end and cannot be resumed"),
        ));
    }
    match crate::descriptor::fold(&events) {
        Some(descriptor) if descriptor.mode == Mode::Continuable => {}
        Some(_) => {
            return Err(Error::new(
                ErrorKind::Refused,
                format!("subagent: child {child_id} was started as one-shot and cannot be resumed"),
            ))
        }
        None => {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("subagent: child {child_id} has no descriptor; it cannot be resumed"),
            ))
        }
    }

    let (turns, text) = read_child_facts(&store);
    Ok((
        ChildRun {
            id: child_id.to_string(),
            path,
            model,
            turns,
            text,
            mode: Mode::Continuable,
            depth,
            ..ChildRun::default()
        },
        store,
    ))
}
