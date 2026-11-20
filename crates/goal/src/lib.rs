//! A goal is the operator saying "keep going until this is done".
//!
//! The state lives in the session log, not in memory: a harness that restarts
//! mid-goal must be able to answer "was I still working on something, and how
//! many rounds in?" by reading the same file it already replays. So the three
//! `goal/*` events are the state, and [`fold`] is the state machine. Nothing
//! here holds a lock, a thread or a timer — a goal that could not be justified
//! by the log would be a goal that survives a crash while its evidence does not.
//!
//! The continuation round itself belongs to whoever owns the turn loop; this
//! crate only decides whether there should be another one.

#![forbid(unsafe_code)]

use forge_core::{Error, ErrorKind, Event, EventType, Result};
use serde_json::{Map, Value};

/// How many rounds a goal gets when the operator does not say.
///
/// A continuation loop with no bound is a loop that spends money until someone
/// notices. Ten is enough for "finish the refactor" and small enough that the
/// worst case is a bill the operator can read in one line.
pub const DEFAULT_MAX_ROUNDS: u32 = 10;

/// The template fed back to the model to open a continuation round.
///
/// A constant, not a formatted string, so that two runs over the same log
/// produce byte-identical prompts: a prompt built from the clock or from a
/// counter would make the log insufficient to explain itself.
pub const CONTINUATION_PROMPT: &str = "Continue toward the goal: {goal}. Take the next concrete step. If the goal is finished, say so and stop; if something is blocking you, say what it is and stop.";

/// Where a goal is, as a reader of the log sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Complete,
    Blocked,
    Cancelled,
    Exhausted,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Complete => "complete",
            Status::Blocked => "blocked",
            Status::Cancelled => "cancelled",
            Status::Exhausted => "exhausted",
        }
    }
}

/// Why a goal stopped. The wire words are also the `goal/ended` payload values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Complete,
    Blocked,
    Cancelled,
    Exhausted,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Complete => "complete",
            StopReason::Blocked => "blocked",
            StopReason::Cancelled => "cancelled",
            StopReason::Exhausted => "exhausted",
        }
    }

    /// The status this reason leaves behind. The mapping is total, so a reader
    /// never has to guess what "ended because blocked" means for a goal.
    pub fn status(self) -> Status {
        match self {
            StopReason::Complete => Status::Complete,
            StopReason::Blocked => Status::Blocked,
            StopReason::Cancelled => Status::Cancelled,
            StopReason::Exhausted => Status::Exhausted,
        }
    }

    /// The inverse of `as_str`, for readers that take the reason out of an event
    /// payload or a tool's `meta`. Unknown text is `None` rather than a guess: a
    /// verdict nobody can name must not close a goal.
    pub fn parse(text: &str) -> Option<StopReason> {
        match text {
            "complete" => Some(StopReason::Complete),
            "blocked" => Some(StopReason::Blocked),
            "cancelled" => Some(StopReason::Cancelled),
            "exhausted" => Some(StopReason::Exhausted),
            _ => None,
        }
    }
}

/// One goal, as folded from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub id: String,
    pub text: String,
    pub max_rounds: u32,
    /// How many continuation rounds have been opened. Counted in the log, so a
    /// restart resumes at the same number instead of starting the budget over.
    pub rounds: u32,
    /// How many times the goal has advanced. Counted in the log beside `rounds`,
    /// so two entries that both folded the same goal see the same number and can
    /// agree on which of them may write the next round. A log written before
    /// this field existed folds to `0`, which is exactly what a fresh goal is, so
    /// an old session resumes without a rewrite. See [`fold`].
    pub revision: u64,
    pub status: Status,
}

impl Goal {
    pub fn new(id: impl Into<String>, text: impl Into<String>, max_rounds: u32) -> Goal {
        Goal {
            id: id.into(),
            text: text.into(),
            max_rounds,
            rounds: 0,
            revision: 0,
            status: Status::Active,
        }
    }

    pub fn is_open(&self) -> bool {
        self.status == Status::Active
    }

    /// Should the loop open another round, and which one?
    ///
    /// The bound is checked here rather than by the caller so that "a
    /// continuation round is bounded" is a property of this crate, testable
    /// without a turn loop.
    pub fn next_action(&self) -> Action {
        match self.status {
            Status::Active => {
                // `rounds` counts the continuation rounds the log has opened, so
                // the turns spent so far is one more than that (the first turn
                // came with `goal/created`). `max_rounds` is a budget of turns,
                // not of continuations: with max 2 the goal gets two turns, and
                // the second is spent by the first continuation. Comparing
                // `rounds` against `max_rounds` instead would spend one turn
                // more than the operator paid for.
                if self.rounds + 1 >= self.max_rounds {
                    Action::Stop {
                        reason: StopReason::Exhausted,
                    }
                } else {
                    Action::Continue {
                        round: self.rounds + 1,
                    }
                }
            }
            Status::Complete => Action::Stop {
                reason: StopReason::Complete,
            },
            Status::Blocked => Action::Stop {
                reason: StopReason::Blocked,
            },
            Status::Cancelled => Action::Stop {
                reason: StopReason::Cancelled,
            },
            Status::Exhausted => Action::Stop {
                reason: StopReason::Exhausted,
            },
        }
    }

    /// The identity of the round that [`Goal::next_action`] would open, or
    /// `None` when the goal has stopped.
    ///
    /// This is the same decision as `next_action`, plus the `revision` the log
    /// must still show for this caller to be the one that opens the round. A
    /// writer that skips the check is a writer that lets two entries continue the
    /// same goal at once; the log then counts two rounds for one turn of work.
    pub fn next_action_identity(&self) -> Option<RoundIdentity> {
        match self.next_action() {
            Action::Continue { round } => Some(RoundIdentity {
                goal_id: self.id.clone(),
                revision: self.revision,
                round,
            }),
            Action::Stop { .. } => None,
        }
    }
}

