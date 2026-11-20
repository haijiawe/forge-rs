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
/// The files the model handed over with `present`, read from the manifest the
/// tool appends to. Private for the same reason as `teams` and `goal_route`.
mod present_route;
pub mod ratelimit;
pub mod server;
/// The subagent tree of one session joined with each child's timing. Private:
/// a wire shape for one route, not part of this crate's boundary.
mod subagent_route;
/// Reads the boards `hearth team run` leaves in the data dir. Private: it is
/// a wire shape for one route, not part of this crate's public boundary.
mod teams;
/// The workspace a session was started in, as a read-only tree: list, read,
/// and the changes git knows about. Private for the same reason as `teams`.
mod workspace_route;

pub use api::{
    CancelToken, DeltaSink, EventWriter, Options, StaticSource, SteeringSource, TurnContext,
    TurnRunner,
};
pub use capabilities::{Entry, Report};
pub use ratelimit::{RateLimit, RateLimiter};
pub use server::{Server, WS_SUBPROTOCOL};
