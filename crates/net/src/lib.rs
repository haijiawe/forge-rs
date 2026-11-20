//! Generic blocking HTTP for the tools that talk to the web.
//!
//! Layer L1. This crate knows URLs, sockets, TLS and response framing; it does
//! **not** know what a provider is, what a session is, or what a tool is. The
//! provider keeps its own SSE shaping and error vocabulary (`hearth-llm`), the
//! tools keep their argument decoding (`hearth-tool`), and both share the
//! transport below.
//!
//! Deliberate omissions: no async (the whole harness is thread-based, and one
//! runtime would be a second concurrency model to reason about), no cookie jar
//! (a tool fetch is one request), no HTTP/2 (a build of h2 plus a second TLS
//! configuration, for a client that makes single requests), no proxy support
//! yet (an explicit gap, listed in `README.md`).

#![forbid(unsafe_code)]

pub mod client;
pub mod html;
pub mod http;
pub mod tls;
pub mod url;

pub use client::{
    read_bounded, Client, ClientOptions, Response, DEFAULT_MAX_BYTES, DEFAULT_TIMEOUT,
    MAX_REDIRECTS,
};
pub use html::to_text;
pub use http::{parse_status, read_head, status_slug, Body, BodyMode, Head};
pub use tls::{connect, parse_pem_certificates, Connection, Roots, ERR_ABORTED};
pub use url::{host_without_port, port_from_authority, Url};
