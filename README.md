# forge-rs

A Rust rewrite of the Go tree in `../archive/forge-go`. The Go tree is the reference
implementation: its tests are the oracle, and ported behaviour is expected to
match it rather than to improve on it. It was archived there once the port passed
W6 and the five capability crates were wired into the CLI; nothing in this tree
reads it at build or test time, because the conformance cases, fixtures and
goldens were copied in. It is documentation and a second opinion, not a
dependency.

The workspace is arranged as fifteen product crates, one per boundary that should be
violated loudly, plus `tools/archcheck` and `tools/bench` gates. `architecture.yaml`
states the layer map, and `tools/archcheck` fails the build when a `Cargo.toml`
dependency points to a higher layer or a crate has no boundary README. Cargo
already enforces that the graph compiles; these gates enforce that it points the
right way and is written down.

## Crate map

| Crate | Layer | Holds | Must not |
| --- | --- | --- | --- |
| `forge-core` | L0 | event envelope, ids, canonical JSON, error kinds | import any other forge crate |
| `forge-config` | L0 | config file, home dir, defaults, validation | know about sessions |
| `forge-exec` | L1 | spawn, stdio, timeouts, process-tree kill, sandbox hook | know about tools or sessions |
| `forge-sandbox` | L1 | containment: job objects, Landlock, `sandbox-exec` | know about the model |
| `forge-session` | L2 | append-only log, projection, decisions, fork | perform I/O outside its log; compaction reader only |
| `forge-llm` | L2 | provider contract, OpenAI-compatible SSE client | know about sessions |
| `forge-tool` | L2 | registry, executor, policy, approvals, builtin tools | decide policy itself |
| `forge-protocol` | L2 | wire types shared by server and UI | depend on the server |
| `forge-store` | L2 | SQLite index, FTS search, rebuild | own the event log |
| `forge-import` | L2 | read-only import of old session trees | overwrite an existing session |
| `forge-subagent` | L2 | nested agent runs on their own logs | run without a depth limit |
| `forge-mcp` | L1 | MCP stdio/HTTP client | know about sessions or tools |
| `forge-skill` | L1 | skill frontmatter scanner | import any forge crate |
| `forge-server` | L3 | HTTP/1.1 + WebSocket over the log | contain agent logic |
| `forge-cli` | L3 | the `forge` binary | hold logic that belongs in a crate |

## Toolchain constraints

These were measured on this host, not chosen:

- **Minimum rustc is 1.89**, set in `Cargo.toml`'s `rust-version` and enforced by
  the dependency tree rather than by taste: `rusqlite 0.43` requires 1.89 and
  `libsqlite3-sys 0.40` requires 1.88. The earlier `1.85` in this file was wrong
  for months of the port's planning; `cargo +1.85 check` fails with
  `rustc 1.85.1 is not supported by the following packages` before compiling a
  single line. Verified both ways: the whole suite passes on 1.89
  (`cargo +1.89.0 test --workspace --offline`, 64 suites) and on 1.96.
- **The target directory must be all-ASCII.** `.cargo/config.toml` sets
  `target-dir = "D:/forge-rs-target/forge-rs"`. With the target inside
  `D:\魔改版Agent框架\...`, every crate that compiles C dies in `cl.exe` with
  `D8050 ... 未能将命令行放入调试记录中`. The source tree may keep its name;
  only the target must move.
- **The same path is also exported as `CARGO_TARGET_DIR`** — in the same
  `.cargo/config.toml`, via `[env]`. `build.target-dir` is cargo's private
  business and is not passed to the programs cargo runs, but the e2e harness
  spawns a real `forge` binary and finds it through that variable. Without it,
  `pwsh -File check.ps1` fails with `forge binary not found at
  ...\forge-rs\target\debug\forge.exe` while the same command from a shell that
  exports the variable passes. Two adjacent lines on purpose: change one, change
  both.
- **MSVC only.** The GNU toolchain is installed but broken:
  `error calling dlltool 'dlltool.exe': program not found`. A build failure must
  not be "fixed" by switching to GNU.
- **Offline only.** Crates come from the rsproxy mirror in
  `D:\Rust\.cargo\config.toml`; `crates.io` is unreachable here. Use
  `--offline`. Adding a dependency that is not in the local cache will fail with
  what looks like a certificate error.
