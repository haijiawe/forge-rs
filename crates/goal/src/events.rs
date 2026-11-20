//! The three `goal/*` rows, and nothing else.
//!
//! Each constructor is the only place a payload shape is written, so the fold in
//! [`crate::fold`] and the writers in the CLI cannot drift apart without one of
//! them failing a test. They take `&Goal` rather than loose numbers on purpose:
//! a caller that has not folded the goal does not know its revision, and a
//! continuation written with a guessed revision is a broken fence.

use forge_core::{Event, EventType};
use serde_json::{Map, Value};

use crate::domain::{Goal, StopReason};

/// `goal/created` — the operator asked for this, and the log now knows.
///
/// The payload carries `revision: 0`: a new goal has advanced zero times. The
/// field is optional on read so a log written before it existed folds to the same
/// zero (see [`crate::fold::fold`]).
pub fn goal_created(session: &str, goal: &Goal) -> Event {
    let payload = serde_json::json!({
        "goalId": goal.id,
        "text": goal.text,
        "maxRounds": goal.max_rounds,
        "revision": goal.revision,
    });
    Event::new(session, EventType::GOAL_CREATED, Some(payload))
}

/// `goal/continued` — the harness opened round `round` by itself.
///
/// Written before the round runs, so a crash during the round leaves the
/// expensive thing counted rather than free. Each continuation advances the
/// revision by exactly one, which is what makes the CAS in
/// `append_continued_if_current` a fence rather than a hint.
pub fn goal_continued(session: &str, goal: &Goal, round: u32) -> Event {
    let payload = serde_json::json!({
        "goalId": goal.id,
        "round": round,
        "revision": goal.revision + 1,
    });
    Event::new(session, EventType::GOAL_CONTINUED, Some(payload))
}

/// `goal/ended` — the goal stopped, with the reason a human would ask for.
///
/// `revision` is written unchanged: ending a goal does not advance it, it records
/// the revision at which it ended.
pub fn goal_ended(session: &str, goal: &Goal, reason: StopReason, note: Option<&str>) -> Event {
    let mut payload = Map::new();
    payload.insert("goalId".into(), Value::String(goal.id.clone()));
    payload.insert("reason".into(), Value::String(reason.as_str().to_string()));
    payload.insert("revision".into(), Value::from(goal.revision));
    if let Some(note) = note {
        payload.insert("note".into(), Value::String(note.to_string()));
    }
    Event::new(session, EventType::GOAL_ENDED, Some(Value::Object(payload)))
}
