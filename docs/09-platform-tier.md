# ADR-0003 — the platform tier: what is missing, and in which language

- **Status:** proposed (2026-11-17); waves P1–P3 plus one follow-up wave are
  delivered, and each wave's status line below says what was measured and what was
  not. P4 is not started.
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
| attachments on `POST /messages` | yes | **declared, worse than dropped** | the server validates `kind`/size, then writes the **whole base64 body into the `user/message` event**: every log reader pays for it, `GET /events` returns it, a fork copies it verbatim, and an append-only log can never forget it — while nothing projects it, so no model ever saw a byte. Fixed in P1: bodies go to `<dataDir>/blobs/`, the event keeps `{kind,id,mediaType,name,bytes,path}` |

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
| `read_image` | **Rust**, done in P4 | `Message` gained `parts: Vec<ContentPart>` (`serde(skip)`, so the frozen contract JSON is byte-identical), the OpenAI builder emits `{"type":"image_url"}` with a `data:` URI, and `read_image` returns the bytes through `Result.parts` — never through the log, which keeps `meta.imagePath`/`mediaType`/`bytes` and re-reads the file on later turns. Measured end to end: the second request body contains `data:image/png;base64,…`, the 21-line log contains no `iVBOR`, and deleting the file makes the next turn report no image |
| `worktree` | **Rust**, shell out to `git` | `crates/tool/src/builtin/shell.rs` already spawns through `forge-exec`, so tree-kill and the sandbox apply for free |
| `present` | **Rust**, small | Writes a manifest the web UI reads; no new dependency |
| attachments (file/image) | **Rust**, done in P1 | Bodies live in `<dataDir>/blobs/` (`crates/session/src/blobs.rs`), the event keeps a reference, and `GET /api/sessions/{id}/attachments/{id}` serves the bytes. Pixels reached the model in P4: the same `Attachment` list is read back through `crates/cli/src/attachments.rs` and handed to `forge_llm` as `ContentPart::Image` | 
| `schedule` | **Rust**, new L2 crate `forge-schedule` | rrule is parsing, not compute. Write a **subset** (FREQ/INTERVAL/BYDAY/BYHOUR/BYMINUTE/COUNT/UNTIL) — no dependency. *Lost:* pulling a full rrule crate (new dep, new edge) |
| `goal` | **Rust**, new L2 crate `forge-goal` | A state machine plus a continuation loop; persisted as events, so restart keeps it |
| plugin runtime | **Rust host + out-of-process plugins over stdio JSON-RPC** | The MCP client (`crates/mcp`, L1) already does exactly this, and §2.3 measured the round trip on this stack at 37 µs empty / 56 µs for 4 KiB against a 1,203 ms model call. A plugin = MCP-shaped server plus lifecycle hooks. Language-agnostic *by construction*, zero new deps, and the sandbox already contains children. *Lost:* see §2.4 — the embedded-engine options were re-priced with a real build, and the size argument against them turned out to be wrong |
| plugin market | **Rust**, L2 `forge-market` | HTTP index + tarball + checksums; `ring` is already there for hashing |
| experts, expert teams | **Rust loader** (`forge-experts`, L1) over **Markdown + YAML frontmatter** | Same shape as skills (`crates/skill` reads frontmatter today), so the format is already proven. Prompts are content, not code |
| agent teams | **Rust**, extend `forge-subagent` into `forge-team` | Serial delegation already exists; teams add parallel workers, a shared task board and an inbox |
| Office | **skill + external program**, detected at runtime | Ships no parser. If the program is absent the skill says so (fail closed) instead of half-writing a file. *Lost for now:* pure-Rust `docx-rs`/`umya-spreadsheet` — revisit only if a single-file install becomes a hard requirement |
| ACP-style compression | **Rust**, new L2 `forge-context` | The interesting part is the addressing model (block ids, restore, search) and the cache ledger, not throughput |
| image generation | **MCP**, nothing in-tree | Already possible today |

### 2.3 What the round trip costs (measured on this stack)

The obvious objection to a process boundary is latency, so it was measured before
it was chosen — and then measured again properly, because the first measurement
was a proxy.

**Superseded:** the original numbers here (80.6 µs empty / 118.3 µs at 4 KB /
343.7 µs at 64 KB) came from two *Node* processes talking to each other. That
measures Node's pipes, not this host's. The `durationMs` field on a tool result is
no substitute either: `crates/tool/src/executor.rs:74` records `as_millis()`, so
every sub-millisecond call reads as 0 or 1.

