//! `hearth-bench`: a six-metric performance harness with a per-platform
//! baseline gate. The library exposes the pure pieces (baseline documents,
//! statistics, gate decisions) so they can be unit-tested without running the
//! slow measurements; `src/main.rs` wires them to the real harness.

pub mod args;
pub mod baseline;
pub mod gate;
pub mod metrics;
pub mod platform;
pub mod stats;
pub mod time;
