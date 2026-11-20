//! What a host does after a turn ends: drain the queue that was waiting.
//!
//! A session can accumulate work while a turn is running. A background subagent
//! settles and leaves its answer as a `next-turn` message; another process writes
//! a `next-step` steer; a second POST is accepted while the first turn is still
//! going (the frozen HTTP contract says so). None of that is lost — it is in the
//! log — but nothing acts on it until something asks. Before this module, that
//! something was the operator typing again, which meant a delegated child's answer
//! could sit unread for hours.
//!
//! DSH solves this with a resident agent: a long-lived object per session that
//! owns an inbox and a phase, woken by events. This tree has no resident object,
//! because the loop is a function of the log (I1) and a process that is not
//! running has no inbox to keep. What it has is [`forge_agent::Agent::kick`],
//! which claims queued messages and runs each as one turn — so "resident" here
//! means a loop that keeps asking, not an object that stays alive.
//!
//! The bound matters. Draining is work that reaches a model, so a queue that keeps
//! refilling itself (a child settling inside a drain, which settles another) must
//! stop somewhere instead of running until the operator notices. [`MAX_DRAINED_TURNS`]
//! is that stop, and [`Drained::capped`] says whether it was hit, so a caller can
//! report "there is still work queued" rather than pretending the queue is empty.

use forge_agent::Agent;

/// How many turns one drain may run before it stops and says so.
///
/// Eight is twice the default subagent parallelism: enough to pick up a fan-out's
/// worth of settlements in one pass, small enough that a loop bug costs eight
/// model calls rather than an unbounded number.
pub const MAX_DRAINED_TURNS: usize = 8;

// A guard against someone raising this to "unbounded" later: every unit here is a
// model call, so the ceiling is part of the contract rather than a tuning knob.
const _: () = assert!(
    MAX_DRAINED_TURNS >= 2 && MAX_DRAINED_TURNS <= 16,
    "MAX_DRAINED_TURNS must stay between 2 and 16: each unit is one model call"
);

/// What a drain did.
///
/// Not Clone: orge_core::Error is not, and a drain result is read once.
#[derive(Debug, Default)]
pub struct Drained {
    /// How many turns the drain ran. Zero means the queue was already empty.
    pub turns: usize,
    /// Whether the cap stopped it with work still queued.
    pub capped: bool,
    /// The first failure, if one stopped the drain early. A failed turn's error is
    /// already in the log; this is for the host to report, not to re-record.
    pub error: Option<forge_core::Error>,
}

impl Drained {
    /// Whether anything happened.
    pub fn is_empty(&self) -> bool {
        self.turns == 0 && self.error.is_none()
    }
}

/// Run every message queued on `agent`, up to [`MAX_DRAINED_TURNS`].
///
/// Callers pass the agent that just finished a turn: its provider, tools, hooks
/// and cancel token are the ones this session is already using, so a drained turn
/// is indistinguishable from one the operator asked for — except that nobody is
/// waiting on the deltas.
///
/// `pending` is checked before each kick so a queue that is empty costs one log
/// read and no model call. That is the common case: most turns leave nothing
/// behind.
pub fn drain(agent: &Agent) -> Drained {
    drain_with(
        || agent.pending().map(|inbox| !inbox.is_empty()),
        || {
            let (runs, err) = agent.kick();
            (runs.len(), err)
        },
    )
}

/// [`drain`] with the two effects passed in.
///
/// The loop is where the interesting decisions are — how many turns, what stops
/// it, what a failure means — and none of them need a model to test. Pulling the
/// effects out keeps the cap and the error paths testable without a scripted
/// provider that would only ever answer "pong".
pub fn drain_with<P, K>(mut pending: P, mut kick: K) -> Drained
where
    P: FnMut() -> forge_core::Result<bool>,
    K: FnMut() -> (usize, Option<forge_core::Error>),
{
    let mut out = Drained::default();
    loop {
        match pending() {
            Ok(false) => return out,
            // A log that cannot be read is not an empty queue; stop and say so
            // rather than silently dropping what is there.
            Err(err) => {
                out.error = Some(err);
                return out;
            }
            Ok(true) => {}
        }
        if out.turns >= MAX_DRAINED_TURNS {
            out.capped = true;
            return out;
        }
        let (ran, err) = kick();
        out.turns += ran;
        if let Some(err) = err {
            out.error = Some(err);
            return out;
        }
        if ran == 0 {
            // `kick` claimed nothing after the pending check said otherwise: the
            // queue was emptied by another entry between the two reads. Nothing
            // to do, and nothing wrong.
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::{Error, ErrorKind};
    use std::cell::Cell;

    // The cap itself is guarded by the `const _: () = assert!(..)` above: a test
    // here would only re-state a compile-time constant, which clippy rejects.

    #[test]
    fn an_empty_result_says_nothing_happened() {
        assert!(Drained::default().is_empty());
        assert!(!Drained {
            turns: 1,
            ..Drained::default()
        }
        .is_empty());
    }

    #[test]
    fn an_empty_queue_costs_no_turn() {
        let out = drain_with(|| Ok(false), || panic!("kick must not run"));
        assert_eq!(out.turns, 0);
        assert!(!out.capped);
        assert!(out.error.is_none());
    }

    #[test]
    fn a_queue_that_keeps_refilling_stops_at_the_cap_and_says_so() {
        // The pathological case: every kick "finds" more work. Without a cap this
        // is an unbounded number of model calls. A single kick can overshoot the
        // cap — it drains whatever is queued — so the assertion is "at or just
        // past it", and the cap is still a real ceiling: one more batch.
        let out = drain_with(|| Ok(true), || (3, None));
        assert!(
            out.turns >= MAX_DRAINED_TURNS,
            "the cap must be reached, got {}",
            out.turns
        );
        assert!(
            out.turns <= MAX_DRAINED_TURNS + 2,
            "one batch may overshoot the cap, not more: {}",
            out.turns
        );
        assert!(out.capped, "hitting the cap must be reported, not hidden");
        assert!(out.error.is_none());
    }

    #[test]
    fn a_finished_queue_stops_without_capping() {
        let left = Cell::new(2);
        let out = drain_with(
            || Ok(left.get() > 0),
            || {
                left.set(left.get() - 1);
                (1, None)
            },
        );
        assert_eq!(out.turns, 2);
        assert!(!out.capped);
        assert!(out.error.is_none());
    }

    #[test]
    fn a_failed_turn_stops_the_drain_and_is_reported() {
        let out = drain_with(
            || Ok(true),
            || {
                (
                    1,
                    Some(Error::new(ErrorKind::Refused, "the provider gave up")),
                )
            },
        );
        assert_eq!(out.turns, 1, "the turn that failed still happened");
        assert!(out.error.is_some());
        assert!(!out.capped);
    }

    #[test]
    fn an_unreadable_log_is_not_an_empty_queue() {
        let out = drain_with(
            || Err(Error::new(ErrorKind::Io, "cannot read the session log")),
            || panic!("kick must not run when the queue cannot be read"),
        );
        assert_eq!(out.turns, 0);
        assert!(
            out.error.is_some(),
            "a read failure must surface instead of looking like 'nothing queued'"
        );
    }

    #[test]
    fn a_race_that_empties_the_queue_between_two_reads_ends_quietly() {
        let out = drain_with(|| Ok(true), || (0, None));
        assert_eq!(out.turns, 0);
        assert!(out.error.is_none());
        assert!(!out.capped);
    }
}
