//! A plugin host: a child process that speaks JSON-RPC 2.0 over stdio.
//!
//! Why a process and not a library: the plugin language is the plugin author's
//! choice, and the only way to honour that is a process boundary. The measured
//! cost of a round trip is 80.6 µs for an empty result and 343.7 µs for 64 KB
//! (2,000 Node-to-Node round trips, `docs/09-platform-tier.md` §2.3) — against a
//! model round trip of about 1,200 ms, a plugin call is 0.007% of a turn. So the
//! boundary is cheap, and the alternative (`wasmtime`, or `cdylib` plugins
//! compiled per language) buys nothing this host needs while adding a toolchain
//! to every plugin author.
//!
//! A plugin is *not* an MCP server. It can also contribute prompts, hooks that
//! run before or after a turn, and decorators that wrap another tool's result —
//! and it has a lifecycle the host drives (`load`, `unload`) rather than a
//! session it joins. That is why `forge-mcp` is imported for its log sink only
//! and the transport here is this crate's own: the shapes differ, and forcing
//! one through the other would put plugin knowledge into an L1 MCP client.
//!
//! The protocol is documented in `PROTOCOL.md` next to this file; a plugin
//! written from that document alone must work, and the reference plugin under
//! `tests/reference-node/` is the proof that it can be.

#![forbid(unsafe_code)]

pub mod connection;
pub mod error;
pub mod host;
pub mod manifest;
pub mod protocol;

pub use connection::{Connection, Stopped, CLOSE_GRACE};
pub use error::{Error, Kind, Result};
pub use host::{Host, Loaded};
pub use manifest::{is_valid_name, Manifest, Runtime};
pub use protocol::{
    is_hook_phase, Capabilities, Handshake, Message, PromptSpec, Request, ToolOutput, ToolSpec,
    DEFAULT_MAX_BYTES, DEFAULT_TIMEOUT_MS, HOOK_PHASES, PROTOCOL_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_protocol_version_is_the_one_the_document_states() {
        // PROTOCOL.md §1 says version 1; a host that speaks something else and
        // does not say so is the failure this constant exists to prevent.
        assert_eq!(PROTOCOL_VERSION, 1);
        assert_eq!(DEFAULT_MAX_BYTES, 4 << 20);
        assert_eq!(DEFAULT_TIMEOUT_MS, 10_000);
        // The two hook phases the document lists, in the order it lists them.
        assert_eq!(HOOK_PHASES, &["before_turn", "after_tool"]);
    }
}
