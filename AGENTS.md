# AGENTS.md — read this before you touch a file

This tree is a Rust rewrite of `../archive/forge-go` (Go). The Go tree is the **reference
implementation**: it is frozen, it is what our tests compare against, and reading
it is the fastest way to understand any behaviour here.

## Three laws

1. **The event log is the only truth.** Every fact about a session is one line in
   `<dataDir>/sessions/<id>.jsonl`, appended in order, never rewritten. If a
   behaviour is not in the log, a restart loses it — and a user is allowed to
   restart.
2. **A reader must be able to read a log it has never seen.** Unknown event types
   are data (skip and count them). Unknown *versions* are errors (refuse the
   file, do not touch it). Payload members are described by the contract, never
   enforced by the reader.
3. **Fail closed where it matters, and say what you did not do.** A sandbox that
   cannot confine must refuse to start, not start quietly unconfined. A check
   that cannot run is `SKIP`, never `OK`. A wave that is not finished is not
   marked done.

## Where things live

| Path | What | Layer |
| --- | --- | --- |
| `crates/core` | event envelope, ids, canonical JSON, error kinds | L0 |
| `crates/config` | config file, home dir, defaults | L0 |
| `crates/exec` | spawn, tree kill, sandbox hook | L1 |
| `crates/sandbox` | job objects / Landlock / sandbox-exec | L1 |
| `crates/session` | log store, projection, compaction, decisions | L2 |
| `crates/llm` | provider contract, SSE client | L2 |
| `crates/tool` | registry, executor, policy, approvals, builtins | L2 |
| `crates/protocol` | wire types shared with the UI | L2 |
| `crates/server` | HTTP/1.1 + WebSocket | L3 |
| `crates/cli` | the `forge` binary | L3 |
| `tools/archcheck` | the dependency gate | — |
| `tests/` | the shared oracle: conformance cases, fixtures, goldens | — |

The layer map is not advice. `architecture.yaml` states it and `make arch`
fails the build when an edge points up. A crate equals a boundary: if you are
tempted to add a module that imports "just one thing" from a higher layer, the
answer is a trait in the lower crate, not an import.

## Build and verify

```powershell
cargo test --offline          # unit + conformance tests
cargo run -q -p archcheck --  # the dependency gate
cargo clippy -- -D warnings
make check                    # all of the above, in the order CI uses
```

Two environment facts, both measured, both non-negotiable:

- **The target directory must be ASCII.** `.cargo/config.toml` points it at
  `D:/forge-rs-target/forge-rs`. Moving it inside this tree makes every crate
  that compiles C fail with `cl: D8050 ... 未能将命令行放入调试记录中`.
- **MSVC only, offline.** The GNU toolchain is installed but broken
  (`dlltool.exe: program not found`), and crates.io is unreachable; the rsproxy
  mirror in `D:\Rust\.cargo\config.toml` is what `--offline` reads.

## Task card

Before you write code, write four lines in your head (or in the task):

```
goal:      what becomes true when this is done
scope:     which files may change
accept:    the command and its expected output
unknowns:  what you will report instead of inventing
```

## Definition of done, by change type

| Change | Done means |
| --- | --- |
| A new crate | README with frontmatter + four sections; `make arch` green |
| A new event type | added to `EventType::ALL`, `docs/contracts` equivalent here, and a conformance case |
| A bug fix | a test that fails on the old code and passes on the new |
| A perf claim | a measured number on this box, with the command that produced it |
| A platform claim | either measured here, or labelled `unverified on this host` |

## Handoff shape

Report in this order: `done` (files + line counts), `evidence` (command + output
tail), `open` (what is not done and why), `risk` (what could bite the next
person). Never report a passing gate you did not run.

## Six anti-patterns

1. Do not edit `tests/conformance/cases/**` to make a reader pass. The case is
   the contract; if it is wrong, both implementations are wrong.
2. Do not add a dependency before asking. The budget in `Cargo.toml` is a
   decision record; every line answers "why can the standard library not do
   this".
3. Do not add `tokio` (or any async runtime) "for later". The server is
   thread-per-connection because the workload is one process per project.
4. Do not write `unwrap()` on I/O or parse results in library code. Tests may.
5. Do not claim a sandbox is enforcing something it does not enforce. Report
   `filesystem=false` and say what is missing.
6. Do not let `help` list a command that does not work. A missing line costs a
   question; a lying line costs an afternoon.
