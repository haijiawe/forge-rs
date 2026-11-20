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
//!
//! # Three layers, on purpose
//!
//! ```text
//!     domain   what a goal is, and the words the log may use
//!     fold     log -> Goal, plus the race fence over that state
//!     runtime  (log, liveness) -> Next, a pure decision a host acts on
//! ```
//!
//! The split is the H3 port of DSH's `goal` + `goal-round-driver` pair. DSH keeps
//! the same information spread across a state machine that reacts to events from
//! inside an async runtime; here the driver is a function, so the whole lifecycle
//! can be driven in a test by pushing events into a `Vec`, and a host cannot
//! spend twice because a callback fired twice. See [`runtime::next`].

#![forbid(unsafe_code)]

pub mod domain;
pub mod events;
pub mod fold;
pub mod runtime;

pub use domain::{
    continuation_prompt, Action, CasOutcome, FenceOutcome, Goal, Reservation, RoundIdentity,
    Status, StopReason, CONTINUATION_PROMPT, DEFAULT_MAX_ROUNDS,
};
pub use events::{goal_continued, goal_created, goal_ended};
pub use fold::{check_in, fence, fold, fold_and_decide, validate, CheckIn};
pub use runtime::{next, reservation_to_open, Next, Snapshot};

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::{Event, EventType};
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
