//! The fixed-window limiter behind `rate_limited` / 429.
//!
//! Layer L3. `POST /api/sessions/{id}/messages` is the only request that makes
//! the server spend model tokens, so it is the only one that is limited: a
//! script in a `while true` loop can otherwise queue an unbounded number of
//! turns behind one session, and every one of them is a bill.
//!
//! Three decisions worth naming:
//!
//! * **Per session, not per peer.** The listener is loopback-only and the token
//!   is per data directory, so a peer key would put two browser tabs on one
//!   budget while still letting one session starve another. A session is the
//!   unit a human thinks in, and the unit whose log grows.
//! * **Fixed window, not sliding.** A sliding window needs a ring of timestamps
//!   or a timer per key. This limiter exists to stop the obvious loop, not to
//!   shape traffic, and a fixed window costs one `Instant` and one `u32`.
//! * **Only accepted attempts are counted.** A request rejected for another
//!   reason (wrong state, missing runner) did no work and must not spend the
//!   caller's budget; that check therefore lives in `accept_message`, after the
//!   state checks and before the first append.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The limit applied to one session.
///
/// `max == 0` disables limiting. That is not a test-only escape hatch: a caller
/// that owns the process and drives it programmatically (the e2e harness, a
/// benchmark) knows its own request rate, and making it sleep would only make
/// the test slower.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub max: u32,
    pub window: Duration,
}

impl RateLimit {
    /// Two turns a second, sustained, per session. A human cannot type that
    /// fast; a script that can is a loop, and the loop is what this catches.
    pub const DEFAULT_MAX: u32 = 120;
    /// One minute, so `Retry-After` is expressed in a number a client can wait.
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

    pub fn new(max: u32, window: Duration) -> Self {
        Self { max, window }
    }

    /// No limit at all; `attempt` always allows.
    pub fn disabled() -> Self {
        Self {
            max: 0,
            window: Self::DEFAULT_WINDOW,
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.max == 0
    }
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            max: Self::DEFAULT_MAX,
            window: Self::DEFAULT_WINDOW,
        }
    }
}

struct WindowState {
    start: Instant,
    count: u32,
}

/// Above this many tracked sessions the map is swept, so a server that has
/// served thousands of sessions over its lifetime does not keep one dead entry
/// per session forever.
const PRUNE_AT: usize = 512;

/// Per-session fixed windows.
pub struct RateLimiter {
    limit: RateLimit,
    windows: Mutex<HashMap<String, WindowState>>,
}

impl RateLimiter {
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub fn limit(&self) -> RateLimit {
        self.limit
    }

    /// Count one attempt against `key`.
    ///
    /// `None` means allowed. `Some(remaining)` means the caller must answer 429
    /// with `Retry-After` derived from `remaining`, and must write nothing.
    ///
    /// `now` is a parameter rather than a call to `Instant::now` so the window
    /// boundary is testable without sleeping for a minute.
    pub fn attempt(&self, key: &str, now: Instant) -> Option<Duration> {
        if self.limit.is_disabled() {
            return None;
        }
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if windows.len() >= PRUNE_AT {
            let limit = self.limit;
            windows.retain(|_, window| Self::remaining(limit, window, now).is_some());
        }

        let window = windows.entry(key.to_string()).or_insert(WindowState {
            start: now,
            count: 0,
        });
        match Self::remaining(self.limit, window, now) {
            // The window has expired (or this is a new key): start a fresh one
            // and charge this attempt to it.
            None => {
                window.start = now;
                window.count = 1;
                None
            }
            Some(_) if window.count < self.limit.max => {
                window.count += 1;
                None
            }
            Some(remaining) => Some(remaining),
        }
    }

    /// How much of the window is left, or `None` when it has expired. An
    /// overflowing `Instant` means "expired": a clock that cannot represent the
    /// end of the window is not a reason to accept unlimited traffic.
    fn remaining(limit: RateLimit, window: &WindowState, now: Instant) -> Option<Duration> {
        let end = window.start.checked_add(limit.window)?;
        if now >= end {
            None
        } else {
            Some(end - now)
        }
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(max: u32, window_secs: u64) -> RateLimiter {
        RateLimiter::new(RateLimit::new(max, Duration::from_secs(window_secs)))
    }

    #[test]
    fn allows_exactly_the_limit_then_refuses() {
        let limiter = limiter(3, 60);
        let now = Instant::now();
        for expected in 0..3 {
            assert_eq!(
                limiter.attempt("s1", now),
                None,
                "attempt {expected} should be allowed"
            );
        }
        let remaining = limiter.attempt("s1", now).expect("fourth is limited");
        assert!(remaining <= Duration::from_secs(60));
        assert!(remaining > Duration::from_secs(59), "got {remaining:?}");
    }

    #[test]
    fn a_fresh_window_resets_the_count() {
        let limiter = limiter(1, 60);
        let start = Instant::now();
        assert_eq!(limiter.attempt("s1", start), None);
        assert!(limiter.attempt("s1", start).is_some());
        let later = start + Duration::from_secs(61);
        assert_eq!(limiter.attempt("s1", later), None, "window should restart");
    }

    #[test]
    fn keys_are_independent() {
        let limiter = limiter(1, 60);
        let now = Instant::now();
        assert_eq!(limiter.attempt("s1", now), None);
        assert_eq!(
            limiter.attempt("s2", now),
            None,
            "each session has a budget"
        );
        assert!(limiter.attempt("s1", now).is_some());
    }

    #[test]
    fn a_disabled_limit_never_refuses() {
        let limiter = RateLimiter::new(RateLimit::disabled());
        let now = Instant::now();
        for _ in 0..10_000 {
            assert_eq!(limiter.attempt("s1", now), None);
        }
    }

    #[test]
    fn a_refused_attempt_does_not_extend_the_window() {
        // The whole point of a fixed window: a client that keeps hammering is
        // refused until the original window ends, not 60s after its last try.
        let limiter = limiter(1, 60);
        let start = Instant::now();
        assert_eq!(limiter.attempt("s1", start), None);
        let mut probe = start;
        for _ in 0..5 {
            probe += Duration::from_secs(10);
            assert!(limiter.attempt("s1", probe).is_some());
        }
        let just_after = start + Duration::from_secs(61);
        assert_eq!(
            limiter.attempt("s1", just_after),
            None,
            "the window still ends 60s after it began"
        );
    }
}
