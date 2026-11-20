//! Reading the log back: the state machine, the fence, and the door check.
//!
//! [`fold`] is the only place that decides what a sequence of `goal/*` rows
//! means, and [`fence`] is the only place that decides whether a claim is still
//! live. Both take a plain slice, so both are testable without a session, a
//! store, or a clock (H3).

use forge_core::{Error, ErrorKind, Event, Result};
use serde_json::{Map, Value};

use crate::domain::{
    Action, FenceOutcome, Goal, Reservation, Status, StopReason, DEFAULT_MAX_ROUNDS,
};

/// Fold the current goal out of a session's events.
///
/// The **most recent** `goal/created` wins: a session may finish one goal and
/// start another, and the log keeps both while the fold keeps the live one.
/// Events carrying a different `goalId` are ignored on purpose — an older
/// goal's trailing `goal/ended` arriving after the new goal began must not end
/// the new one. A payload this build cannot read is skipped rather than
/// refused: the log is data, and a line written by a future version must not
/// take the reader down (AGENTS.md rule 2).
///
/// **`revision` is optional and a missing one reads as `0`.** A session written
/// before this field existed has no `revision` on any `goal/*` payload; folding
/// it as zero makes it look like a fresh, never-advanced goal, which is exactly
/// how it behaves (its `rounds` still count, but its CAS fence starts at zero).
/// Revision only ever rises, so it is folded with `max`: a later line cannot pull
/// the count back down, and a stray old-style line in a new log changes nothing.
pub fn fold(events: &[Event]) -> Option<Goal> {
    let mut current: Option<Goal> = None;

    for event in events {
        let payload = match event.payload.as_ref() {
            Some(payload) => payload,
            None => continue,
        };
        let object = match payload.as_object() {
            Some(object) => object,
            None => continue,
        };
        // Every goal event names its goal; one that cannot is unreadable.
        let id = match object.get("goalId").and_then(Value::as_str) {
            Some(id) => id,
            None => continue,
        };

        match event.typ.as_str() {
            "goal/created" => {
                let text = object.get("text").and_then(Value::as_str).unwrap_or("");
                let max_rounds = object
                    .get("maxRounds")
                    .and_then(Value::as_u64)
                    .map(|value| value.min(u32::MAX as u64) as u32)
                    .unwrap_or(DEFAULT_MAX_ROUNDS);
                let mut goal = Goal::new(id, text, max_rounds);
                // Missing revision means zero: an old log starts the fence here.
                goal.revision = payload_revision(object);
                current = Some(goal);
            }
            "goal/continued" => {
                let round = match object.get("round").and_then(Value::as_u64) {
                    Some(round) => (round.min(u32::MAX as u64)) as u32,
                    // A continuation we cannot number is still a continuation:
                    // ignoring it would report a budget larger than was spent.
                    None => continue,
                };
                let revision = payload_revision(object);
                if let Some(goal) = current.as_mut() {
                    if goal.id == id {
                        goal.rounds = goal.rounds.max(round);
                        goal.revision = goal.revision.max(revision);
                    }
                }
            }
            "goal/ended" => {
                let reason = match object.get("reason").and_then(Value::as_str) {
                    Some(text) => StopReason::parse(text),
                    None => None,
                };
                let reason = match reason {
                    Some(reason) => reason,
                    None => continue,
                };
                let revision = payload_revision(object);
                if let Some(goal) = current.as_mut() {
                    if goal.id == id {
                        goal.status = reason.status();
                        goal.revision = goal.revision.max(revision);
                    }
                }
            }
            _ => {}
        }
    }

    current
}

/// The `revision` a `goal/*` payload declares, or `0` when it is absent.
///
/// Absent is not corrupt: it is a log from before the field existed, and zero is
/// both its true value and the value a fresh goal has today.
fn payload_revision(object: &Map<String, Value>) -> u64 {
    object.get("revision").and_then(Value::as_u64).unwrap_or(0)
}

