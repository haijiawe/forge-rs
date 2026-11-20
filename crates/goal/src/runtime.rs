//! The round driver as a pure function.
//!
//! DSH's `goal-round-driver` is an async, stateful listener: it holds a
//! `DriverState` per agent, subscribes to a dozen events, and decides inside its
//! callbacks. That is what a plugin runtime forces. Here the same decision is a
//! function of two inputs — the log and the host's own liveness — with no state,
//! no timer and no futures:
//!
//! ```text
//!     next(Snapshot { events, claim, running }) -> Next
//! ```
//!
//! Two things follow, and both matter for the comparison the operator asked for.
//! A test can drive a whole goal lifecycle by writing events into a `Vec`, and a
//! host cannot decide to keep spending because a callback fired twice — the
//! decision is recomputed from the log every time it is asked.
//!
//! The fence is still here, not dropped: [`Next::Running`] is only reachable when
//! the claim re-checks [`FenceOutcome::Valid`] against the same slice the
//! decision was made from, so "the log says round 3" and "I am allowed to run
//! round 3" are answered by one read.

use forge_core::Event;

use crate::domain::{continuation_prompt, Action, Goal, Reservation, RoundIdentity, StopReason};
use crate::fold::{self, CheckIn};

/// Everything the decision needs. Both inputs are borrowed, so asking twice is
/// free and asking from two threads needs no coordination.
pub struct Snapshot<'a> {
    /// The session log, as folded by the caller.
    pub events: &'a [Event],
    /// The claim the host holds, if a round is in flight under one.
    pub claim: Option<&'a Reservation>,
    /// Whether the host is already running a turn for this session. A goal round
    /// is a turn, so starting one while another runs is a double spend, and the
    /// decision says so instead of assuming the caller checked.
    pub running: bool,
}

/// What the host should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// This log has never held a goal.
    NoGoal,
    /// A goal exists and has stopped. `reason` is the word to write if the host
    /// is the one closing it; a goal already ended in the log needs nothing.
    Ended { goal: Goal, reason: StopReason },
    /// The goal is open, the claim is live, and a round is under way.
    Running { identity: RoundIdentity },
    /// The goal is open, but another entry advanced it (or closed it) after this
    /// claim was taken. Step aside; do not write.
    Superseded { goal: Goal, detail: String },
    /// The goal is open and unclaimed, but this host is mid-turn.
    Busy { identity: RoundIdentity },
    /// Open this round: write `goal/continued` under the identity, then run
    /// `prompt` as the next user turn.
    Open {
        identity: RoundIdentity,
        prompt: String,
    },
}

/// Decide what to do about the goal in `snapshot`.
///
/// The order of the checks is the contract:
///
/// 1. no goal → [`Next::NoGoal`] (not `Ended`: nothing to close)
/// 2. goal not active → [`Next::Ended`]
/// 3. a claim is held → re-fence it; live means [`Next::Running`], stale means
///    [`Next::Superseded`], and a goal that ended under the claim means
///    [`Next::Ended`]
/// 4. the host is busy → [`Next::Busy`]
/// 5. the budget allows another round → [`Next::Open`]
/// 6. otherwise → [`Next::Ended`] with [`StopReason::Exhausted`]
pub fn next(snapshot: Snapshot<'_>) -> Next {
    let Some(goal) = fold::fold(snapshot.events) else {
        return Next::NoGoal;
    };
    if !goal.is_open() {
        return Next::Ended {
            reason: stop_reason(&goal),
            goal,
        };
    }

    if let Some(claim) = snapshot.claim {
        match fold::check_in(snapshot.events, claim) {
            CheckIn::KeepGoing => {
                return Next::Running {
                    identity: RoundIdentity {
                        goal_id: claim.goal_id.clone(),
                        revision: claim.revision,
                        round: claim.round,
                    },
                };
            }
            CheckIn::Stop { reason, detail } => {
                // A stale claim means another entry owns the goal: the goal is
                // still open, so this is not the place to close it.
                if goal.is_open() && reason == StopReason::Cancelled {
                    return Next::Superseded { goal, detail };
                }
                return Next::Ended { goal, reason };
            }
        }
    }

    let identity = match goal.next_action_identity() {
        Some(identity) => identity,
        None => {
            return Next::Ended {
                reason: stop_reason(&goal),
                goal,
            };
        }
    };
    if snapshot.running {
        return Next::Busy { identity };
    }
    Next::Open {
        prompt: continuation_prompt(&goal),
        identity,
    }
}

/// The word that describes why a stopped goal is stopped.
fn stop_reason(goal: &Goal) -> StopReason {
    match goal.next_action() {
        Action::Stop { reason } => reason,
        // `next_action` only returns Continue for an active goal, and this
        // function is only called for one that is not. Reaching here would mean
        // the two disagree, and the conservative word for that is not "complete".
        Action::Continue { .. } => StopReason::Cancelled,
    }
}

/// The claim an [`Next::Open`] would carry, or `None` when there is nothing to
/// open. A host that wants the reservation before acting can ask for it without
/// re-deriving the identity.
pub fn reservation_to_open(snapshot: Snapshot<'_>) -> Option<Reservation> {
    match next(snapshot) {
        Next::Open { identity, .. } => Some(Reservation::of(&identity)),
        _ => None,
    }
}
