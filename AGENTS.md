# AGENTS.md — read this before you touch a file

This tree is a Rust rewrite of a Go implementation that was **deleted on
2026-11-17**. Comments below that cite `forge-go/...` name files in that retired
tree; they no longer resolve here, and that is expected. What the Go tree
defined is now frozen in `docs/contracts/` and in the corpora under `tests/`,
and those are what the tests compare against.

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

Twenty-four crates, four layers. `architecture.yaml` is the authority; this table
is the same information with a purpose attached to each crate, because a layer
number does not tell you where a new file belongs.

| Layer | Crate | What it owns |
| --- | --- | --- |
| L0 | `crates/core` | event envelope, ids, canonical JSON, error kinds, digests |
| L0 | `crates/config` | config file, home dir, defaults, validation |
| L1 | `crates/exec` | spawn, tree kill, the sandbox hook |
| L1 | `crates/sandbox` | job objects / Landlock / sandbox-exec / AppContainer |
| L1 | `crates/mcp` | MCP client: stdio and HTTP transports |
| L1 | `crates/skill` | skill discovery, frontmatter, scanning |
| L1 | `crates/net` | HTTP GET/POST, TLS roots, HTML→text |
| L1 | `crates/experts` | the Markdown expert/team roster |
| L2 | `crates/session` | log store, projection, compaction, fork, blobs |
| L2 | `crates/llm` | provider contract, SSE client, content parts |
| L2 | `crates/tool` | registry, executor, policy, approvals, builtins |
| L2 | `crates/protocol` | wire types shared with the UI |
| L2 | `crates/agent` | the turn loop, history, hooks, inbox |
| L2 | `crates/store` | the SQLite index and its search |
| L2 | `crates/import` | importing foreign session logs |
| L2 | `crates/subagent` | catalog, descriptors, backends, archive, authority |
| L2 | `crates/goal` | goal domain, events, fold, runtime claim |
| L2 | `crates/schedule` | the rrule subset and the instant table |
| L2 | `crates/market` | index, digest-verified install, tamper-aware removal |
| L2 | `crates/plugin` | the plugin host over stdio JSON-RPC |
| L2 | `crates/team` | shared board, parallel workers, task graph |
| L2 | `crates/context` | compaction blocks, pruning, the ledger |
| L3 | `crates/server` | HTTP/1.1 + WebSocket, all routes |
| L3 | `crates/cli` | the `hearth` binary and its subcommands |

Not crates, and equally part of the map:

| Path | What it is | Rule |
| --- | --- | --- |
| `web/` | the React UI, built by Vite into `web/dist` | zero UI dependencies by decision; `web/src/api/protocol.ts` must change in the same commit as `crates/protocol` |
| `apps/desktop` | the Tauri 2 window, its own cargo workspace | may not be imported by anything; must not enter `check.ps1` (Linux CI would need webkit2gtk) |
| `tests/` | the language-neutral oracle: conformance cases, fixtures, goldens | read-only by law — never edit a case to make a reader pass |
| `skills/` | skills shipped with the tree | a skill is data, not code; it may not import a crate |
| `examples/` | runnable samples (currently a plugin) | must keep working, or be deleted |
| `tools/` | `archcheck`, `bench` | build-time only; nothing here ships in the binary |
| `scripts/` | `sync.cjs`, the installers | may not be required at runtime |
| `docs/` | decisions, records, comparisons, operations | see `docs/README.md` for which is which |
| `bin/` | the release binary, rebuilt by hand | not a cargo artifact — see `docs/13-handover.md` |

### Where do I add things

| You want to | It goes | Because |
| --- | --- | --- |
| a new agent tool | `crates/tool/src/builtin/`, then name it in `policy.rs` | an unnamed tool is silently denied by the fallback |
| a new HTTP route | `crates/server/src/<x>_route.rs` + an arm in `handle_api` | the route table is one function on purpose |
| a new wire type | `crates/protocol/src/protocol.rs` **and** `web/src/api/protocol.ts` | there is no generator; drift is silent |
| a new event type | `EventType::ALL`, a `docs/contracts` line, a conformance case | law 2 makes types additive, and the case is what proves it |
| a new platform integration | a new L1 or L2 crate | a crate is a boundary; a module is not |
| a capability the operator turns on | a `Config` section + `wiring.rs`, never a default-on | absence of a tool is discoverable; a tool that always fails is not |
| a UI panel | `web/src/components/`, no new npm dependency without asking | the budget is a decision record |

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

- **The build path must be ASCII.** This checkout is `D:\hearth`, so the
  default in-tree `target/` is fine and nothing needs to be set. If you move
  the tree somewhere with a non-ASCII character in the path, point
  `CARGO_TARGET_DIR` at an ASCII directory first — otherwise every crate that
  compiles C (rusqlite bundled, ring) fails with
  `cl: D8050 ... 未能将命令行放入调试记录中`. `.cargo/config.toml` explains why
  that setting does not live in the file.
- **MSVC only, offline.** The GNU toolchain is installed but broken
  (`dlltool.exe: program not found`), and crates.io is unreachable; the rsproxy
  mirror in `D:\Rust\.cargo\config.toml` is what `--offline` reads.

## Pushing

`git push` and `git fetch` both die against github.com:443 on this network
(connection reset), so `origin/master` is maintained through the REST API
instead. One command does the whole loop:

```powershell
node scripts/sync.cjs "commit message"            # commit, push, align
node scripts/sync.cjs --message-file note.txt     # commit a multi-paragraph message
node scripts/sync.cjs --no-commit                 # push and align the current HEAD
node scripts/sync.cjs --dry "message"             # say what would happen, write nothing
```

Use `--message-file` for anything longer than a subject line: backticks, quotes
and blank lines survive a file, and none of them survive every shell on the way
here.

The third step matters: a commit created through the API is a new object, so its
SHA differs from the local one even though the tree is identical. `sync.cjs`
rebuilds that object locally (the API normalises timestamps to `Z`, so it tries
the plausible offsets) and refuses to move the branch unless the rebuilt object
hashes to the SHA GitHub reported. After it runs, `git status` says
`master...origin/master` with nothing ahead and nothing behind.

The token comes from `$GITHUB_PAT`, or from `GITHUB_PAT:` in
`$HEARTH_CREDENTIALS` (default `D:\dsh\.credentials.yaml`). It is never printed.

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
