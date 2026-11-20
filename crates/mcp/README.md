---
dir: crates/mcp
layer: L1
role: external MCP client — stdio and Streamable HTTP transports, protocol 2025-06-18
public:
  - PROTOCOL_VERSION
  - Transport
  - Server
  - ToolDef
  - CallResult
  - Content
  - ServerInfo
  - Logger
  - NoopLogger
  - Error
  - Dial
  - dial
  - Client
  - ErrServerGone
  - ErrProtocolVersion
may_import: []
must_not:
  - import any hearth crate, including hearth-core
  - put stderr bytes into the JSON-RPC stream
  - handshake inside Dial
  - use the real network in tests
gate: cargo test -p hearth-mcp --offline
---

## 做什么

A blocking, thread-based MCP client for both transports in the M5 surface:
JSON-RPC 2.0 over a child process (one JSON object per line) and Streamable
HTTP (POST a JSON document, read a JSON document or an SSE stream). `Dial`
only connects; `initialize` does the handshake. A missing process/session and
an unsupported protocol version are deliberately different failures.

## 公开面

`Transport::{Stdio,Http}` and `Server` describe the peer; `dial`/`Dial` take a
`&Server` and an `Arc<dyn Logger>` and return `Box<dyn Client>`. `Client` has
`initialize`, `tools`, `call`, `close`, and `clone_handle` (a second owner of
the same connection so `close` can unblock an in-flight call). Results use
`ServerInfo`, `ToolDef`, `CallResult`, and `Content`. `Error` is typed;
`ErrServerGone` and `ErrProtocolVersion` are the sentinel strings. `Logger`
receives stderr and diagnostics — it never receives protocol bytes.

## 依赖与边界

No hearth-internal dependency, not even `hearth-core`: MCP is an external
protocol and must not be bound to internal event or error types. External
dependencies are `serde`/`serde_json` for wire JSON and
`rustls`/`rustls-pki-types`/`webpki-roots` for HTTPS, all from the workspace
budget. HTTP/1.1 framing, chunked decoding, SSE, and the tiny local test server
are implemented here because no HTTP client crate is in the budget.

The boundary is "wire client, not policy": this crate does not choose which
tools are allowed, does not persist anything, and does not know about sessions
or approvals.

## 不要做

- Do not add a tokio runtime or async API. The transports are blocking and
  thread-per-connection.
- Do not make stderr part of the protocol stream. It goes to the `Logger`
  only; a malformed stdout line kills the connection instead.
- Do not make the per-call timeout sticky. A timed-out call leaves the
  connection usable; a dead server does not.
- Do not add a test that dials a real host. HTTP tests use plain TCP on
  127.0.0.1, and TLS is checked only by building the root store.
