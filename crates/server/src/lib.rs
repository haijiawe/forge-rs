//! HTTP and WebSocket over the append-only session log.
//!
//! Layer L3. The public boundary between this crate and the runner is frozen in
//! [`api`]; W4 implements the server behind it. The Go oracle is
//! `forge-go/internal/server` and the wire contract is `docs/contracts/api.md`.

#![forbid(unsafe_op_in_unsafe_fn)]
// The frozen `api` boundary defines the runner factory as an inline
// `Option<Arc<dyn Fn(&str) -> Arc<dyn TurnRunner> + Send + Sync>>`. Reshaping
// that type into an alias would change the public contract, so allow Clippy's
// style lint at the crate root instead of touching the frozen file.
#![allow(clippy::type_complexity)]

pub mod api;
pub mod capabilities;
/// One session's goal, folded from its log for `GET /api/sessions/{id}/goal`.
/// Private for the same reason as `teams`: a wire shape for one route, not
/// part of this crate's boundary.
mod goal_route;
pub mod ratelimit;
pub mod server;
/// Reads the boards `forge team run` leaves in the data dir. Private: it is
/// a wire shape for one route, not part of this crate's public boundary.
mod teams;

pub use api::{
    CancelToken, DeltaSink, EventWriter, Options, StaticSource, SteeringSource, TurnContext,
    TurnRunner,
};
pub use capabilities::{Entry, Report};
pub use ratelimit::{RateLimit, RateLimiter};
pub use server::{Server, WS_SUBPROTOCOL};
