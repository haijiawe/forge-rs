# ADR-0003 — the platform tier: what is missing, and in which language

- **Status:** proposed (2026-11-17). Nothing in this document is implemented.
- **Decision owner:** the user. This records the gap inventory, the language
  decisions, and the wave plan; it does not authorise the work.
- **Supersedes nothing.** [08-rust-port.md](08-rust-port.md) (ADR-0002) still
  governs the language of the kernel. This ADR answers the next question: the
  kernel is Rust, *what about the parts we never wrote?*

## 0. The judgement being corrected

ADR-0002 ported a set of frozen surfaces (`docs/contracts/`) rather than the full
feature list of the tree it replaced. Those surfaces were themselves a subset:
M4 was store/import/summarizer, M5 was MCP/skill/subagent, M6 was
sandbox/exec/danger/doctor. **There was never a surface for the platform tier**,
so the platform tier was never written — not deleted, never specified.

That is the honest reading of "为什么能力少了". The rest of this document is
what it would take to specify and build it.

## 1. The gap, measured on this host

Evidence column says how each row was checked. "declared" means a contract or a
constant exists and no code implements it — the worst state, because it looks
done from the outside.

### 1.1 Tool-level gaps (a `Tool` impl, nothing structural)

| Capability | Original DSH | forge-rs today | Evidence |
| --- | --- | --- | --- |
| `web_search` | yes | **missing** | no tool named it; `crates/tool/src/builtin/` has 5 files |
| `web_fetch` | yes | **missing** | same |
| `read_image` | yes (`read_image`, `acp_read_original_image`) | **missing** | `crates/llm/src/contract.rs` `Message.content` is `String`, so no image part can be sent |
| `worktree` | yes (`create_worktree`) | **missing** | no `git worktree` call anywhere |
| `present` (deliver a file) | yes | **missing** | no such tool |
| attachments on `POST /messages` | yes | **declared, dropped** | `crates/server/src/server.rs:1770-1786` validates `kind`/size, then `accept_message` uses only `body.text` |

### 1.2 Runtime gaps (a subsystem, new crate)

| Capability | Original DSH | forge-rs today | Evidence |
| --- | --- | --- | --- |
| scheduled tasks (rrule) | yes, 4 tools | **missing** | no timer, no rrule parser |
| goal (cross-round auto-continuation) | yes, 3 tools | **missing** | no persisted goal, no continuation loop |
| plugin runtime | yes, Cordis services/events/slots | **missing** | `architecture.yaml` has 16 crates, none a loader |
| plugin market | yes, index + install + versions | **missing** | `plugin_manager`/`plugin_market` have no counterpart |
| agent teams (parallel + task board) | yes, native dispatch + `wait_agent` | **partial** | `crates/subagent` is *serial* delegation, no board, no inbox |
| ACP-style addressable compression | yes (compress/decompress/search) | **summary compaction only** | `crates/session/src/compact.rs` folds to one summary; no random access |

### 1.3 Config/content gaps (files, not code)

| Capability | Original DSH | forge-rs today | Evidence |
| --- | --- | --- | --- |
| expert roster | yes, 22 divisions | **missing** | `crates/config` has no expert key |
| expert teams | yes, with a coordinator prompt | **missing** | same |
| agent presets | yes | **missing** | `config.profile` exists but only `"default"` is known (`crates/server/src/server.rs` `defaultProfile`) |
| Office documents | yes, via skills | **missing** | no docx/xlsx/pptx path in `crates/tool` |
| image generation | yes, via MCP | reachable | MCP client exists; nothing to add in-tree |

## 2. Language decisions

### 2.1 The rule

**One language for everything that ships inside the binary.** Rust. The kernel's
standing properties are 7.0 MB, nine direct dependencies, no async runtime, 5 ms
`--version`, 6.3 MB idle — measured in [dsh-comparison.md](dsh-comparison.md).
Every module written in a second compiled language costs three things at once:
a second build chain in CI, a second cross-compile matrix, and an error-boundary
translation layer. None of the gaps below are CPU-bound enough to justify that.

There are exactly three places where another language is the *right* answer, and
none of them put a compiler into our build:

