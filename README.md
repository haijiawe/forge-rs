# forge-rs

A Rust rewrite of the Go tree in `../forge-v2`. The Go tree is the reference
implementation: its tests are the oracle, and ported behaviour is expected to
match it rather than to improve on it.

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

- **The target directory must be all-ASCII.** `.cargo/config.toml` sets
  `target-dir = "D:/forge-rs-target/forge-rs"`. With the target inside
  `D:\魔改版Agent框架\...`, every crate that compiles C dies in `cl.exe` with
  `D8050 ... 未能将命令行放入调试记录中`. The source tree may keep its name;
  only the target must move.
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

`.github/workflows/ci.yml` runs the same gate on every push and pull request
(`fmt`, `clippy -D warnings`, `archcheck`, `cargo test` on ubuntu and windows,
plus the web bundle with its 2 MB raw budget). `.github/workflows/release.yml`
builds and checksums the artifacts on version tags.

## What is not done yet

- The Rust provider accepts `https://` through the pinned `webpki-roots` bundle,
  plus an optional `ca_file` PEM (see `crates/llm/README.md`) for a private CA.
  No live request was made to the example endpoint during the port.
- Compaction is wired on both surfaces (`forge chat` before each turn, `forge web`
  at the top of a turn, both at the Go original's 200-record floor). The web
  surface compacts through `Server::append_event` because the server owns the seq
  counter, so its summary lands one turn later than the CLI's.
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
  only the Windows job object was measured. `forge doctor` says so in its
  `sandbox` line rather than claiming containment it has not shown.
- No `LICENSE` file yet: all rights reserved, do not redistribute.
- `make check` could not be executed on the authoring host because GNU make is
  not installed. Its component commands were run directly and are the source
  of the expected output above; `check.ps1` runs the same five steps on Windows.
