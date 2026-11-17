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
    pub status: Status,
}

impl Goal {
    pub fn new(id: impl Into<String>, text: impl Into<String>, max_rounds: u32) -> Goal {
        Goal {
            id: id.into(),
            text: text.into(),
            max_rounds,
            rounds: 0,
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
}

/// The decision the turn loop acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Continue { round: u32 },
    Stop { reason: StopReason },
}

/// `goal/created` — the operator asked for this, and the log now knows.
pub fn goal_created(session: &str, goal: &Goal) -> Event {
    let payload = serde_json::json!({
        "goalId": goal.id,
        "text": goal.text,
        "maxRounds": goal.max_rounds,
    });
    Event::new(session, EventType::GOAL_CREATED, Some(payload))
}

/// `goal/continued` — the harness opened round `round` by itself.
///
/// Written before the round runs, so a crash during the round leaves the
/// expensive thing counted rather than free.
pub fn goal_continued(session: &str, goal: &Goal, round: u32) -> Event {
    let payload = serde_json::json!({
        "goalId": goal.id,
        "round": round,
    });
    Event::new(session, EventType::GOAL_CONTINUED, Some(payload))
}

/// `goal/ended` — the goal stopped, with the reason a human would ask for.
pub fn goal_ended(session: &str, goal: &Goal, reason: StopReason, note: Option<&str>) -> Event {
    let mut payload = Map::new();
    payload.insert("goalId".into(), Value::String(goal.id.clone()));
    payload.insert("reason".into(), Value::String(reason.as_str().to_string()));
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
                current = Some(Goal::new(id, text, max_rounds));
            }
            "goal/continued" => {
                let round = match object.get("round").and_then(Value::as_u64) {
                    Some(round) => (round.min(u32::MAX as u64)) as u32,
                    // A continuation we cannot number is still a continuation:
                    // ignoring it would report a budget larger than was spent.
                    None => continue,
                };
                if let Some(goal) = current.as_mut() {
                    if goal.id == id {
                        goal.rounds = goal.rounds.max(round);
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
                if let Some(goal) = current.as_mut() {
                    if goal.id == id {
                        goal.status = reason.status();
                    }
                }
            }
            _ => {}
        }
    }

    current
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