The real measurement is `crates/plugin/examples/ipc_cost.rs`: the Rust host
starts one Node plugin, then makes 2,000 sequential `tools/call` round trips per
payload size after discarding 100 warm-up calls, timed with `Instant`. The
in-process rows are the control — the same `serde_json` encode/decode with no
boundary — so the pipe rows read as *floor + boundary*.

```text
cargo run --release -p forge-plugin --example ipc_cost --offline
```

Windows, node v24.21.0, two consecutive runs (p50 / p95, µs):

| Measurement | Run 1 | Run 2 |
| --- | --- | --- |
| pipe: empty result | 37.5 / 71.2 | 36.3 / 71.8 |
| pipe: 4 KiB result | 56.0 / 96.1 | 51.7 / 92.8 |
| pipe: 64 KiB result | 230.7 / 400.0 | 240.8 / 428.2 |
| in-process: empty result | 1.2 / 2.1 | 1.1 / 1.2 |
| in-process: 4 KiB result | 2.8 / 5.8 | 2.8 / 3.5 |
| in-process: 64 KiB result | 24.3 / 59.9 | 24.1 / 61.0 |

Two conclusions, and they point in opposite directions:

1. **Relative to an in-process call, the boundary is expensive.** ~31× on an empty
   result, ~20× at 4 KiB, ~9.5× at 64 KiB. Anyone claiming a process boundary is
   "as fast as a function call" is wrong, and the ratio gets worse as payloads
   get smaller.
2. **Relative to what a turn actually spends, it is invisible.** One model round
   trip on this project measured **1,203 ms** = 1,203,000 µs. A 37 µs plugin call
   is **0.003%** of that. A thousand of them cost 37 ms — still 3% of one model
   call.

The second is the one that decides, because a turn is one model call plus a
handful of tool calls, not a hot loop of IPC.

**The cost that does matter is startup, not the call.** Spawn + handshake measured
**44.9 ms** and **36.5 ms** on the two runs — roughly **1,000× a single call**.
`node --version` alone is 17.8 ms p50 on this host, so about half of that is the
interpreter's own startup and the rest is module load plus our handshake. This is
why `chat` holds one `Host` for the whole turn and `Host::load` is the only thing
that spawns: a design that started a child per call would turn a 1,000-call turn
from 37 ms into 45 seconds. **Keeping the child alive is the decision that
protects performance here; the transport choice is not.**

It also makes "write it in Rust if you want it fast" a measurable claim — and the
measurement corrected it. The first version of this section said a native plugin
would be faster by "roughly an order of magnitude". That was an inference from
`node --version` costing 17.8 ms, and it was **too strong by about 2×**. With
`crates/plugin/src/bin/forge-plugin-native.rs` built and loaded through
`runtime: "exec"`, three runs give:

| | Node plugin | Native plugin |
| --- | --- | --- |
| spawn + handshake, p50 | 45.6 / 44.0 / 43.2 ms | **9.8 / 8.5 / 8.8 ms** |
| empty result, min | 30.3 / 24.6 / 25.3 µs | **15.7 / 15.8 / 15.8 µs** |
| 4 KiB result, min | 33.8 / 32.0 / 32.2 µs | 23.3 / 23.6 / 20.5 µs |
| 64 KiB result, min | 106.6 / 100.3 / 107.9 µs | 72.0 / 75.1 / 74.1 µs |

So native startup is about **5×** faster, not 10×, and the per-call floor is
roughly halved with a visibly tighter distribution (no JIT, no GC). The `min`
column is the honest comparator for per-call cost: Node's p50 wandered between
36 µs and 63 µs across five runs while its min stayed near 25 µs, so a ratio
computed from p50 alone would have been noise.

**And it still does not decide anything.** 9.8 ms is 0.8% of one model call;
45 ms is 3.8%. Both are paid *once per plugin per turn*. The largest performance
difference between plugin languages is therefore under 4% of a single model
round trip — which is why §2.6 decides the plugin language on distribution and
authoring instead.

**What this does not settle.** These are round trips with no tool work. A plugin
doing real I/O costs whatever the I/O costs. If a future plugin needs a hot loop
of small calls, the answer is to batch the calls or write that plugin natively —
not to move the host.