| Where | Language | Why |
| --- | --- | --- |
| Plugin *bodies* | whatever the plugin author uses | the host talks a documented protocol over stdio; see §2.2 |
| Office documents | Python or Node, invoked as an external program by a skill | Rust's docx/xlsx story is immature; the original delegates to libraries too |
| Web UI | TypeScript (already) | `web/`, 35 files, 78 tests |

### 2.2 Decisions, one per gap

| Gap | Decision | Reasoning (and the alternative that lost) |
| --- | --- | --- |
| `web_fetch`, `web_search`, market fetches | **Rust**, new L1 crate `forge-net` | TLS is already in the budget (`rustls`+`ring`+`webpki-roots`). Extract the generic part of `crates/llm/src/openai.rs`'s blocking HTTP client. *Lost:* a separate Go helper — no throughput argument, and it would double the toolchain |
| `web_search` backend | **configurable endpoint**, no built-in scraping | Search needs a service; a keyless HTML scrape breaks silently. Config key `search.endpoint`/`search.key`, absent ⇒ tool refuses with a clear message (fail closed) |
| `read_image` | **Rust**, plus a protocol change | The blocker is `Message.content: String`. Needs a content-parts shape (`text` \| `image_url`) in `crates/llm/src/contract.rs`, the OpenAI request builder, and the event payload |
| `worktree` | **Rust**, shell out to `git` | `crates/tool/src/builtin/shell.rs` already spawns through `forge-exec`, so tree-kill and the sandbox apply for free |
| `present` | **Rust**, small | Writes a manifest the web UI reads; no new dependency |
| attachments (file/image) | **Rust**, finish the declared surface | Either implement or delete the constant; §1.1 shows the current state | 
| `schedule` | **Rust**, new L2 crate `forge-schedule` | rrule is parsing, not compute. Write a **subset** (FREQ/INTERVAL/BYDAY/BYHOUR/BYMINUTE/COUNT/UNTIL) — no dependency. *Lost:* pulling a full rrule crate (new dep, new edge) |
| `goal` | **Rust**, new L2 crate `forge-goal` | A state machine plus a continuation loop; persisted as events, so restart keeps it |
| plugin runtime | **Rust host + out-of-process plugins over stdio JSON-RPC** | The MCP client (`crates/mcp`, L1) already does exactly this, and §2.3 measured the round trip at 80 µs empty / 118 µs for 4 KB against a 1,203 ms model call. A plugin = MCP-shaped server plus lifecycle hooks. Language-agnostic *by construction*, zero new deps, and the sandbox already contains children. *Lost:* wasmtime (adds a large dependency and a second compile step), V8/deno_core (an order of magnitude larger than the whole binary), `cdylib` loading (ABI and version hell, and breaks `panic = "abort"`) |
| plugin market | **Rust**, L2 `forge-market` | HTTP index + tarball + checksums; `ring` is already there for hashing |
| experts, expert teams | **Rust loader** (`forge-experts`, L1) over **Markdown + YAML frontmatter** | Same shape as skills (`crates/skill` reads frontmatter today), so the format is already proven. Prompts are content, not code |
| agent teams | **Rust**, extend `forge-subagent` into `forge-team` | Serial delegation already exists; teams add parallel workers, a shared task board and an inbox |
| Office | **skill + external program**, detected at runtime | Ships no parser. If the program is absent the skill says so (fail closed) instead of half-writing a file. *Lost for now:* pure-Rust `docx-rs`/`umya-spreadsheet` — revisit only if a single-file install becomes a hard requirement |
| ACP-style compression | **Rust**, new L2 `forge-context` | The interesting part is the addressing model (block ids, restore, search) and the cache ledger, not throughput |
| image generation | **MCP**, nothing in-tree | Already possible today |

### 2.3 What the round trip costs (measured)

The obvious objection to a process boundary is latency, so it was measured before
it was chosen. Two Node processes (parent client, child server), 2,000 sequential
JSON-RPC calls, same machine, `process.hrtime.bigint()` around the loop:

| Result payload | Per call |
| --- | --- |
| empty | **80.6 µs** |
| 4 KB | **118.3 µs** |
| 64 KB | **343.7 µs** |

