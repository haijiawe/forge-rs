---
dir: crates/agent
layer: L2
role: one-turn agent loop and transcript summarizer; owns event ordering and the consumer-owned tool hook
public: [hearth-agent]
may_import: [hearth-core, hearth-llm, hearth-session]
must_not: [hearth-tool, hearth-server, hearth-cli]
gate: cargo run -q -p archcheck --offline
---

## 做什么

Port of the forge-go one-turn agent loop and the transcript summarizer. `Agent::run` drives one user turn: it re-projects the append-only log through hearth-session (invariant I1), emits events in the frozen events-v1 order (envelope step 1-based, payload index 0-based), retries provider failures only before the first text fragment, closes the stream exactly once on cancellation while still writing `turn/end`, and feeds tool-call results back into the model. `ProviderSummarizer` folds an ordered event range into messages and summarizes it with no tools, no retries, no fallback, and thinking markers stripped.

## 公开面

`Agent`, `Options`, `RunResult`, `Cancel`, the `ToolRunner` and `EventLog` traits and `ToolResult` shape (the narrow consumer-owned tool hook — W3's tool crate implements it later), the payload constants and stop/reason helpers, `history` helpers, `err` metadata helpers, the two extension surfaces — `Hooks`/`Hook`/`HookCall`/`HookOutcome`/`HOOK_PHASES` (rewrite seams, see docs/11-adr-agent-hooks.md) and `Dispatch`/`EventView`/`Listener`/`Problem` (observation only: `emit` per appended row, `serial` at the turn boundary), the resident surface — `Agent::pending`/`enqueue`/`kick`/`when_idle`/`run_maintenance`/`phase` over the durable queues in `inbox` (`Inbox`/`Msg`/`Target`), whose rows are `agent/inbox` and whose maintenance windows are `agent/status` — and `ProviderSummarizer` / `Summary` / the summarizer error constants.

## 依赖与边界

Depends only on hearth-core (events and errors), hearth-llm (frozen provider contract), hearth-session (store and projection) and serde_json. Never imports hearth-tool or any higher layer; tools are supplied through `ToolRunner`. History is always rebuilt from the log — the crate keeps no second copy of conversation truth. No async runtime, no new dependencies, no unsafe.

## 不要做

Do not keep a queue in memory — a prompt that was accepted must survive the process, so it goes in the log; do not write `agent/status` for an ordinary turn, because two diagnostic rows per turn serve no reader; Do not write the prompt twice when `PromptRecorded` is set; do not retry after a text fragment; do not emit an `error` event for a cancelled turn; do not send tool calls or reasoning to the summarizer's model; do not fall back to invented summary text; do not let an observer contradict a rewriter (a listener must never change what the loop sends — that is `Hooks`, and it is recorded); do not add a `hearth-tool` dependency or edit `Cargo.toml` / `architecture.yaml`.