- **Cross targets are compile checks only.** `x86_64-unknown-linux-gnu` and
  `aarch64-apple-darwin` are installed; no linker for either triple exists on
  this host. `aarch64-unknown-linux-gnu` and `aarch64-pc-windows-msvc` are not
  installed. `make release` checks the nine pure-Rust crates; `forge-cli` is
  excluded because `rustls`/`ring` needs a target C compiler (`cc`) this host
  does not have. The native CLI is covered by `cargo build --release --offline`.

## Quick start

From a fresh shell, with only `.cargo/config.toml` in effect:

```powershell
cargo build --release --offline
cargo test --offline
cargo run -q -p archcheck --offline
```

`cargo run -q -p archcheck --offline -- -json` emits the CI form; `-root <path>`
checks a tree other than the one above the working directory.

## Commands

Nine spellings, the same nine the Go oracle froze in `docs/contracts/cli.md`:

| Command | What it does |
| --- | --- |
| `forge --version` | prints the version without reading config |
| `forge help` | the nine-line usage block |
| `forge doctor [--json]` | ten read-only checks, in a fixed order |
| `forge chat [flags] [prompt]` | one turn against the model, streaming to stdout |
| `forge web [flags]` | the local browser UI on loopback |
| `forge index [--rebuild] [--stats] [--search TEXT]` | query or rebuild the SQLite index |
| `forge import <old-dir> [--dry-run]` | read-only import of an old-format session tree |
| `forge session fork <id> [--at N] [--id NAME]` | open a new session at a given seq |
| `forge "<prompt>"` | a bare prompt is shorthand for `forge chat "<prompt>"` |

`chat` and `web` both build their tool set through one place (`crates/cli/src/wiring.rs`):
the seven builtin tools, every tool a configured MCP server advertises (each one with its
own `ask` rule, because it runs outside forge), a `skill` tool when at least one skill is
found, and a `subagent` tool when `subagent.maxDepth > 0`. A broken MCP entry is reported
and skipped rather than stopping the runtime.

## Verify

The full gate is one command:

```powershell
make check
```

which is equivalent to running these five in order:

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo run -q -p archcheck --offline
cargo test --offline
cargo run --release -q -p forge-bench --offline -- -strict
```

A clean architecture run ends with `archcheck: ok (.)`; on a complete tree the
test run ends with `test result: ok`. `make release` compile-checks the
installed cross targets only — it does not link, because this host has no linker
for them.

`.github/workflows/ci.yml` runs three jobs on every push and pull request: `test`
on ubuntu and windows against the **minimum** rustc (1.89), `lint` on current
stable (`fmt --check`, `clippy --all-targets -D warnings`), and `web` (`npm ci`,
`tsc --noEmit`, `vitest run`, `npm run build`, 2 MB raw bundle budget). Two
toolchains because clippy grows lints in new releases: a developer on stable can
otherwise write code that a pinned-toolchain lint job never sees — `% 2 != 0`
becoming `is_multiple_of` was exactly that. CI deliberately does **not** pass
`--offline` (a cold runner has an empty registry); the local `Makefile` and
`check.ps1` do, because this host cannot reach crates.io. `.github/workflows/release.yml`
builds and checksums the artifacts on version tags.

Neither workflow has ever run on a GitHub runner: this checkout has no remote and
no `gh`. Both were rehearsed step by step locally instead (see `docs/ci.md`).

## What is not done yet

- The Rust provider accepts `https://` through the pinned `webpki-roots` bundle,
  plus an optional `ca_file` PEM (see `crates/llm/README.md`) for a private CA.
  No live request was made to the example endpoint during the port.
