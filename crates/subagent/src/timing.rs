//! How long a child's turns actually ran — the `subagentTiming` projection.
//!
//! Ported from DSH `packages/subagent/subagent/src/projection.ts`
//! (`subagentTimingProjectionDefinition`, key `subagentTiming`, stateVersion 3),
//! reduced to the shape this tree needs: a small value folded out of a child's
//! own log, never a live timer. Every number here is recovered from
//! `turn/start` and `turn/end` timestamps, so a restart loses nothing — that is
//! AGENTS.md law 1 applied to duration, exactly as it is applied to final text.
//!
//! Three deliberate differences from DSH's fold:
//!
//! * DSH keeps an open interval (`active: {since, through}`) for a UI clock.
//!   Here `active` is a bool and [`Timing::settled_ms`] is only what is already
//!   banked, because nothing in this tree renders a ticking counter.
//! * DSH pairs turns by position alone. Here a turn pairs by `payload.index`
//!   when both sides carry one, and by arrival order when they do not — a log
//!   written by a loop that numbers its turns keeps working even if a record is
//!   missing in the middle, and a log that does not number them still reads.
//! * DSH keys `lastTurnCompleted` off `event.data.reason.kind`. Here
//!   [`Timing::last_turn_completed`] is the closing timestamp instead: "did it
//!   finish" is a question the caller can answer from the reason on the row it
//!   already has, while "when did it finish" is the fact only this fold sees.
//!
//! Reads only. Nothing in this module appends an event or opens a log for
//! writing, so a timing read can never make a log that a crashed run left
//! behind look healthier than it is.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use forge_core::{Error, ErrorKind, Event, EventType, Result};
use serde_json::Value;

use crate::catalog;
use crate::child::{child_log_path, is_child_id};
use crate::list;

/// One child's turn timing, as its own log proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timing {
    /// The child session this projection describes.
    pub child_id: String,
    /// The sum of every closed turn's `turn/end.ts - turn/start.ts`, in
    /// milliseconds. A turn whose clock ran backwards contributes zero, never a
    /// negative number.
    pub settled_ms: i64,
    /// Whether a turn is open right now: at least one `turn/start` with no
    /// matching `turn/end`. Always false once `session/end` has been read — a
    /// session that ended has no running turn, whatever the log left open.
    pub active: bool,
    /// The timestamp of the last `turn/end` that closed a turn, or `None` when
    /// no turn ever completed.
    pub last_turn_completed: Option<i64>,
    /// How many turns closed. Only a paired start counts; an orphan end is not
    /// a turn.
    pub turns: usize,
}

impl Timing {
    /// A child with no closed turn and no open one. Used when a catalog row
    /// names a child whose log this machine does not have.
    fn nothing_yet(child_id: &str) -> Timing {
        Timing {
            child_id: child_id.to_string(),
            settled_ms: 0,
            active: false,
            last_turn_completed: None,
            turns: 0,
        }
    }
}

/// Fold one child's timing out of its events.
///
/// Damage is skipped, never reported: an unknown event type, a record without a
/// usable timestamp, and a `turn/end` that pairs with nothing all contribute
/// zero and do not stop the fold. The log is data (AGENTS.md law 2), and a time
/// projection that refuses to render a log because one row is odd is worse than
/// one that renders what it can.
pub fn of_log(child_id: &str, events: &[Event]) -> Timing {
    // Starts that carry no `index`, in arrival order: the n-th such end closes
    // the n-th such start.
    let mut ordinal: VecDeque<i64> = VecDeque::new();
    // Starts that carry an `index`, oldest first. A log that re-used a turn
    // number leaves two entries here, and an end takes the oldest — first in,
    // first closed, the same rule the ordinal queue follows.
    let mut indexed: Vec<(u64, i64)> = Vec::new();
    let mut timing = Timing::nothing_yet(child_id);
    let mut ended = false;

    for event in events {
        // A record with no usable timestamp cannot contribute a duration. Zero
        // and negative values are how an unwritten clock shows up here.
        if event.ts <= 0 {
            continue;
        }
        let typ = event.typ.as_str();
        if typ == EventType::TURN_START.as_str() {
            match index_of(event) {
                Some(index) => indexed.push((index, event.ts)),
                None => ordinal.push_back(event.ts),
            }
        } else if typ == EventType::TURN_END.as_str() {
            let start = match index_of(event) {
                Some(index) => take_indexed(&mut indexed, index),
                None => ordinal.pop_front(),
            };
            let Some(start) = start else {
                // An end with no start describes a turn this log never opened.
                // Ignored rather than counted, so a duplicated end cannot
                // invent a turn that never ran.
                continue;
            };
            // A clock that stepped backwards must never make a turn look like it
            // un-ran; the floor is zero.
            timing.settled_ms += (event.ts - start).max(0);
            timing.turns += 1;
            timing.last_turn_completed = Some(event.ts);
        } else if typ == EventType::SESSION_END.as_str() {
            ended = true;
        }
    }

    // An open start is an open turn, unless the session wrote its terminal
    // record: then the turn is a crash artifact, not a running one.
    timing.active = !ended && (!ordinal.is_empty() || !indexed.is_empty());
    timing
}

