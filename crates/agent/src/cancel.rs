//! A minimal shared cancellation token.
//!
//! The frozen `forge_llm` contract is synchronous: one turn owns one thread and
//! `Stream::close` is the cancellation door. This token is the consumer-owned
//! half of that design — the HTTP layer (or a test) sets it, the agent checks
//! it between deltas and closes the stream exactly once.
//!
//! It carries a **cause** because "the turn was cancelled" is not an answer.
//! Seven different paths stop a turn — the operator, a parent session shutting
//! down, a wall-clock deadline, a policy hook, an owner being disposed — and a
//! log that only says `cancelled` makes them indistinguishable after the fact.
//! DSH keeps this in memory; here it is part of the token so the loop can write
//! it down.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

/// Why a turn was stopped. The first cause wins: a second `cancel` arriving
/// while the turn unwinds does not describe the reason it is unwinding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cause {
    /// Nobody said why. A bare `cancel()` reaches this, which is honest: the
    /// token can only report what its caller told it.
    #[default]
    Unknown,
    /// The operator: Ctrl-C in a terminal, the stop button in the browser, or
    /// an explicit `POST /api/sessions/{id}/cancel`.
    User,
    /// A parent session is going away, so its delegations go with it.
    Parent,
    /// A deadline the harness set, not one the operator asked for.
    Timeout,
    /// A policy or extension refused to let the turn continue.
    Hook,
    /// The owner of the loop was dropped while the turn was still running.
    Disposed,
}

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::Unknown => "unknown",
            Cause::User => "user",
            Cause::Parent => "parent",
            Cause::Timeout => "timeout",
            Cause::Hook => "hook",
            Cause::Disposed => "disposed",
        }
    }

    fn code(self) -> u8 {
        match self {
            Cause::Unknown => 0,
            Cause::User => 1,
            Cause::Parent => 2,
            Cause::Timeout => 3,
            Cause::Hook => 4,
            Cause::Disposed => 5,
        }
    }

    fn from_code(code: u8) -> Cause {
        match code {
            1 => Cause::User,
            2 => Cause::Parent,
            3 => Cause::Timeout,
            4 => Cause::Hook,
            5 => Cause::Disposed,
            _ => Cause::Unknown,
        }
    }
}

/// A cheap, cloneable "stop now" flag plus the reason it was raised.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    cause: Arc<AtomicU8>,
}

impl Cancel {
    /// Stop the turn without naming a reason.
    ///
    /// Kept because most callers genuinely do not know one, and because every
    /// existing call site compiles unchanged. Prefer [`Cancel::cancel_with`]
    /// wherever the reason *is* known: the difference between "the operator
    /// stopped this" and "the owner was disposed" is the difference between a
    /// decision and a bug.
    pub fn cancel(&self) {
        self.cancel_with(Cause::Unknown);
    }

    /// Stop the turn and record why. A cause already recorded stays: the first
    /// reason is the reason.
    pub fn cancel_with(&self, cause: Cause) {
        let _ = self.cause.compare_exchange(
            Cause::Unknown.code(),
            cause.code(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Make this token live again, forgetting the cause.
    ///
    /// A resident host serves many turns from one token: the operator presses
    /// stop in turn three, and turn four must not begin already cancelled. The
    /// cause is cleared first, so a cancel arriving between the two stores is the
    /// one that wins rather than being overwritten by the reset.
    pub fn reset(&self) {
        self.cause.store(Cause::Unknown.code(), Ordering::SeqCst);
        self.flag.store(false, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// The recorded cause, or `None` when the turn was never cancelled.
    pub fn cause(&self) -> Option<Cause> {
        if !self.is_cancelled() {
            return None;
        }
        Some(Cause::from_code(self.cause.load(Ordering::SeqCst)))
    }

    /// The cause as `turn/end.cancelledBy` spells it. The member is free-form
    /// (the recovery path writes `cli:recovery`), so readers must treat an
    /// unrecognised value as data rather than an error.
    pub fn cause_str(&self) -> Option<&'static str> {
        self.cause().map(Cause::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::{Cancel, Cause};

    #[test]
    fn a_fresh_token_has_no_cause() {
        let cancel = Cancel::default();
        assert!(!cancel.is_cancelled());
        assert_eq!(cancel.cause(), None);
        assert_eq!(cancel.cause_str(), None);
    }

    #[test]
    fn a_named_cancellation_reports_its_cause() {
        let cancel = Cancel::default();
        cancel.cancel_with(Cause::User);
        assert!(cancel.is_cancelled());
        assert_eq!(cancel.cause(), Some(Cause::User));
        assert_eq!(cancel.cause_str(), Some("user"));
    }

    #[test]
    fn a_bare_cancellation_reports_unknown_not_nothing() {
        // The token was raised; refusing to name why is `unknown`, not silence.
        let cancel = Cancel::default();
        cancel.cancel();
        assert_eq!(cancel.cause(), Some(Cause::Unknown));
        assert_eq!(cancel.cause_str(), Some("unknown"));
    }

    #[test]
    fn the_first_cause_wins() {
        let cancel = Cancel::default();
        cancel.cancel_with(Cause::User);
        // A disposed owner noticing afterwards does not rewrite history.
        cancel.cancel_with(Cause::Disposed);
        assert_eq!(cancel.cause(), Some(Cause::User));
    }

    #[test]
    fn every_cause_round_trips_through_its_code() {
        for cause in [
            Cause::Unknown,
            Cause::User,
            Cause::Parent,
            Cause::Timeout,
            Cause::Hook,
            Cause::Disposed,
        ] {
            assert_eq!(Cause::from_code(cause.code()), cause);
            assert!(!cause.as_str().is_empty());
        }
    }

    #[test]
    fn clones_share_one_flag() {
        let cancel = Cancel::default();
        let other = cancel.clone();
        other.cancel_with(Cause::Parent);
        assert!(cancel.is_cancelled());
        assert_eq!(cancel.cause(), Some(Cause::Parent));
    }
}
