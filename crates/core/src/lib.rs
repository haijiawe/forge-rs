//! The frozen vocabulary every other crate shares.
//!
//! Three things live here, and nothing else may: the [`event`] envelope, the
//! [`ids`] that name things, and [`json`]'s canonical encoding. They are here
//! because a second definition of any of them is a second truth, and the whole
//! design of this harness rests on there being exactly one.
//!
//! Rules this crate keeps, and that its tests hold it to:
//!
//! * An event is validated at the boundary and never again. Validation checks
//!   the envelope only; payload members are described, not enforced, so a
//!   reader built today can read a log written a year from now.
//! * An unknown `type` is data, not an error. An unknown `v` is an error, not
//!   data. The difference is whether ignoring it can silently change meaning.
//! * Re-encoding a parsed event reproduces its bytes. Golden files are a
//!   contract here, not a convenience.

pub mod error;
pub mod event;
pub mod ids;
pub mod json;

pub use error::{Error, ErrorKind, Result};
pub use event::{Event, EventType};
pub use ids::{new_id, SessionId};
pub use json::canonical;
