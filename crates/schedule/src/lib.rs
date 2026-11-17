//! Recurrence rules and the scheduled-task registry (layer L2).
//!
//! A scheduled task is a small thing with a long life: an rrule and a prompt,
//! stored where no session log can take it down, waiting for a caller to ask
//! "what is due now". That is the whole surface, and it is deliberately narrow
//! because the interesting decisions are made by whoever delivers the prompt.
//!
//! Two properties are load-bearing:
//!
//! - **Determinism.** Nothing here reads a clock. Every function that needs
//!   "now" takes `now_ms`, Unix epoch milliseconds in UTC, so a recurrence can
//!   be tested against a table of hand-computed instants instead of against
//!   whatever day the test suite happens to run (see `tests/oracle.rs`).
//! - **Bounded work.** A rule can name a set of instants that is effectively
//!   empty. Every search has a step budget and answers `None` or a short list
//!   rather than spinning, because a scheduler that hangs is worse than one
//!   that admits it found nothing.
//!
//! The calendar arithmetic (`civil`) is hand-written for the same reason the
//! dependency list is empty: `days_from_civil` is thirty lines that can be
//! checked against a table, and a date library would be a much larger surface
//! for the same answer.
//!
//! ```no_run
//! use forge_schedule::{parse_rule, Registry, Task};
//!
//! # fn main() -> forge_core::Result<()> {
//! let registry = Registry::open("schedules");
//! let rule = parse_rule("FREQ=WEEKLY;BYDAY=MO,WE,FR;BYHOUR=9;BYMINUTE=0")?;
//! let task = Task::new("FREQ=WEEKLY;BYDAY=MO,WE,FR;BYHOUR=9;BYMINUTE=0", "stand up", 1_794_906_000_000, 1_794_906_000_000);
//! registry.add(task)?;
//!
//! // The caller owns the clock; the registry only compares against it.
//! for due in registry.due(1_794_906_000_000)? {
//!     println!("{} is due at {}", due.task.id, due.at_ms);
//!     registry.mark_run(&due.task.id, due.at_ms)?;
//! }
//! # let _ = rule;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod civil;
mod registry;
mod rule;
mod task;

pub use civil::from_civil;
pub use registry::Registry;
pub use rule::{parse_rule, Freq, Rule, Weekday};
pub use task::{Due, Task};