For scale: one model round trip to the endpoint this project measures against is
**1,203 ms**. A plugin call is therefore 0.007% of one model call; a thousand of
them cost 80 ms. Node's JSON path is also the slow end of the range this host will
see — a Rust plugin skips an interpreter start and a parse of its own.

So "which language is fastest" is the wrong question at this boundary: the host
already pays microseconds, and the reason to let the plugin author choose is that
the choice does not show up in the profile. The costs that do show up are
distribution (can it be installed), authoring (can it be written), and containment
(can it be stopped) — and a child process wins all three.

**What this does not settle.** 80 µs is Node-to-Node with no tool work. A plugin
doing real I/O costs whatever the I/O costs. If a future plugin needs to sit in a
hot loop of small calls, the answer is to write it in Rust or batch the calls, not
to move the host.

### 2.4 Rejected outright

- **Go for the scheduler.** No throughput argument, and it re-introduces the
  toolchain we just deleted. The one thing Go bought us was build speed, and a
  scheduler is 200 lines.
- **A second compiled language for "hot" paths.** There are none: the measured
  costs are startup and memory, both already answered.
- **Putting Python inside the binary** (e.g. a bundled interpreter) to make
  Office documents work offline. It turns a 7 MB file into a 40 MB installer and
  a licensing question.
- **Rewriting the web UI in Rust/WASM.** The UI is 265 kB gzipped and its gates
  are green; a WASM rewrite buys nothing measurable.

## 3. Crate plan and the layer graph

New crates, all obeying the existing rule (an edge may point only to a lower or
equal layer), with the `README.md` frontmatter `archcheck` requires:

| Crate | Layer | May import | Purpose |
| --- | --- | --- | --- |
| `forge-net` | L1 | `forge-core` | generic blocking HTTP GET/POST: redirects, size cap, timeouts. `forge-llm` keeps only the SSE/chat shaping |
| `forge-experts` | L1 | — | parse expert/team definitions (Markdown + frontmatter) |
| `forge-schedule` | L2 | `forge-core` | rrule subset, next-occurrence, persistence |
| `forge-goal` | L2 | `forge-core`, `forge-session` | goal state machine, continuation rounds |
| `forge-market` | L2 | `forge-core`, `forge-net`, `ring` | index fetch, install, verify |
| `forge-plugin` | L2 | `forge-core`, `forge-mcp` | plugin host over stdio JSON-RPC; MCP transport reuse |
| `forge-context` | L2 | `forge-core`, `forge-session` | addressable compression blocks, restore, search |
| `forge-team` | L2 | `forge-core`, `forge-session`, `forge-subagent` | parallel workers + shared task board |

`forge-mcp` stays L1 and gains no plugin knowledge: the plugin crate depends on
it, not the other way round. `tools/archcheck` and `architecture.yaml` change in
the same commit that adds each crate — that is the rule, not a suggestion.

Tool additions (`crates/tool/src/builtin/`): `web_fetch`, `web_search`,
`read_image`, `worktree`, `present`. That takes the builtin count from 7 to 12,
and the number is written down in four places that must move together:

- `docs/contracts/cli.md:295` — "Seven tools are registered unconditionally …
  `tools={n}` … is at least seven";
- `crates/tool/README.md:16` — the seven names, spelled out;
- `crates/tool/src/builtin/mod.rs:1` and `:21` — the module doc and `all()`;
- `crates/cli/src/wiring.rs:14` — "the frozen seven names".

## 4. Why the event log does not need a version bump

New subsystems want new event types (`goal/*`, `schedule/*`, `team/*`,
`plugin/*`). Law #2 in `AGENTS.md` already covers this: **an unknown event type
is data — skip and count it; an unknown *version* is an error.** So types are
additive, and a v1 reader that meets a `goal/created` line keeps working.

The obligations that come with it:

1. Add each type to `EventType::ALL` (`crates/core/src/event.rs:50`), which is
   what `validate` consults (`:73`, `:138`) — a type missing from `ALL` is
   rejected as "does not match the v1 envelope", so the addition is testable.
2. Add a conformance case per new type under `tests/conformance/cases/events/`
   (21 cases today: 9 accept, 12 refuse).
