# ADR-0002 — the harness is written in Rust

- **Status:** accepted, in progress (2026-11-16). Supersedes the language half of
  [01-tech-stack-adr.md](01-tech-stack-adr.md) (ADR-0001, Go + React).
- **Decision owner:** the user. This document records the consequences, not the
  debate; ADR-0001 keeps the measurements that led to Go so the reversal has a
  paper trail.
- **Scope:** a new tree at `D:\魔改版Agent框架\forge-rs`. The Go tree was moved
  to `D:\魔改版Agent框架\archive\forge-go` once the port passed W6 and the five
  capability crates were wired in: it is **frozen** as the reference
  implementation — archived rather than deleted, with its git history intact, not
  edited except for bug fixes that change an oracle, and it is what the Rust
  build is tested against. Nothing in this tree reads it at build or test time;
  the conformance cases, fixtures and goldens were copied here, so the Go tree is
  documentation and a second opinion, not a dependency.

## 0. Why the switch, in one paragraph

Go was chosen for fast iteration and a small dependency surface. Rust wins on
the axes that are permanent — decode throughput, binary size, idle memory — and
loses on the axis that is felt every minute of every day, compile time. The user
weighed that trade and chose Rust. What makes this survivable is that the
expensive part of the port is **not** the code: the contracts, the event format,
21 conformance cases, five fixture logs and two golden files are language
neutral and already frozen. A Rust reader that reproduces those bytes is not
"probably equivalent" — it is verified against a Go writer.

## 1. Measured facts this decision rests on

From `06-perf-and-gaps.md` §7 (same box, same 4,278-byte event):

| Axis | Go 1.27 | Rust 1.96 | Note |
| --- | --- | --- | --- |
| JSON decode | 6,718 ns | 1,057 ns | 6.4× faster |
| JSON encode | 3,413 ns | 1,473 ns | 2.3× faster |
| Release binary | 13.1 MB | 0.26 MB (hello) | 16× smaller, before UI embed |
| Idle RSS | 7.9 MB | 4.4 MB | 1.8× |
| Cold start | 13 ms | 5 ms | 2.6× |
| Incremental build | 166 ms | 466 ms debug / 1,595 ms release | **3–10× slower** |

The build-time cost is real and it is the price. §6 is shaped around it: small
crates, few dependencies, `lto = "thin"` in release only.

## 2. Crate map

A crate is the only dependency edge Cargo enforces at compile time, so the layer
map is expressed as a crate graph rather than as a convention. Ten crates, one
per boundary that a reviewer would want to see violated loudly:

| Crate | Layer | Holds | Must not |
| --- | --- | --- | --- |
| `forge-core` | L0 | event envelope, ids, canonical JSON, error kinds | import any other forge crate |
| `forge-config` | L0 | config file, home dir, defaults, validation | know about sessions |
| `forge-exec` | L1 | spawn, stdio, timeouts, process-tree kill, sandbox hook | know about tools or sessions |
| `forge-sandbox` | L1 | containment: job objects, Landlock, `sandbox-exec` | know about the model |
| `forge-session` | L2 | append-only log, projection, compaction, decisions, fork | perform I/O outside its log |
| `forge-llm` | L2 | provider contract, OpenAI-compatible SSE client | know about sessions |
| `forge-tool` | L2 | registry, executor, policy, approvals, builtin tools | decide policy itself |
| `forge-protocol` | L2 | wire types shared by server and UI | depend on the server |
| `forge-server` | L3 | HTTP/1.1 + WebSocket over the log | contain agent logic |
| `forge-cli` | L3 | the `forge` binary | hold logic that belongs in a crate |

`tools/archcheck` reads `architecture.yaml` and the crate manifests and fails
the build when an edge points up. It is the same tool as the Go one, re-derived
rather than translated: in Rust the check is cheap because `Cargo.toml` already
states the truth.

## 3. Dependency budget

The Go build had exactly one direct dependency. Rust cannot: std has no JSON, no
TLS, no SQL and no CSPRNG. The budget is therefore "every line answers why the
standard library cannot":

| Dependency | Why | What it replaces |
| --- | --- | --- |
| `serde`, `serde_json` | JSON is the on-disk and on-the-wire format | nothing in std |
| `getrandom` | 128-bit ids and session tokens need a CSPRNG | `crypto/rand` |
| `ring` | SHA-256 for import ids, and the digest rustls needs anyway | `crypto/sha256` |
| `rustls`, `rustls-pki-types`, `webpki-roots` | HTTPS to the model endpoint; `native-tls` would pull OpenSSL on Linux | `crypto/tls` |
| `rusqlite` (bundled) | the session index needs SQL with FTS5 | `modernc.org/sqlite` |
| `regex` | `fs_grep` promises patterns | `regexp` |