- Compaction is wired on both surfaces (`forge chat` before each turn, `forge web`
  at the top of a turn, both at the Go original's 200-record floor). The web
  surface folds only turns that are already closed, so the summary covers history
  the user can no longer see and lands inside the turn that triggered it — the
  open prompt is projected after the summary, exactly as on the CLI. A summary
  that comes back empty is refused rather than appended (`web_compaction_refuses_an_empty_summary`
  in `crates/cli/src/web.rs`): an empty fold would delete history and record
  nothing in its place.
- Cross-target release builds from this host are compile checks only; see the
  toolchain section and `docs/release.md`. CI uses native runners for the
  artifacts it claims.
- `archcheck` checks layer direction and the README contract only. It does not
  inspect Rust source, enforce the dependency budget, or check that a crate's
  public API matches its README; those are review and `Cargo.toml` concerns.
- Benchmarks are ported in `tools/bench` and a windows/amd64 baseline was recorded
  from a settled tree; `make bench-check` / `-strict` is now armed on this host.
  No linux/amd64 baseline exists yet, so that platform prints a skip line.
- Language-neutral HTTP, event and WebSocket golden corpora are shared with the
  Go oracle and run in both trees. WS caught three real Rust server bugs (delta
  seq, control-ping framing, close code 1000) which are fixed.
- The frozen contracts (`docs/contracts/`) were written while Go was the
  reference, so their code samples are Go; the behaviour they pin is what the
  Rust crates implement, and the corpora under `tests/conformance/cases/` are the
  proof. `src/` has no `unsafe` (every crate denies it).
- Linux and macOS containment (Landlock, `sandbox-exec`) is compile-checked only;
  only the Windows path was measured. `forge doctor` says so in its `sandbox` line
  rather than claiming containment it has not shown.
- **On Windows containment is two rings, and the second one narrows what tools can
  do.** A Job Object caps the process tree, per-process/job memory and process
  count; an AppContainer decides which files and networks the child can reach
  (write access to the workspace, plus `internetClient` only when `allow_net`).
  Measured on this host: a command inside the container writes its workspace, a
  write to a sibling directory is denied by the OS, and a loopback connect is
  refused. The cost is real — a contained process may not read the build directory
  it was started from, and it starts with *nothing* else readable either, so tools
  that expect a home directory (git, npm, pip) fail where they used to work. The
  fix is a list, not a hole: `crates/cli/src/grants.rs` grants read on the
  tool-owned paths a machine actually has (`~/.gitconfig`, `~/.cargo`, `~/.npmrc`,
  `%APPDATA%\npm`, `%APPDATA%\pip`, `~/.config`) and read-write on `%TEMP%`, and
  `sandboxAccess.read` / `sandboxAccess.write` in `config.json` replaces that list
  (an explicit empty list means "grant nothing extra"). Verified on this host by
  reading `~/.gitconfig` from inside a container and failing to read an ungranted
  file next to it (`windows_reads_are_granted_only_where_asked`), and by running a
  real `git config --global --get user.name` inside the container, which printed the
  configured name only after the grants existed. A granted path outside the
  workspace also needs `FILE_TRAVERSE` on every directory above it, or the tool dies
  on `failed to stat '...AppData\Local': Permission denied` before it opens anything
  — so the grant walks the ancestors, skipping volume roots (rewriting `E:\`'s DACL
  measured 160 s) and stopping where it is refused. That write is not free: Windows
  re-evaluates inheritance for the whole subtree, measured 9 s for `%TEMP%`, 11 s for
  `AppData\Local`, 20 s for `AppData` and 25 s for the profile root. It is paid once
  per directory per identity — repeat grants are skipped by asking the ACL, and a
  pause over two seconds prints what it is doing. One profile per
  workspace path is left behind in `%LOCALAPPDATA%\Packages` (permanent, keyed by
  the path); `forge doctor --prune-profiles` deletes them — 106 were removed on this
  machine, after which the count read back as 0. That service is the one unreliable
  part of this story: on a host where enough of those
  profiles have accumulated it starts refusing *new* names outright for minutes at a
  time, so an existing profile is adopted by lookup and never re-created, a hiccup is
  retried for about eight seconds, and if a new identity still cannot be minted the
  containment falls back to one shared identity for the user — which still confines a
  command to the directories granted to it, but no longer separates two workspaces
  from each other. The last resort is said out loud rather than silently skipped.
  A host that cannot create a
  package identity (or a Windows version without AppContainers) gets
  `filesystem=false` as a WARN and `read-only` is refused rather than downgraded.
- The web transcript was measured in a real browser: 5,000 messages (10,001
  events, 480,130 px of scrollback) scroll at a flat **60 fps** while mounting 27
  rows, load in ~650 ms, and hold ~18 MB of heap. Numbers, method and the three
  browser-only defects the measurement exposed are in `docs/web-ui.md`. It is one
  headless Edge on Windows/amd64; no other browser or GPU was measured.
- No `LICENSE` file yet: all rights reserved, do not redistribute.
- `make check` could not be executed on the authoring host because GNU make is
  not installed. Its component commands were run directly and are the source
  of the expected output above; `check.ps1` runs the same five steps on Windows.
