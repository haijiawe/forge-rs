---
dir: crates/llm
layer: L2
role: provider contract and the OpenAI-compatible SSE client
public: []
may_import: [forge-core]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

The model adapter: a `Provider` trait that yields deltas, and an OpenAI-compatible SSE implementation that speaks `/chat/completions`. Ported in W2 from `forge-v2/internal/llm`.

## 公开面

Nothing yet — the crate is a placeholder until W2. The planned surface is `Provider`, `Request`, `Message`, `ToolSpec`, `Delta`, `Stream`, and the `openai` client behind `FORGE_BASE_URL`/`FORGE_API_KEY`.

## 依赖与边界

Do not import `forge-session`: the provider must not know that a log exists, or replaying a stream becomes impossible to unit test. Do not add an async runtime; the stream is read on the calling thread.

## 不要做

- Do not write code here before the wave that owns it. A placeholder that
  compiles is honest; a half-implementation that looks finished is not.
- Do not add a dependency outside the workspace budget in Cargo.toml.