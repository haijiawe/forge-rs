---
dir: crates/agent
layer: L2
role: one-turn agent loop and transcript summarizer; owns event ordering and the consumer-owned tool hook
public: [forge-agent]
may_import: [forge-core, forge-llm, forge-session]
must_not: [forge-tool, forge-server, forge-cli]
gate: cargo run -q -p archcheck --offline
---

## 做什么

Port of the forge-v2 one-turn agent loop and the transcript summarizer. `Agent::run` drives one user turn: it re-projects the append-only log through forge-session (invariant I1), emits events in the frozen events-v1 order (envelope step 1-based, payload index 0-based), retries provider failures only before the first text fragment, closes the stream exactly once on cancellation while still writing `turn/end`, and feeds tool-call results back into the model. `ProviderSummarizer` folds an ordered event range into messages and summarizes it with no tools, no retries, no fallback, and thinking markers stripped.

## 公开面

`Agent`, `Options`, `RunResult`, `Cancel`, the `ToolRunner` and `EventLog` traits and `ToolResult` shape (the narrow consumer-owned tool hook — W3's tool crate implements it later), the payload constants and stop/reason helpers, `history` helpers, `err` metadata helpers, and `ProviderSummarizer` / `Summary` / the summarizer error constants.

## 依赖与边界

Depends only on forge-core (events and errors), forge-llm (frozen provider contract), forge-session (store and projection) and serde_json. Never imports forge-tool or any higher layer; tools are supplied through `ToolRunner`. History is always rebuilt from the log — the crate keeps no second copy of conversation truth. No async runtime, no new dependencies, no unsafe.

## 不要做

Do not write the prompt twice when `PromptRecorded` is set; do not retry after a text fragment; do not emit an `error` event for a cancelled turn; do not send tool calls or reasoning to the summarizer's model; do not fall back to invented summary text; do not add a `forge-tool` dependency or edit `Cargo.toml` / `architecture.yaml`.