Deliberately **absent**:

- **No async runtime.** No tokio, no hyper. The server is thread-per-connection
  over `std::net`, which is what the Go build effectively was: one process per
  project, a handful of connections, work measured in seconds. An async runtime
  would add ~100 transitive crates and a scheduler to save nothing measurable,
  and it would make every function `async` for the rest of the project's life.
- **No `clap`.** Argument parsing is 200 lines and has to reorder flags that
  appear after a positional argument — behaviour `clap` expresses awkwardly and
  the Go build already hand-rolled.
- **No `anyhow`/`thiserror`.** One `Error` type with a `kind` is enough, and a
  proc-macro crate is a tax on every build.
- **No `chrono`.** Timestamps are `i64` milliseconds; formatting ISO-8601 for
  humans is 30 lines.

## 4. Toolchain constraints, measured on this host (2026-11-16)

These are not preferences. Each was found by a failing build:

1. **The target directory must be an all-ASCII path.** With the target inside
   `D:\魔改版Agent框架\...`, every crate that compiles C (`rusqlite`, `ring`)
   dies with `cl: 命令行 error D8050 : 无法执行 c1.dll: 未能将命令行放入调试记录中`
   — `cl.exe` writes its command line into a debug record in the ANSI code page,
   and the `/Fo` path is not representable. Fixed by `.cargo/config.toml`:
   `target-dir = "D:/forge-rs-target/forge-rs"`. The source tree may keep its
   name; only the target must move.
2. **MSVC only.** The `stable-x86_64-pc-windows-gnu` toolchain is installed but
   unusable: `error calling dlltool 'dlltool.exe': program not found`. Do not
   "fix" a build failure by switching to GNU.
