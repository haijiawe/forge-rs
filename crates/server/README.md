---
dir: crates/server
layer: L3
role: HTTP/1.1 and WebSocket over the session log
public: []
may_import: [forge-core, forge-session, forge-protocol]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

The local service: REST routes from `docs/contracts/api.md` and the WebSocket event stream. Ported in W4 from `forge-v2/internal/server` (which hand-wrote an RFC 6455 subset on the standard library).

## 公开面

Nothing yet — placeholder until W4. The acceptance is the 34-case HTTP corpus in `tests/conformance/cases/http/`, which the Go implementation already passes.

## 依赖与边界

Do not put agent logic here. The server reads the log, appends intake records, and hands turns to a `TurnRunner`; it must not own a provider or a tool registry.

## 不要做

- Do not write code here before the wave that owns it. A placeholder that
  compiles is honest; a half-implementation that looks finished is not.
- Do not add a dependency outside the workspace budget in Cargo.toml.