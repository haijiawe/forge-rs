---
dir: crates/llm
layer: L2
role: provider contract and the OpenAI-compatible SSE client
public: [Provider, Stream, Request, Message, ToolSpec, Delta, Usage, Aggregator, ProviderError, Client, Options, OpenAiStream, Canceller]
may_import: [hearth-core]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

The model adapter: a `Provider` trait that yields deltas, and an OpenAI-compatible SSE implementation that speaks `/chat/completions`, ported from `forge-go/internal/llm/openai`. The provider uses blocking sockets on the calling thread — no async runtime. Plain `http://` uses `std::net::TcpStream`; `https://` uses the workspace rustls stack with the pinned `webpki-roots` CA bundle.

### Private CA

`Options { ca_file: Some(path), .. }` adds the certificates in a PEM file to the pinned bundle, so an endpoint fronted by a private CA verifies without disabling public trust. The private roots are **added on top of** `webpki-roots`; the public bundle is never replaced. A missing, unreadable, or malformed CA file is reported as a typed `ProviderError` whose message names the file but never prints its contents (a bundle can sit beside a private key). The config file exposes the same knob as the optional `caFile` key.

## 公开面

The frozen `contract.rs` surface (`Provider`, `Stream`, `Request`, `Message`, `ToolSpec`, `Delta`, `Usage`, `Aggregator`, `ProviderError`) plus the OpenAI adapter: `openai::Client`, `Options`, `openai::OpenAiStream`, `openai::Canceller`, `openai::provider_error`, and the shared error-code constants. `Client::stream_open` returns the concrete stream so callers can obtain a `Canceller` and close a blocked reader from another thread.

## 依赖与边界

Do not import `hearth-session`: the provider must not know that a log exists, or replaying a stream becomes impossible to unit test. Do not add an async runtime; the stream is read on the calling thread.

## 不要做

- Do not add a dependency outside the workspace budget in Cargo.toml.
- Do not silently switch to async or add an SSE/HTTP client crate: the wire code is intentionally std + workspace-TLS only.
- Do not let an API key reach an error message or a clipped body.