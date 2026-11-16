# ADR-0002 — the harness is written in Rust

- **Status:** accepted, in progress (2026-11-16). Supersedes the language half of
  [01-tech-stack-adr.md](01-tech-stack-adr.md) (ADR-0001, Go + React).
- **Decision owner:** the user. This document records the consequences, not the
  debate; ADR-0001 keeps the measurements that led to Go so the reversal has a
  paper trail.
- **Scope:** a new tree at `D:\魔改版Agent框架\forge-rs`. The Go tree
  (`forge-v2`) is **frozen** as the reference implementation: it is not deleted,
  not edited except for bug fixes that change an oracle, and it is what the Rust
  build is tested against until W6.

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

`forge-v2` is the writer; `forge-rs` is the reader under test. The shared bytes
live in this tree, copied verbatim in W0:

| Artefact | Count | What it pins |
| --- | --- | --- |
| `tests/conformance/cases/events/*.json` | 21 (9 accept, 12 refuse) | envelope validation, error wording, canonical bytes |
| `tests/fixtures/events/v1/*.jsonl` | 5 | real logs: happy path, approvals, aborted turn, unknown future event, torn tail |
| `tests/golden/projection/basic.json` | 1 | the folded state after replaying a known log |
| `tests/golden/agent/text-turn.json` | 1 | a whole turn's event sequence |
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

| Wave | Delivers | Acceptance |
| --- | --- | --- |
| **W0** | workspace, `.cargo/config.toml`, `forge-core` (event, ids, canonical JSON, errors), conformance runner, this document | 21/21 cases; canonical bytes identical; `cargo test` green |
| **W1** | `forge-session` (store, recovery, projection), `forge-config`, `forge-exec`, `forge-sandbox`, `forge-cli` (version/help/doctor) | 5 fixture logs replay to the golden projection; torn tail repaired; `--version` < 30 ms; `doctor` prints ten checks |
| **W2** | `forge-llm` (SSE), agent loop with retries, cancellation, steering | fixture streams drive a whole turn; event order matches `tests/golden/agent/text-turn.json` |
| **W3** | `forge-tool` (registry, executor, policy, approvals, 7 builtin tools, danger list) | a tool call executes end to end; approval recorded before asked; denied paths produce `denied`/`denied_refused` |
| **W4** | `forge-protocol`, `forge-server` (HTTP + WS), embedded UI | API contract §4 satisfied; WS frames; the React build served from the binary |
| **W5** | index (`rusqlite`), import, MCP, skills, subagent | `forge index --rebuild` / `--search`; `forge import` reproduces an old tree; a fake MCP server's tools appear |
| **W6** | bench, release, docs | six cross-built artefacts; `bench -strict`; this document's claims all measured |

The web UI is **not** ported. It is TypeScript and it talks HTTP: it moves
across unchanged, which is also the cheapest proof that the wire contract held.

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

## 8. Risks

| Risk | Mitigation |
| --- | --- |
| Compile time makes iteration painful | ten small crates, `thin` LTO in release only, no async, no proc-macro crates beyond serde |
| Two trees drift while both exist | `forge-v2` is frozen; a fix that changes an oracle is made in Go first, then ported |
| Windows-only C toolchain quirk resurfaces | ASCII target dir is in `.cargo/config.toml`, documented in §4.1, and asserted by CI |
| Rust rewrite stalls at 60% | waves are independently shippable; W0–W1 already replace the storage layer, and the Go binary keeps working meanwhile |
| Agent-written Rust compiles but is unidiomatic | every crate has a README with `may_import`/`must_not`, and `archcheck` enforces the graph, not the style |
