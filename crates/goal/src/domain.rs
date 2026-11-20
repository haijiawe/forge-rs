//! The vocabulary: what a goal is, and what the log is allowed to say about it.
//!
//! Everything here is plain data with no I/O and no clock in it, so the same
//! values can be folded from a log, printed by the CLI, and compared in a test
//! without a session in sight. Keeping the vocabulary separate from the fold and
//! from the decision is what makes the decision testable on a hand-written
//! `Vec<Event>` — which is the whole point of the split (H3).

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

/// Why a goal stopped. The five words a reader of `goal/ended` can see.
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

    /// The status this reason leaves the goal in.
    pub fn status(self) -> Status {
        match self {
            StopReason::Complete => Status::Complete,
            StopReason::Blocked => Status::Blocked,
            StopReason::Cancelled => Status::Cancelled,
            StopReason::Exhausted => Status::Exhausted,
        }
    }

    /// Read a `goal/ended.reason` back. Unknown text is `None`, never a guess:
    /// a future reason this build does not know must not be read as "complete".
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
    /// an old session resumes without a rewrite. See [`crate::fold`].
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

impl Reservation {
    /// The claim a round would carry, from the identity a writer just folded.
    pub fn of(identity: &RoundIdentity) -> Reservation {
        Reservation {
            goal_id: identity.goal_id.clone(),
            revision: identity.revision,
            round: identity.round,
        }
    }
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

/// The decision the turn loop acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Continue { round: u32 },
    Stop { reason: StopReason },
}

/// The text to feed back to the model for the next round.
pub fn continuation_prompt(goal: &Goal) -> String {
    CONTINUATION_PROMPT.replace("{goal}", &goal.text)
}