/// Read one child's log and fold its timing.
///
/// A child id that could not be a child—one with a path separator or `..`—is
/// [`ErrorKind::Invalid`] before any path is built, so a caller can never turn a
/// timing read into a directory escape. A well-formed id with no log on disk is
/// [`ErrorKind::NotFound`]; both messages name the id, because the caller is
/// usually a human looking at one line of output.
pub fn of_child(data_dir: &Path, child_id: &str) -> Result<Timing> {
    if !is_child_id(child_id) {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("subagent timing: not a child id: {child_id:?}"),
        ));
    }
    let path = child_log_path(data_dir, child_id);
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!(
                "subagent timing: no log for child {child_id} (looked for {})",
                path.display()
            ),
        ));
    }
    Ok(of_log(child_id, &list::read_events(&path)?))
}

/// Every direct child recorded on a parent's log, with its timing, sorted by
/// `child_id`.
///
/// The children come from the parent's own `subagent/catalog` rows, so this is a
/// read of one log plus one read per child — it never invents a child, and it
/// never starts one.
///
/// A row whose child log is **not on this disk** still appears, as a row of
/// zeroes (`settled_ms: 0`, `active: false`, `last_turn_completed: None`,
/// `turns: 0`) with its real `child_id`: the parent's record that the child
/// existed is true even when the child's log has been moved or cleaned away, and
/// silently dropping the row would make a list that disagrees with the catalog.
/// Any other per-child failure — a log that exists and cannot be parsed — is
/// returned, because that is a fact about this machine worth failing on.
///
/// The result is deterministic: the same parent log and the same child logs
/// produce byte-identical values across calls, which is what lets a caller
/// compare two reads as a cheap consistency check.
pub fn children(data_dir: &Path, parent_session_id: &str) -> Result<Vec<Timing>> {
    let parent_id = parent_session_id.trim();
    if !is_session_id(parent_id) {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("subagent timing: not a session id: {parent_session_id:?}"),
        ));
    }
    let events = list::read_events(&child_log_path(data_dir, parent_id))?;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<Timing> = Vec::new();
    for entry in catalog::fold(&events) {
        // The catalog is the parent's memory: one row per child. A repeated row
        // (a log written twice by a bug) must not produce two timings for one
        // child, because the second read would be of the same file.
        if !seen.insert(entry.child_id.clone()) {
            continue;
        }
        match of_child(data_dir, &entry.child_id) {
            Ok(timing) => out.push(timing),
            Err(e) if e.kind() == ErrorKind::NotFound || e.kind() == ErrorKind::Invalid => {
                out.push(Timing::nothing_yet(&entry.child_id));
            }
            Err(e) => return Err(e),
        }
    }
    out.sort_by(|a, b| a.child_id.cmp(&b.child_id));
    Ok(out)
}

/// The `payload.index` of a turn record, when it has a usable one.
fn index_of(event: &Event) -> Option<u64> {
    event
        .payload
        .as_ref()
        .and_then(|payload| payload.get("index"))
        .and_then(Value::as_u64)
}

/// Remove and return the oldest open start carrying `index`.
fn take_indexed(open: &mut Vec<(u64, i64)>, index: u64) -> Option<i64> {
    let at = open.iter().position(|(candidate, _)| *candidate == index)?;
    Some(open.remove(at).1)
}

/// Whether `id` may be joined under `<data_dir>/sessions`.
///
/// Session ids are not child ids — they carry `_` — so [`is_child_id`] cannot be
/// reused. What matters is the same thing: the value is one path segment. The
/// contents are otherwise not checked, because a reader must be able to read a
/// log it has never seen (AGENTS.md law 2).
fn is_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && !id.contains('/')
        && !id.contains('\\')
}