3. Never reuse a type name for a different meaning. If a payload shape changes,
   it is a new type.

## 5. Waves

Each wave is independently shippable and independently verifiable. No wave starts
before the previous one's gates are green.

**P1 — tools (smallest useful step, ~1 week).**
`forge-net`; `web_fetch`; `web_search` behind a configured endpoint; `worktree`;
`present`; finish or delete attachments. *Acceptance:* a real fetch of a real URL
through `forge-net` in a test with a local fixture server; `web_search` refuses
with a named reason when unconfigured; `git worktree list` shows the new tree;
`cargo test --workspace` + `check.ps1` green; no new dependency line in
`Cargo.toml`.

**P2 — time and memory (~2 weeks).**
`forge-schedule` + `forge goal` + the continuation loop. *Acceptance:* a
next-occurrence table for a fixed rrule corpus (the rrule subset is the oracle);
a goal survives a restart because it is in the log; a continuation round is
bounded and stops on `blocked`.

**P3 — platform layer (1–2 months).**
`forge-plugin` (protocol doc + lifecycle + a reference plugin written in a
*non-Rust* language, to prove the claim), `forge-market`, `forge-experts`,
`forge-team`. *Acceptance:* **one** complete reference plugin in Node (the language
DSH's plugin authors already use, so its friction is the migration path) plus a
**ten-line Python smoke test** that only proves a second runtime can join the
protocol — two full reference plugins would be two maintenance surfaces bought to
re-prove one property; a market install verifies a checksum and refuses a tampered
tarball; an expert team dispatches two members and writes their results to a shared
board.

**P4 — context and content (optional).**
`forge-context` (addressable compression), Office skills, pure-Rust document
writers if a single-file install becomes a requirement.

Why this is last rather than first, in three lines:

1. **P1–P3 add; P4 changes.** Addressable compression needs block ids, restore and
   search on top of the log, which moves `projection`, `fork` and the transcript's
   rendering semantics. The earlier waves add siblings to the log, not new rules
   for reading it.
2. **The earlier waves change its inputs.** Compression policy is tuned against
   "which sessions grow long", and `schedule`, `goal` and `team` all make sessions
   longer. Optimising today's growth curve against tomorrow's load is how a
   compression budget ends up wrong for the only sessions that need it.
3. **Office is content, not architecture.** It is a skill plus a program probe:
   zero kernel risk, doable at any time. It sits last because it is not urgent,
   not because it is hard.

**Revisit trigger.** If long sessions lose detail before the model's window is
full, `forge-context` moves ahead of P2 — the loss is in the context window, not
in rendering (10,001 events already scroll at 60 fps, measured in `docs/web-ui.md`).

## 6. What this does not buy

- **Plugin ecosystem value.** A runtime is not a market: DSH's advantage is
  installed plugins, not the loader. Shipping P3 gives us the ability to have
  plugins, not plugins.
- **Feature parity with DSH.** P1–P4 close the *capability* list, not the
  *surface* list (desktop shell, xterm terminal, ACP agent protocol, Office
  rendering, image models).
- **A licence.** `Cargo.toml` still says `license = "UNLICENSED"` and there is no
  `LICENSE` file. Nothing here may be redistributed until that changes. This is
  a prerequisite for P3, not a follow-up.

## 7. Risks

| Risk | Why it is real | Mitigation |
| --- | --- | --- |
| The plugin protocol ossifies before anyone writes a second plugin | the MCP shape is a good guess, not a measured fit | ship one non-Rust reference plugin in the same wave; treat its friction as the design input |
| `read_image` breaks the provider contract | `Message.content: String` is frozen in `contract.rs` | additive content-parts variant, keep `content` as the text-only shorthand, one conformance case per shape |
| New event types silently rot | readers skip what they do not know | one conformance case per type, plus a `doctor` line counting unknown types in the active data directory |
| Wave work outruns the gates | 8 new crates is 8 new README + archcheck edges | `archcheck` fails the build, so drift is loud; never batch two crates into one wave item |
| Effort lands on features nobody uses | the gap list came from a feature *list*, not from usage | P1 first (daily tools), then let usage pick between P2 and P3 |