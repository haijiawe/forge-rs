---
dir: crates/protocol
layer: L2
role: the wire types shared by the server and the UI
public: []
may_import: [forge-core]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

The JSON shapes of the local API: session summaries, event pages, approval payloads, WebSocket frames, the error envelope. Ported in W4 from `forge-go/internal/protocol`.

## 公开面

Nothing yet — placeholder until W4. The TypeScript types for the web UI will be generated from these definitions, so a field added here becomes a field in the UI.

## 依赖与边界

Do not depend on the server. This crate is the meeting point of two implementations; if it imports one of them, the other cannot use it.

## 不要做

- Do not write code here before the wave that owns it. A placeholder that
  compiles is honest; a half-implementation that looks finished is not.
- Do not add a dependency outside the workspace budget in Cargo.toml.