3. **MSVC lives at `D:\Visual Studio\VC\Tools\MSVC\14.51.36231\`** and is not on
   `PATH`. `cc` finds it through the registry; nothing needs to be exported.
4. **Crates come from the rsproxy mirror** (`D:\Rust\.cargo\config.toml`).
   `crates.io` itself is unreachable here (schannel credential failure), so a
   build that needs a crate not in the cache fails with a network error that
   looks like a certificate problem. `--offline` works for the current budget;
   1,666 crates are cached.
5. **Installed cross targets:** `x86_64-unknown-linux-gnu`,
   `aarch64-apple-darwin`. Missing: `aarch64-unknown-linux-gnu`,
   `aarch64-pc-windows-msvc`. Cross builds are compile checks only until a
   linker for those triples exists — same honesty rule as the Go tree.

## 5. The oracle: how a port is proven

`archive/forge-go` is the writer; `forge-rs` is the reader under test. The shared bytes
live in this tree, copied verbatim in W0:

| Artefact | Count | What it pins |
| --- | --- | --- |
| `tests/conformance/cases/events/*.json` | 21 (9 accept, 12 refuse) | envelope validation, error wording, canonical bytes |
| `tests/fixtures/events/v1/*.jsonl` | 5 | real logs: happy path, approvals, aborted turn, unknown future event, torn tail |
| `tests/golden/projection/basic.json` | 1 | the folded state after replaying a known log |
| `tests/golden/agent/text-turn.json` | 1 | a whole turn's event sequence |
| `tests/conformance/cases/http/*.json` | 34 | HTTP route/error/stream behavior |
| `tests/conformance/cases/ws/*.json` | 8 | snapshot/event/delta framing, ping, close codes, upgrade policy |
| `docs/contracts/*.md` | 6 | field names, status codes, exit codes |

Rules that keep the oracle honest:

- **A case file is never edited to fit a reader.** If Rust disagrees with a
  case, Rust is wrong or the case is wrong for both — and that is a conversation
  with the Go tree, not a quiet edit here.
- **Byte equality, not structural equality.** `ok_minimal_event` asserts the
  exact line, key order included, because the Go writer produced it.
- **The conformance runner is shared, not duplicated.** It reads the same
  directory; adding a case adds it to both languages at once.

`cargo test -p forge-core` already runs all 21 cases and reproduces every
`canonical` array byte for byte (W0, 2026-11-16).

## 6. Waves

Each wave ends with a green gate and a commit; nothing is "in progress" across a
wave boundary.

| Wave | Delivers | Acceptance | Status (2026-11-16) |
| --- | --- | --- | --- |
| **W0** | workspace, `.cargo/config.toml`, `forge-core` (event, ids, canonical JSON, errors), conformance runner, this document | 21/21 cases; canonical bytes identical; `cargo test` green | Delivered, `04ebb4f` |
| **W1** | `forge-session` (store, recovery, projection), `forge-config`, `forge-exec`, `forge-sandbox`, `forge-cli` (version/help/doctor), `tools/archcheck` | 5 fixture logs replay to the golden projection; torn tail repaired; `--version` < 30 ms; `doctor` prints ten checks | Delivered, `1b5ef75` |
| **W2** | `forge-llm` (SSE), agent loop with retries, cancellation, steering | fixture streams drive a whole turn; event order matches `tests/golden/agent/text-turn.json` | Delivered, `1242294` |
| **W3** | `forge-tool` (registry, executor, policy, approvals, 7 builtin tools, danger list) | a tool call executes end to end; approval recorded before asked; denied paths produce `denied`/`denied_refused` | Delivered with W2, `1242294` |
| **W4** | `forge-protocol`, `forge-server` (HTTP + WS), embedded UI, `forge chat`/`forge web` | API contract §4 satisfied; WS frames; the React build served from the binary | Delivered, `4002c60` |
| **W5** | index (`rusqlite`), import, MCP, skills, subagent | `forge index --rebuild` / `--search`; `forge import` reproduces an old tree; a fake MCP server's tools appear | Delivered, `4002c60` |
| **W6** | bench, release, docs | portable six-metric bench; three native release runners; this document's claims all measured | Delivered, `4002c60` |

### W1 evidence (measured on this host, not quoted from a report)

- `pwsh -File check.ps1` (fmt → clippy `-D warnings` → archcheck → workspace tests): **ok**.
- `forge --version`: **10.5 ms**, binary **1.64 MB** (Go: 11 ms / 13.1 MB).
- `forge doctor`: 10 checks in **4 ms** warm. The Rust port initially spawned
  PowerShell for the disk check (664–1379 ms); it now calls
  `GetDiskFreeSpaceExW` from `windows-sys 0.52` — a dependency already present in
  the lock file via `ring`, not a new one.
- Cross checks: `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin` exit 0.
- Tests: session 21, exec 25, sandbox 11, cli 12, config 11, core 15,
  archcheck 27. The sandbox suite intentionally allocates 1 GiB to prove the
  job-object cap; its "memory allocation failed" line is the expected signal,
  not a failing test.
- `tools/archcheck` proves its rules by deliberate violations: an upward edge
  in `crates/core/Cargo.toml` reports `edge.upward: forge-core (L0) depends on
  forge-exec (L1)` and exits 1.

GNU `make` is not installed on this host (only `nmake`, which echoes recipes),
so `check.ps1` runs the same four steps in the same order as the Makefile.

### W2 boundary, fixed before delegating

`crates/llm/src/contract.rs` is owned by the consumer, not by whichever wave
writes the provider. It froze `Role`, `Message`, `ToolCall`, `ToolSpec`,
`Request::validate`, `Usage`, `Delta`, `Stream`, `Provider`, `ProviderError` and
`Aggregator` before three agents started in parallel, so the agent loop and the
SSE client compile against the same shapes. `forge-agent` likewise defines its
own narrow `ToolRunner` trait; W3's `forge-tool` implements it later instead of
the two crates waiting on each other. `forge-agent` is declared in
`architecture.yaml` L2, and no new dependency was added to the workspace for
this split.

The web UI is **not** ported. It is TypeScript and it talks HTTP: it moves
across unchanged, which is also the cheapest proof that the wire contract held.

### W2/W3 evidence (measured on this host)

- `pwsh -File check.ps1`: **ok** across the whole workspace, after W2/W3 landed.
- `forge-llm`: 35 integration tests, including UTF-8 split inside 🚀, a 256 KiB
  tool-arguments fragment, exact `[0, 1, 0, 1]` call indices, Retry-After in
  seconds and HTTP-date form, key redaction, and `[DONE]` gating. `https://`
  bases use the workspace-frozen rustls stack, not a TCP-only stub.
- `forge-agent`: 8 loop tests + 9 summarizer tests. Golden two-step turn matches
  `tests/golden/agent/text-turn.json` once `ts`/`id` are stripped. The frozen
  `contract.rs` was not edited by the provider or loop agents.
- `forge-tool`: 31 tests after replacing the brief's accidental hand-rolled
  regex with the workspace's `regex` crate. The dumber matcher would have
  changed what the model sees from `fs_grep` — a behaviour port cannot ship.
- `DEFAULT_SUMMARY_BYTES` was first written as 8 KiB by the agent; the lead
  changed it to **2048**, matching Go's `session.DefaultSummaryBytes`
  (`archive/forge-go/internal/session/compact.go:513`) and `m4-surface.md` §4. A
  different default silently changes what a resumed session remembers.
- Deliberate limits: cancellation is a cooperative `Cancel` token checked
  between stream deltas, not a goroutine interrupting a blocking `Recv`; a
  private-CA TLS endpoint is not covered (trust is pinned `webpki-roots`).

### W4–W6 evidence (measured on this host)

- `cargo build --release --offline`: exit 0, native binary **4,368,896 B**. The
  Go release binary was 13.1 MB; the difference is mostly Go's runtime and the
  two HTTP/TLS stacks, not missing functionality.
- `forge --version` returns in **5 ms** in the bench harness; the Go tree's
  recorded figure was 7.21 ms. `forge doctor` prints ten checks in warm
  single-digit milliseconds; the sandbox check is a WARN that names exactly
  what this host does and does not confine.
- Web assets from the unchanged React tree build cleanly
  (`index-3Y85-UxI.js` 265.72 kB / gzip 81.64 kB) and `crates/server/build.rs`
  embeds `web/dist` on the next server rebuild. The 34 HTTP conformance cases
  run against the real Rust loopback server; a further six server tests cover
  WebSocket frames, policy-before-upgrade, static cache headers, embedded UI,
  cookie auth, and cancel/steer.
- `forge-store` rebuilds 1,000 sessions / 100,000 events in **335 ms** best of
  repeated runs (budget 10 s). `forge-import` ports all six fixture families
  verbatim and re-parses every output with `forge_core::parse_log`; 37 tests.
  `forge-mcp` has 43 tests over stdio and HTTP/SSE, `forge-skill` 21 over
  fixtures and a golden scan, `forge-subagent` 22 including nested delegation.
- `forge chat`/`forge web` are wired into `crates/cli` with the Go flags,
  exit codes, golden event order, `RecordingApprover`, and per-turn adapters.
  Browser answers are written by the HTTP layer with `decidedBy:"user"`;
  cancelled/timed-out asks are written by the adapter with
  `decidedBy:"cancelled"|"timeout"|"error"`, matching the frozen contract.
- `tools/bench` records **windows/amd64, rustc 1.96.0** in
  `scripts/bench/baseline.json` from a settled tree: version startup 5.14 ms,
  append fsync 4,501/s, append batch 76,942/s, replay 10k 159,766/s,
  projection 10k 92,313/s, event roundtrip 2,843 ns/op. A subsequent `-strict`
  run passed all six metrics (the fsync figure swung 18%, inside its deliberate
  35% storage tolerance). Debug-harness numbers were discarded: they measured
  the harness, not the release build.
- `.github/workflows/release.yml` uses three **native** runners (Windows,
  Ubuntu, macOS arm64) because no cross linker exists here; docs/release.md
  states which artifacts are not produced and that the workflow has never run
  on this host. The Go tree's six cross-check artifacts remain the only
  compile-only evidence for the additional targets.
- The WebSocket golden corpus is mirrored byte-for-byte in both trees and
  caught three Rust server deviations: synthetic delta `seq`, a JSON text
  keepalive instead of an RFC6455 control ping, and close code 1011 on ping
  timeout. All three were fixed in `crates/server/src/server.rs`; goldens were
  not weakened. Go `TestWS` (8 subtests) and Rust `golden_ws_transcripts` pass.
- Full workspace: `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `archcheck`, and `cargo test --workspace
  --offline` all exit 0. The sandbox suite's
  `memory allocation of 1610612736 bytes failed` line is the expected signal
  from the job-object memory cap test, not a failing test.

## 7. What changes on purpose

Not every Go idiom survives, and pretending otherwise would produce worse Rust:

| Go | Rust | Why |
| --- | --- | --- |
| `net/http` server | hand-written HTTP/1.1 on `std::net` threads | no runtime; the request set is small and frozen in `api.md` |
| `sync.Mutex` + struct | `Arc<Mutex<…>>` with a named owner type | the owner is the thing that must not be cloned accidentally |
| `error` returns with `%w` | `Result<T, Error>` with a `kind` | callers branch on kind, humans read the message |
| goroutine per turn | `std::thread` + channels | one turn at a time per session is already the contract |
| `context.Context` | an explicit `Cancel` token | cancellation is a handle the server hands the runner, not an ambient value |
| struct tags for wire names | serde attributes | same idea, same names |

## 8. Porting traps found in W1

Places where a careful Rust port still got the contract wrong, kept here because
each one is a shape of mistake that will recur in the later waves.

| Trap | What happened | Rule it broke |
| --- | --- | --- |
| Minting ids where the contract copies them | `fork` gave every copied record a fresh `id`, with a plausible-sounding comment ("two logs must not share an id"). Events-v1 §4.1 allows exactly **two** envelope changes per copied record — `session` and `seq` — and §9 makes uniqueness a **per-log** invariant, so a shared id is not a collision but the correlation handle a reader relies on. Fixed in `crates/session/src/fork.rs`, pinned by a per-record id/ts/turn/step/payload comparison against the parent. | events-v1 §4.1, §9 |
| Trusting "uniqueness" as global | The same confusion would make `import` reject a legacy log whose ids came from another tool, or make W5's index key on `(session, id)` instead of `(session, seq)`. | events-v1 §9 |
| A placeholder that "looks finished" | `crates/{llm,protocol,server,tool}` contain only a doc comment naming their wave. Their READMEs say `public: []` and state what is *planned*, because a half-implementation is harder to spot than an empty one. | AGENTS.md law 3 |
| A fixture that projects to nothing | The first version of `web_compaction_folds_closed_turns_into_one_summary` seeded `{"turn": n}` for every record. The summarizer projects `text`, so the history came out empty, `summarize_messages` took its "one message (the system prompt)" branch and returned an **empty string with no error** — the test failed on the payload, not on the compaction. Seed what the product writes, or the fixture measures nothing. | m4 surface (`text` is the projected field) |
| Two writers, one guard | `forge_session::compact` refuses an empty summary (`ERR_EMPTY_SUMMARY`), and `forge chat` inherits that guard through `compact_log`. `forge web` builds its summary event by hand and called the summarizer directly, so it had no guard: a log whose records project to no messages would have replaced hundreds of records with an empty summary. The guard now lives in `crates/cli/src/web.rs` too, pinned by `web_compaction_refuses_an_empty_summary` (log bytes identical before and after). A shared helper would have made this impossible; two calls to the same library are not one code path. | session compaction invariants |
| A test that encodes the bug it guards | The handshake test asserted the RFC sample's `Sec-WebSocket-Accept` **without** its trailing `=`, and the WebSocket corpus trimmed `=` from both sides before comparing. So a server that dropped the padding passed 100% of the suite while failing every browser: a browser compares the accept byte for byte. SHA-1 is 20 bytes, so the correct value always ends in one `=`. Both assertions now demand it. When a test compares an encoding, compare the whole encoding. | RFC 6455 §4.2.2, api.md §3 |
| A flow no real client can perform | `api.md` described a browser that sends `X-Forge-Protocol` on a WebSocket upgrade and presents a cookie "set out of band". A page can do neither: it cannot set handshake headers, and nothing in the product ever set that cookie. Every test passed because the tests wrote raw handshakes and could set any header they liked. Found by driving the shipped bundle in a real browser; see `docs/web-ui.md`. A protocol step that only a hand-written client can execute is not implemented, it is documented. | api.md §2, §3 |

## 9. Risks

| Risk | Mitigation |
| --- | --- |
| Compile time makes iteration painful | ten small crates, `thin` LTO in release only, no async, no proc-macro crates beyond serde |
| Two trees drift while both exist | `archive/forge-go` is frozen; a fix that changes an oracle is made in Go first, then ported |
| Windows-only C toolchain quirk resurfaces | ASCII target dir is in `.cargo/config.toml`, documented in §4.1, and asserted by CI |
| Rust rewrite stalls at 60% | waves are independently shippable; W0–W1 already replace the storage layer, and the Go binary keeps working meanwhile |
| Agent-written Rust compiles but is unidiomatic | every crate has a README with `may_import`/`must_not`, and `archcheck` enforces the graph, not the style |