/// Which round of which revision of which goal a writer is about to open.
///
/// The `revision` is the fence: a continuation may only be written when the
/// log's latest revision still equals the one the writer folded. See
/// [`CasOutcome`] for the result of that comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundIdentity {
    pub goal_id: String,
    pub revision: u64,
    pub round: u32,
}

/// The result of a compare-and-set append of `goal/continued`.
///
/// `Written` carries the new revision the log now shows; `Stale` means another
/// entry advanced the goal first and this one must not write, so it returns the
/// revision it expected and the one it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasOutcome {
    Written(u64),
    Stale { expected: u64, found: u64 },
}

/// A claim on one round, as the entry that opened it held it: "I am running
/// round `round` of goal `goal_id` at revision `revision`". A runner carries this
/// and re-checks it at each checkpoint so a round that was superseded (another
/// entry opened the next round) or whose goal ended stops instead of spending
/// another model call for work nobody is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub goal_id: String,
    pub revision: u64,
    pub round: u32,
}

/// What re-checking a [`Reservation`] against the log says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceOutcome {
    /// Still the live round: keep going.
    Valid,
    /// The goal is no longer active (complete/blocked/cancelled/exhausted).
    Ended { status: Status },
    /// Another entry advanced the goal, or the reservation refers to no goal at
    /// all. `revision`/`rounds` are the numbers the log shows now.
    Stale { revision: u64, rounds: u32 },
}

/// Re-check a [`Reservation`] against a log. This is the race fence: it does not
/// take a lock, it re-reads the one truth (the log) and compares. Two entries can
/// both try; the one whose revision no longer matches folds to [`FenceOutcome::Stale`]
/// and must not write another round.
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

/// The decision the turn loop acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Continue { round: u32 },
    Stop { reason: StopReason },
}

/// `goal/created` — the operator asked for this, and the log now knows.
///
/// The payload carries `revision: 0`: a new goal has advanced zero times. The
/// field is optional on read so a log written before it existed folds to the same
/// zero (see [`fold`]).
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

/// The text to feed back to the model for the next round.
pub fn continuation_prompt(goal: &Goal) -> String {
    CONTINUATION_PROMPT.replace("{goal}", &goal.text)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn revision_starts_at_zero_and_rises_by_one_per_continuation() {
        let goal = Goal::new("g1", "keep the build green", 4);
        assert_eq!(goal.revision, 0, "a fresh goal has advanced zero times");

        let created = goal_created("s", &goal);
        let folded = fold(std::slice::from_ref(&created)).expect("created folds");
        assert_eq!(folded.revision, 0);

        // The writer folded revision 0, so its continuation claims revision 1.
        let first = goal_continued("s", &folded, 1);
        let folded = fold(&[created.clone(), first]).expect("one continuation folds");
        assert_eq!(folded.revision, 1, "one continuation is revision 1");
        assert_eq!(folded.rounds, 1);

        // The next writer must fold 1 and claim 2 — never 1 again.
        let second = goal_continued("s", &folded, 2);
        let folded = fold(&[created, second]).expect("two continuations fold");
        assert_eq!(folded.revision, 2);
        assert_eq!(folded.rounds, 2);
    }

    #[test]
    fn missing_revision_reads_as_zero() {
        // A log from before the field existed: no `revision` on any payload.
        let created = Event::new(
            "s",
            EventType::GOAL_CREATED,
            Some(json!({ "goalId": "g1", "text": "old log", "maxRounds": 3 })),
        );
        let continued = Event::new(
            "s",
            EventType::GOAL_CONTINUED,
            Some(json!({ "goalId": "g1", "round": 1 })),
        );
        let folded = fold(&[created, continued]).expect("old log folds");
        assert_eq!(folded.revision, 0, "absent revision is 0, not an error");
        assert_eq!(folded.rounds, 1, "rounds still count without revision");
    }

    #[test]
    fn revision_never_folds_backwards() {
        // A stray old-style ended line (no revision) after a continued(rev 5)
        // must not pull the fence back down to zero.
        let created = Event::new(
            "s",
            EventType::GOAL_CREATED,
            Some(json!({ "goalId": "g1", "text": "x", "maxRounds": 9, "revision": 0 })),
        );
        let continued = Event::new(
            "s",
            EventType::GOAL_CONTINUED,
            Some(json!({ "goalId": "g1", "round": 5, "revision": 5 })),
        );
        let ended = Event::new(
            "s",
            EventType::GOAL_ENDED,
            Some(json!({ "goalId": "g1", "reason": "complete" })),
        );
        let folded = fold(&[created, continued, ended]).expect("folds");
        assert_eq!(folded.revision, 5, "max keeps the highest revision seen");
    }

    #[test]
    fn next_identity_carries_the_current_revision() {
        let mut goal = Goal::new("g1", "do the thing", 5);
        goal.revision = 7;
        let identity = goal
            .next_action_identity()
            .expect("active goal opens a round");
        assert_eq!(
            identity,
            RoundIdentity {
                goal_id: "g1".to_string(),
                revision: 7,
                round: 1,
            }
        );

        goal.status = Status::Complete;
        assert_eq!(
            goal.next_action_identity(),
            None,
            "a stopped goal opens no round"
        );
    }

    #[test]
    fn cancelled_reason_folds_to_cancelled_status() {
        let goal = Goal::new("g1", "x", 3);
        let created = goal_created("s", &goal);
        let ended = goal_ended("s", &goal, StopReason::Cancelled, Some("paused"));
        let folded = fold(&[created, ended]).expect("folds");
        assert_eq!(folded.status, Status::Cancelled);
    }
}
