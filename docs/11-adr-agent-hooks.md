# ADR-0004 — the agent hook seam: a deliberate, bounded rewrite point

- **Status:** accepted (2026-11-20) by the user ("w5可以搞"). This is a **position
  change**, not a gap fill: it introduces the one rewrite point the tree
  deliberately did not have. Implementation is planned in
  [10-agent-parity.md](10-agent-parity.md) §W5.
- **Decision owner:** the user.
- **Supersedes part of** the "no hidden rewriter" claim in
  [AGENTS.md](../AGENTS.md), [08-rust-port.md](08-rust-port.md) and
  [agent-gaps.md](agent-gaps.md) §C — **narrowly**, see §2.

## 1. The claim this ADR changes, and the claim that stays

Until now the tree made one promise about the loop: *the same log always yields
the same request, because there is no layer that can rewrite it.* That promise is
why `waterfall` had 0 hits in `crates/agent`, and why
[agent-gaps.md](agent-gaps.md) listed it as **deliberately not built**.

The user has decided to buy back the capability, because a harness whose loop
cannot be extended by a plugin is a harness a plugin cannot reach into. This ADR
records the change and the fence around it.

**What changes:** the loop gains *two* named hook points, and only these two, and
only when a hook is installed.

**What stays true:** with no hook installed, the loop is byte-for-byte the loop it
was — same events, same request, same ordering. Determinism is preserved as the
*default*; rewrites are opt-in and always logged. "No hidden rewriter" becomes
"no *unlogged* rewriter".

## 2. The three seams (two rewrite, one decision)

| Seam | When | What a hook may change | What it may not change |
|---|---|---|---|
| `pre-step` | before each provider call, after the inbox/steering is folded into history | the message list handed to the provider | the model id, the session, the tool specs, or any already-written event |
| `request` | immediately after `Request` is built, before it is sent | the request's messages and tool descriptions | the model id (selecting a model is its own operation, not a rewrite), and nothing that would forge a `llm/call` counter |
| `request-error` | after a request attempt fails and the transport policy would give up | nothing durable — it answers only "retry or not" (`HookOutcome::Retry`) | anything on the log; it writes no `hook/rewrite` line because it rewrites nothing |

The two rewrite seams are the useful subset; `request-error` is the one case where
the transport policy cannot know better than a caller — a hook may know an error is
transient when its code does not say so. It is a decision, not a rewrite, so it is
deliberately not logged as one; the retry it forces shows up as the next request.

No other phase is added. The closed set lives in one place
(`crates/agent`'s hook module) and a phase outside it is refused by name, the way
`crates/plugin/src/protocol.rs` already refuses `"during_breakfast"`.

## 3. Why Rust, inside `forge-agent`, not a new crate

The seam is ~two `Option<Arc<dyn Fn>>` fields and two call sites. A crate for it
would be a boundary with one interior edge and no second implementer. It lives in
`forge-agent`, the crate that owns the loop. The plugin bridge that *fills* the
seam lives in `crates/cli` (it needs `forge-plugin`), exactly as the MCP bridge
does — `forge-agent` must not learn that plugins or Cordis exist.

## 4. Why this is safe to ship

- **Opt-in and inert by default.** `Options.hooks` is `None` unless a caller sets
  it. The four existing call sites (`chat.rs`, `web.rs`, worker runner, tests) set
  nothing, so they behave identically; a golden test pins that.
- **Logged, not silent.** Every rewrite that changes the message list writes a
  `hook/rewrite` event (a new type; see 10-agent-parity §W5) naming the phase, the
  plugin, and how many messages changed. A log with no such event proves the loop
  did not rewrite anything — that is the auditable replacement for "there is no
  rewriter".
- **Bounded.** A hook sees messages and tool descriptions, never the log, never
  the model id, never the ability to append events. It cannot fake a `llm/call`,
  cannot select a model, cannot write.
- **Failure is loud.** A hook that errors, or returns a non-object, aborts the
  step with `hook_failed` rather than silently sending the un-rewritten request —
  a rewrite that half-happened is worse than one that did not.

## 5. What this does not buy

- **Not full Cordis parity.** DSH's `agent/turn-stopping` (serial) is not added;
  the three phases above are the useful subset, and the generic `emit`/`serial`
  dispatch is not built (a phase outside the closed set is refused by name). More
  can be argued for separately, each as its own ADR line.
- **Not determinism loss by default.** The default loop is unchanged. The cost is
  paid only by sessions whose operator enables a plugin that claims a hook.
- **Not a security boundary.** A hook runs in-process with the operator's trust,
  like an approver. It is not sandboxed; the plugin *process* is, but the decision
  it returns is trusted, exactly as an MCP tool's output is.