So "which language is fastest" is the wrong question at this boundary. The costs
that show up in a profile are distribution (can it be installed), authoring (can
it be written), and containment (can it be stopped) — and a child process wins all
three.

### 2.4 Rejected, and what each rejection is actually worth

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
- **`cdylib` plugin loading.** ABI and version hell, and it breaks
  `panic = "abort"`. Not re-measured; the objection is structural, not a size.

#### Embedded engines: re-priced with a real build

The first version of this document rejected embedded engines partly on size
("adds a large dependency", "an order of magnitude larger than the whole
binary"). **That was never measured, and for QuickJS it is wrong by about 30×.**
`rquickjs 0.14` is in the offline cache, so it was built in a scratch crate
outside this workspace with the same release profile (`lto = "thin"`,
`codegen-units = 1`, `strip`, `panic = "abort"`):

| | measured |
| --- | --- |
| plain hello binary | 122,368 B |
| same, embedding QuickJS | 1,197,056 B |
| **delta** | **1,074,688 B (1.02 MB)** |
| one-time build cost | 13 s |
| in-process `eval` + JSON, p50 | **3.0 µs** (the pipe is 37 µs) |

So the size objection is withdrawn: 1 MB on an 8.5 MB binary is not a reason to
reject anything, and the in-process call is ~12× faster than the pipe (which
§2.3 already showed is irrelevant next to a 1,203 ms model call).

**What actually disqualifies it is the API surface.** Probing what a plugin
author can reach with no host bindings:

```
JSON object   Math object   Promise function   async/await Promise
console undefined   fetch undefined   require undefined
process undefined   setTimeout undefined   TextEncoder undefined
```

It provides the *syntax* and removes the *value*. An agent writing a plugin that
fetches a URL writes `fetch(...)` and it works on Node 18+; on QuickJS it is
`fetch is not defined` until someone binds it by hand. Every capability —
timers, encoding, filesystem, sockets — becomes host API we would have to design,
implement, document and keep stable. And containment moves from the OS (kill a
child) into our code (QuickJS memory limits and interrupt handlers, wired
correctly).

That is a **different product**: a scripting sandbox with a host-defined API,
not a plugin host that can use an existing ecosystem. It is a reasonable thing to
build someday for untrusted in-process scripting; it is not a cheaper way to get
the plugins this project wants.

**Not measured, therefore not rejected:** `wasmtime` (WASM), `boa` (pure-Rust
JS), `rhai`, `mlua`, `deno_core`/V8 are **absent from the 1,668-crate offline
cache**, and crates.io is unreachable from this host. WASM in particular would
answer the distribution problem in §2.6 — one artifact, every platform, no
interpreter installed, language-agnostic — so it deserves a real evaluation when
a network is available. Recording it as "rejected" without a build would repeat
the mistake this section just corrected.

Re-running the QuickJS measurement does not touch this workspace, and must not:
adding `rquickjs` as a dev-dependency would put it in every `--all-targets` build
and break the dependency budget. Make a scratch crate **outside** the repo with
an ASCII `CARGO_TARGET_DIR`, give it `rquickjs = "0.14"` and the same
`[profile.release]` as the root `Cargo.toml`, then compare against a crate whose
`main` is only `println!("hello")`:

```rust
let rt = rquickjs::Runtime::new()?;
let ctx = rquickjs::Context::full(&rt)?;
ctx.with(|ctx| {
    let out: String = ctx.eval("'hello, ' + (21 * 2)")?;   // "hello, 42"
    let kind: String = ctx.eval("typeof fetch")?;          // "undefined"
    Ok::<_, rquickjs::Error>(println!("{out} / fetch {kind}"))
})?;
```

The size delta is the difference between the two stripped executables, not the
absolute size of either.

### 2.5 How to make a choice like this

The question "which architecture is best, and fastest without losing much" has no
answer until it is turned into four measurements. This is the order that worked
here, and the order that would have saved time if it had been followed from the
start.

**1. Find the denominator before optimising the numerator.** A turn is one model
round trip plus a handful of tool calls. On this project that is 1,203 ms against
37 µs. Any optimization of the 37 µs is rounding error, however impressive the
ratio looks. Write the denominator down first; it disqualifies most candidates
without a benchmark.

**2. Measure the cold path separately from the warm path.** They differ by 1,000×
here (36–45 ms spawn versus 37 µs call). An architecture that looks slow per-call
can be the right one, and an architecture that looks fast per-call can be ruinous
if it pays startup every time. The decision that protected performance in this
design was *keeping the child alive*, not the transport.

**3. Price what the choice removes, not only what it adds.** An embedded engine
would remove the process boundary and add: 1.02 MB of binary (§2.4 measured this;
an earlier draft of this paragraph said "tens of megabytes" and was wrong by 30×),
a second build chain, a second cross-compile matrix, the loss of
`panic = "abort"`, the loss of "kill the child" as a containment story, and a hand-
built API surface for every capability the runtime used to provide. Those are not
microseconds, and they are paid on every build and every incident rather than once
per call.

**4. Ask which property is actually scarce.** For a plugin system the scarce
properties are *can a stranger write one*, *can it be installed*, and *can it be
stopped*. Latency is abundant — there is a 1,203 ms budget sitting right there.
Choose the design that spends the abundant resource to buy the scarce one.

**5. Check which axis you have not measured at all.** Four measurements of time
produced a confident language recommendation while memory — the property this
rewrite was actually sold on — had never been measured once. §2.7 is what happened
when it was: 19.1 MB private per Node plugin, no sharing between plugins, and a
multiplexing saving larger than the whole spread between languages. The
recommendation survived, but its *reasoning* was incomplete, and the next decision
would have been made on the wrong axis.

Three failure modes this project hit, worth naming because all looked like
progress:

| Mistake | What it looked like | What fixed it |
| --- | --- | --- |
| Measuring a proxy | "80 µs round trip" from two Node processes, quoted as if it were our host | measure the real pair; keep the script in the repo so the claim is re-runnable |
| Reading a coarse field as a fine one | `durationMs: 2` treated as evidence about microseconds | check the resolution at the source (`as_millis()`) before quoting it |
| Optimising the axis you happened to measure | four latency numbers, zero memory numbers, a confident recommendation | list the properties the design is sold on and check each has a number (§2.7) |

And the honest cost of the choice that was made: Rust's iteration loop is slower
than Go's or TypeScript's — a full workspace test run is ~30 s here and the borrow
checker sends designs back for rework. That is a real tax on *authoring* speed,
it is not visible in any runtime number above, and it was accepted in exchange
for the runtime properties in §2.1. Pretending it does not exist would make this
document a sales pitch instead of a decision record.

### 2.6 Which language a plugin should be written in

§2.2 says "whatever the plugin author uses", which is right when the author is a
person with a language they already know. It is incomplete when the author is an
**agent**, because an agent has no familiarity to preserve. Its costs are the
price of one iteration and the price of shipping one artifact.

**First, three questions that were being answered as one.** "Which language" is
really:

1. What **runtime** executes plugin code? (`node`, `python`, or the entry itself)
2. What **language** does the author type? (JS, TS, Python, Rust, …)
3. What **artifact** gets distributed? (source tarball, or a per-platform binary)

The protocol already separates them: `runtime` is a closed set of three and the
entry is any file, so **the host is language-agnostic and no choice here is
exclusive**. What is actually being decided is much smaller — *which one gets the
paved road*: the scaffold, the examples, the docs, the first thing a stranger
finds. Choosing a paved road does not remove the other two.

**How many stacks are actually in the tree** (`git ls-files`, by extension):

| Files | What it is | Status |
| --- | --- | --- |
| `.rs` 218 | the kernel, 23 crates | primary |
| `.tsx` 21 + `.ts` 19 | the web UI | primary |
| `.js` 2 | `crates/plugin/tests/reference-node/{index,slow}.js` | protocol reference, not a stack we develop in |
| `.py` 1 | `crates/plugin/tests/reference-node/smoke.py` | 18-line second-runtime proof |
| `.ps1` 2, `.sh` 1 | `check.ps1`, `scripts/install.{ps1,sh}` | glue |
| `.yml` 2, `.yaml` 1 | CI workflows, `architecture.yaml` | config |
| C | SQLite via `rusqlite` bundled, `ring` | already compiled today |

So there are **two primary stacks — Rust and TypeScript** — and JS/Python appear
as two reference implementations of one protocol. "Adding JS plugins" is
therefore not obviously a third stack, but it *is* a second authoring language
next to the frontend's TypeScript, and that cost is real.

**Distribution was a hard constraint; it has been lifted.** The original
`crates/market/src/index.rs` described a bundle as
`name/version/description/author/keywords/homepage/tarball/sha256/size/engines`
with **no `os`/`arch` field**, and required `name` + `version` to be unique, so
six platform builds of `myplugin 1.0.0` could not even be listed — a native
plugin could not be distributed at all. That is now changed: `Bundle` carries
optional `os`/`arch` (both absent = platform-independent source, both present =
a native build, half-declared refused), the uniqueness key is
name + version + os + arch, and `resolve` filters to the host platform before
picking a version. The accepted spellings are the release pipeline's
(`windows`/`linux`/`darwin`, `amd64`/`arm64`), so the market does not refuse the
artifacts this project builds.

The second half of the gap was that an installed bundle landed in
`market/bundles/` while the plugin host only read `plugins/`, so `market install`
ended at a directory nothing loaded. The host now reads **both roots**
(`Host::add_root`), and a name present in both is refused with both paths named
rather than resolved by order. Verified end to end on the release binary:
`market install native` → `plugin list` shows it → `plugin call native echo`
answers; a linux-only bundle on Windows is refused naming the platforms that are
published. A source plugin is still one tarball that installs everywhere its
runtime exists.

**The paved road is TypeScript, and it needs no build step.** This was measured,
not assumed: Node 24.21.0 executes `.ts` directly, and a plugin whose manifest
says `runtime: "node"`, `entry: "index.ts"` — with `interface`, type annotations
and `as Error` in it — completed the whole path through the **release binary**:

```
forge plugin start tspeak   -> tspeak 0.1.0 speaks protocol 1
forge plugin call tspeak greet --args {"name":"no build step"}
                            -> hello from typescript, no build step
```

**Zero host changes.** `Runtime::Node` already runs `node <entry>`; the entry
being `.ts` is not a special case. So TypeScript costs nothing extra to support
and adds no language next to the frontend.

Its limits, also measured — Node strips types, it does not compile them, so only
*erasable* syntax works:

| Construct | Result |
| --- | --- |
| `interface`, annotations, `as`, generics | works |
| `import { x } from './lib.ts'` | works (the `.ts` extension is required) |
| `enum` | `SyntaxError [ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX]: TypeScript enum is not supported in strip-only mode` |
| constructor parameter properties | refused, same error class |

Older Node needs `--experimental-strip-types` or a build; that is exactly what
`engines.runtime` in the market index exists to declare.

So the recommendation, in order:

| Case | Author writes | Why | Private RAM per plugin (§2.7) |
| --- | --- | --- | --- |
| **Paved road** | **TypeScript** (`.ts`, no build) | same language as the UI, types an agent can act on, one artifact for every platform, already runs | 19.1 MB |
| Plain JS is fine too | `.js` | identical runtime and distribution; use it when types buy nothing | 19.1 MB |
| The work already lives in a library | **Python** | pandas/openpyxl/requests are the reason to be there; same distribution story | 7.5 MB |
| CPU-bound, or no runtime may be assumed | **Rust** via `runtime: "exec"` | ~5× faster cold start, half the per-call floor, no runtime dependency, **cheaper than the host itself**; the market now carries platform builds and the host reads them | **0.7 MB** |
| Untrusted in-process scripting, someday | embedded engine | see §2.4: 1.02 MB, but no `fetch`/`console`/`require` — a different product | not measured |
| One artifact, no interpreter, any language | **WASM** | the strongest unexplored option; **not measured**, `wasmtime` is not in the offline cache | not measured |

The Rust row is real and tested: `crates/plugin/src/bin/forge-plugin-native.rs`
is a working native plugin, and three integration cases in
`crates/plugin/tests/host_it.rs` load it through `exec`. Before it existed,
`Runtime::Exec` had only ever been tested as a string that parses — every other
case spawned an interpreter, so a broken no-interpreter argv would have passed
the suite.

**What does not change: the kernel stays Rust.** The two decisions are
independent, and §2.3 is why: the boundary costs 0.003% of a turn, so a plugin's
language cannot move the kernel's own 8.5 MB binary, 5 ms start or ~6 MB idle.
Choosing TypeScript for plugins is not a partial retreat from Rust — it is the
*same* decision that put the web UI in TypeScript, which is why it costs no new
stack.

**But "the kernel stays small" is not "the machine stays small".** An earlier
version of this paragraph let the 6 MB idle figure stand as the whole memory
answer. It is not: every enabled plugin is a separate process with its own
interpreter, and §2.7 measures what that adds. Read the two together.

### 2.7 What a plugin costs in RAM (measured)

Every number in §2.3 is a *time*. Memory is a different axis, and it is the one
this rewrite was sold on — `forge web` idles around 6 MB where the Electron
original held several GB. An interpreter per plugin could spend that advantage
quietly, so it was measured instead of assumed in either direction.

Reproduce it with:

```text
cargo build --release -p forge-plugin --bins --offline
cargo run --release -p forge-plugin --example memory_cost --offline -- 120 3
```

It holds the children open and prints its pid; sample that pid and its children
from another shell (`Get-CimInstance Win32_Process -Filter "ParentProcessId=<pid>"`
plus `Get-Process` for private bytes, or `ps -o rss= --ppid <pid>`). **One method
for all three runtimes**, so the comparison is fair. Working set includes shared
image and DLL pages; **private bytes is the honest incremental cost**, so both are
recorded.

An empty plugin of each runtime, plus the host holding them:

| Process | Working set | **Private bytes** |
| --- | --- | --- |
| `node.exe` (one plugin) | 55.9 MB | **19.1 MB** |
| `python.exe` (one plugin) | 12.6 MB | **7.5 MB** |
| native `exec` plugin | 4.4 MB | **0.7 MB** |
| the forge host itself | 6.1 MB | 1.8 MB |

Node's own accounting agrees and explains where it goes: `rss 55.9 MB`,
`heapTotal 6.0 MB`, `heapUsed 4.7 MB`, `external 1.9 MB`. **The plugin's code is
6 MB of a 56 MB process**; the rest is the interpreter and V8. That is a floor
paid for *being Node*, not for anything the author wrote.

**The marginal plugin costs the same as the first.** Three Node plugins held at
once: 55.9 + 55.9 + 55.9 = **167.7 MB working set, 57.5 MB private**. Nothing is
shared. So the cost is per *process*, and the number of processes is the
multiplier that matters.

**Multiplexing was measured, not inferred.** One Node process holding three
plugin instances (each with its own module state through a factory) came to
**55.6 MB working set / 19.7 MB private** — against 167.7 / 57.5 for three
processes. That is **112 MB of working set and 37.8 MB private saved**, and it
puts the per-plugin private cost at 6.6 MB instead of 19.1 MB. The saving is real
and it is larger than the entire spread between languages.

What multiplexing would cost, which is why it is not simply done:

- **Crash isolation.** Today a plugin that segfaults or exits takes only itself
  down; the host reports `child_exited` and the turn continues. Sharing a process
  makes one bad plugin fatal to every plugin in its group.
- **CPU and hang isolation.** A plugin spinning forever is currently one killable
  pid with a 10 s call budget. In a shared process it blocks the others.
- **A protocol change.** Frames would need a plugin id, since one stdout stream
  would carry several plugins' replies. `PROTOCOL.md` has no such field today.

**Enabling a plugin pays for it immediately, whether or not it is called.**
`crates/cli/src/wiring.rs:808-840` loads every name in `plugins.enabled` during
wiring, because the model needs the tool schemas before the first request and
those come from `tools/list` over a live pipe. Deferring the spawn would need a
cached tool list with an invalidation story; that is a design decision, not an
oversight, and it is not made here.

**What this changes about §2.6.** The recommendation stands for one or two
plugins, and the memory column above is now part of it. It changes the *priority*
of the remaining work: choosing Python over TypeScript saves 11.6 MB private per
plugin, while sharing one interpreter saves ~12.5 MB per plugin and a native
plugin saves 18.4 MB — so **process architecture outranks language choice on this
axis**, and a plugin that must always be resident is the strongest case for
`exec` or WASM.

**The DSH comparison, restated honestly.** Five Node plugins plus the host is
about **97 MB private / 286 MB working set**, not 6 MB. That is still roughly
50× less than the 4,914 MB the original held across ten processes, so the
advantage survives — but the headline "6 MB" is only true with no plugins
enabled, and quoting it without that qualifier would be the same mistake as
quoting a Node-to-Node round trip as our own.

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
`present`; attachments finished the way §1.1 describes (bodies out of the log).
*Acceptance:* a real fetch of a real URL
through `forge-net` in a test with a local fixture server; `web_search` refuses
with a named reason when unconfigured; `git worktree list` shows the new tree;
`cargo test --workspace` + `check.ps1` green; no new dependency line in
`Cargo.toml`.
*Status: delivered (`a02e61f`, `abeb8cc`, `d218b03`). `forge-net` parses, fetches
and renders HTML with no new dependency; eight end-to-end tests run against a real
loopback listener; attachment bodies lease `<dataDir>/blobs/` and the event keeps
only the reference.*

**P2 — time and memory (~2 weeks).**
`forge-schedule` + `forge goal` + the continuation loop. *Acceptance:* a
next-occurrence table for a fixed rrule corpus (the rrule subset is the oracle);
a goal survives a restart because it is in the log; a continuation round is
bounded and stops on `blocked`.
*Status: delivered (`608ce96`). 59 schedule tests, the rrule table in
`crates/schedule/tests/oracle.rs`; `goal/created|continued|ended` are in
`EventType::ALL` so a restart reads the same budget; `--max-rounds` counts turns,
not continuations (a bug found by the acceptance run: the first round is the one
that created the goal).*

**P3 — platform layer (1–2 months).**
`forge-plugin` (protocol doc + lifecycle + a reference plugin written in a
*non-Rust* language, to prove the claim), `forge-market`, `forge-experts`,
`forge-team`. *Acceptance:* **one** complete reference plugin in Node (the language
DSH's plugin authors already use, so its friction is the migration path) plus a
small Python smoke test that only proves a second runtime can join the protocol —
two full reference plugins would be two maintenance surfaces bought to re-prove one
property; a market install verifies a checksum and refuses a tampered tarball; an
expert team dispatches two members and writes their results to a shared board.
*Status: delivered (`2b90ab5`). The four crates are green (plugin 30+12, market 110,
experts 77, team 48), and `forge plugin|expert|team|market` are CLI commands.
Acceptance evidence, all run on this host:* the reference Node plugin answers
`initialize`, four tools, a prompt and a hook; `tests/reference-node/smoke.py`
joins the same protocol — **18 lines of code** (31 with its comment block), not the
ten the acceptance sentence first asked for, and the gap is honest: the closed table
plus the id echo plus the `shutdown` reply do not fit in ten; the market refusal path
was run against a real `python -m http.server` with an oversized and then a tampered
tarball (each refusal named both numbers, nothing landed in `bundles/`);
`forge team run release-squad --max-parallel 2` dispatched three roster members
onto one board, each task claimed by the worker of the same name, transcripts under
`<board>/workers/`.

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

**A wave between P3 and P4, done first because the browser was blind.** Four
items from §1.1 that P1 did not cover turned out to be one job: the harness could
not show the model a picture, and the UI could not show the operator what the
model had been offered.

| Item | What was actually wrong | What changed |
| --- | --- | --- |
| `read_image` | absent, and its blocker was the transport: a tool result is a text message | `Message.parts` (`ContentPart::{Text, Image}`, `serde(skip)` so the frozen contract JSON does not move), an OpenAI array-form encoder, `Result.parts`, and one follow-up user message per batch of images |
| attachments reaching the model | recorded and downloadable, never seen | `crates/cli/src/attachments.rs` reads the blob store for the projected messages and for `tool/result` `meta.imagePath` |
| the browser knowing the tool set | no endpoint, no reading of the log | `web/src/state/capabilities.ts` reads the newest `llm/call`'s `tools` and groups it; no route added, no dependency added |
| `sandboxAccess.network` | `Spec.allow_net` existed and was never set | honours `sandboxAccess.network`, measured both directions on Windows |

*Status: delivered. 97 CLI lib tests (six of them new bridge tests that found a
real defect: the `expert` tool was registered without a policy rule, so the mode
fallback denied it in two of three modes), 74 tool tests, 97 web tests across 14
files, `check.ps1` green. End-to-end: the second provider request carries
`data:image/png;base64,…`, the 21-line session log carries no `iVBOR`, and
deleting the file makes the next turn report no image — which is what "re-read
from disk, not stored in the log" means when it is true.*

**Still not done, and named rather than implied:** the browser cannot list
installed plugins, the roster, or a team board, because the server publishes no
route for them (the panel says so on screen instead of guessing); `GET
/api/capabilities` is the one field that would unlock it, and it is not
implemented.

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