/// Re-check a [`Reservation`] against a log. This is the race fence: it does not
/// take a lock, it re-reads the one truth (the log) and compares. Two entries can
/// both try; the one whose revision no longer matches folds to
/// [`FenceOutcome::Stale`] and must not write another round.
pub fn fence(events: &[Event], reservation: &Reservation) -> FenceOutcome {
    let Some(goal) = fold(events) else {
        return FenceOutcome::Stale {
            revision: 0,
            rounds: 0,
        };
    };
    if goal.status != Status::Active {
        return FenceOutcome::Ended {
            status: goal.status,
        };
    }
    if goal.id != reservation.goal_id
        || goal.revision != reservation.revision
        || goal.rounds != reservation.round
    {
        return FenceOutcome::Stale {
            revision: goal.revision,
            rounds: goal.rounds,
        };
    }
    FenceOutcome::Valid
}

/// The decision a host acts on when it re-checks a claim mid-round.
///
/// This is [`fence`] turned into words a caller can match on without knowing the
/// vocabulary of `FenceOutcome`. It exists so the turn loop does not have to
/// decide what `Stale { revision, rounds }` means for its own control flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckIn {
    /// The claim is still the live round.
    KeepGoing,
    /// Stop this round; another entry owns the goal now, or it ended.
    Stop { reason: StopReason, detail: String },
}

/// Re-check a claim and say, in the loop's terms, whether to keep spending.
pub fn check_in(events: &[Event], reservation: &Reservation) -> CheckIn {
    match fence(events, reservation) {
        FenceOutcome::Valid => CheckIn::KeepGoing,
        FenceOutcome::Ended { status } => CheckIn::Stop {
            reason: status_reason(status),
            detail: format!(
                "goal {} is {} at revision {}",
                reservation.goal_id,
                status.as_str(),
                reservation.revision
            ),
        },
        FenceOutcome::Stale { revision, rounds } => CheckIn::Stop {
            reason: StopReason::Cancelled,
            detail: format!(
                "goal {} was advanced by another entry (revision {revision}, {rounds} round(s) opened); this round's claim on revision {} is stale",
                reservation.goal_id, reservation.revision
            ),
        },
    }
}

/// The reason a terminal status corresponds to.
fn status_reason(status: Status) -> StopReason {
    match status {
        Status::Complete => StopReason::Complete,
        Status::Blocked => StopReason::Blocked,
        Status::Cancelled => StopReason::Cancelled,
        Status::Exhausted => StopReason::Exhausted,
        // Active is not terminal; reaching here means the caller folded a status
        // that stopped being active between two reads. Treat it as cancelled —
        // the conservative word for "someone else moved this".
        Status::Active => StopReason::Cancelled,
    }
}

/// Reject a goal that could never make sense, before it is written.
///
/// An empty goal or a zero budget is a request the loop can only fail at
/// quietly, so it is refused at the door with a message that says which.
pub fn validate(goal: &Goal) -> Result<()> {
    if goal.id.trim().is_empty() {
        return Err(Error::new(ErrorKind::Invalid, "goal: empty goal id"));
    }
    if goal.text.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            "goal: empty goal text — say what to keep working on",
        ));
    }
    if goal.max_rounds == 0 {
        return Err(Error::new(
            ErrorKind::Invalid,
            "goal: maxRounds must be at least 1, or the goal can never run",
        ));
    }
    Ok(())
}

/// What [`fold`] says, plus the decision, for a reader that wants both.
///
/// The pair is returned rather than only the action because "no goal at all" and
/// "a goal that is exhausted" are different situations that both look like
/// [`Action::Stop`]. A caller that prints "nothing to do" needs to tell them
/// apart, and one that merges them into a single `Stop` would be inventing a
/// goal that exists.
pub fn fold_and_decide(events: &[Event]) -> (Option<Goal>, Option<Action>) {
    match fold(events) {
        Some(goal) => {
            let action = goal.next_action();
            (Some(goal), Some(action))
        }
        None => (None, None),
    }
}
