//! The wire types shared by the server and the browser UI.
//!
//! Layer L2. Frozen from `archive/forge-go/internal/protocol/protocol.go`; the Go
//! oracle and `docs/contracts/api.md` win on any disagreement. `protocol.rs`
//! holds the shapes; this file only declares the module.

#![forbid(unsafe_op_in_unsafe_fn)]

mod protocol;

pub use protocol::*;
