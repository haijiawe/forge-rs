# forge-rs

A Rust rewrite of a Go implementation (`forge-go`) that was **deleted on
2026-11-17**, after the port passed W6 and the five capability crates were wired
into the CLI. That tree was the reference: its tests were the oracle, and ported
behaviour is expected to match them rather than to improve on them. Removing it
changed no test — the conformance cases, fixtures and goldens were copied into
`tests/`, and nothing here read the Go tree at build or test time. Where a
comment cites `forge-go/internal/...` it names a file of that retired tree, kept
as provenance for the behaviour in question; the frozen contracts in
`docs/contracts/` and the corpora under `tests/` are what `cargo test` checks
against.

The workspace is arranged as twenty-three product crates, one per boundary that should be
violated loudly, plus `tools/archcheck` and `tools/bench` gates. `architecture.yaml`
states the layer map, and `tools/archcheck` fails the build when a `Cargo.toml`
dependency points to a higher layer or a crate has no boundary README. Cargo
already enforces that the graph compiles; these gates enforce that it points the
right way and is written down.

CI runs on GitHub Actions: `ci.yml` on every push (test on Windows and Ubuntu at
the 1.89 floor, lint and web on current stable), `bench.yml` on demand, and
`release.yml` on tags or as a dry run — the dry run was rehearsed on 2026-11-20
and produced all three native artifacts. `docs/ci.md` records what each run
caught, including the defects that only a second machine could see.

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
| `forge-net` | L1 | blocking HTTP(S) client, HTML to text, redirects | know about tools or the model |
| `forge-goal` | L2 | goal lifecycle as events, folding, the continuation prompt | write events itself |
| `forge-schedule` | L2 | rrule subset to instants, the task registry, due times | run a turn |
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
- **The target directory must be all-ASCII, and it is set per environment, not
  in the tree.** With the target inside `D:\魔改版Agent框架\...`, every crate that
  compiles C dies in `cl.exe` with `D8050 ... 未能将命令行放入调试记录中`. A
  committed `target-dir = "D:/forge-rs-target/forge-rs"` looked like the fix and
  broke the first Linux CI job instead, because there `D:/...` is one path
  segment containing a colon. `.cargo/config.toml` therefore pins nothing:
  `check.ps1` derives `CARGO_TARGET_DIR` when this checkout's path is not
  all-ASCII, CI sets `target` per job, and a bare `cargo build` from a shell
  must export it yourself. It has to be an environment variable and not only
  `build.target-dir`, because the e2e harness spawns a real `forge` binary and
  finds it through `CARGO_TARGET_DIR`.
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

From a fresh shell, with only `.cargo/config.toml` in effect — on this
checkout, set the ASCII target directory first (or use `check.ps1`, which
derives it):

```powershell
$env:CARGO_TARGET_DIR = 'D:\forge-rs-target\forge-rs'
cargo build --release --offline
cargo test --offline
cargo run -q -p archcheck --offline
```

`cargo run -q -p archcheck --offline -- -json` emits the CI form; `-root <path>`
checks a tree other than the one above the working directory.

## Commands

Thirteen spellings; the first nine are the ones the Go oracle froze in
`docs/contracts/cli.md`, `goal`/`schedule` are the P2 additions, and
`plugin`/`expert`/`team`/`market` are the P3 additions:

| Command | What it does |
| --- | --- |
| `forge --version` | prints the version without reading config |
| `forge help` | the usage block |
| `forge doctor [--json]` | ten read-only checks, in a fixed order |
| `forge chat [flags] [prompt]` | one turn against the model, streaming to stdout |
| `forge web [flags]` | the local browser UI on loopback |
| `forge index [--rebuild] [--stats] [--search TEXT]` | query or rebuild the SQLite index |
| `forge import <old-dir> [--dry-run]` | read-only import of an old-format session tree |
| `forge session fork <id> [--at N] [--id NAME]` | open a new session at a given seq |
| `forge goal start\|status\|end` | record, inspect or close a goal |
| `forge schedule add\|list\|remove\|next\|run` | recurring prompts, and the tick that opens them |
| `forge plugin list\|start\|stop\|call\|doctor` | inspect and drive out-of-process plugins |
| `forge expert list\|show` | the Markdown expert and team roster |
| `forge team list\|run` | run a roster team over a shared task board |
| `forge market list\|search\|install\|remove\|verify` | install plugin bundles, verified by SHA-256 |
| `forge "<prompt>"` | a bare prompt is shorthand for `forge chat "<prompt>"` |

`chat` and `web` both build their tool set through one place (`crates/cli/src/wiring.rs`):
the ten always-present tools (`fs_read`, `fs_write`, `fs_edit`, `fs_glob`, `fs_grep`,
`read_image`, `shell`, `todo_write`, `worktree`, `present`) and the two network tools
`web_fetch` and `web_search` (absent when the capability cannot be built — an absent tool is
discoverable, one that always fails is not), every tool a configured MCP server advertises
(each one with its own `ask` rule, because it runs outside forge), one `<plugin>.<tool>` per
tool a loaded plugin advertises (also `ask`: it leaves this process), an `expert` tool when
the roster has a summonable expert, a `team` tool when `team.maxParallel > 0` and the roster
has a team (`ask`, because every worker spends money), a `skill` tool when at least one skill
is found, and a `subagent` tool when `subagent.maxDepth > 0`. A `goal_end` tool joins them only
inside a goal loop (`forge chat --goal`), because a verdict nobody reads is not a tool. A
broken MCP entry or a plugin that fails to load is reported and skipped rather than stopping
the runtime.

`read_image` shows the model a file that happens to be a picture. The bytes never enter the
event log: the tool returns them once, the loop forwards them as a follow-up user message, and
the log keeps only `meta.imagePath` / `meta.mediaType` / `meta.bytes` — so a later turn re-reads
the file from the workspace instead of the whole session carrying a base64 copy. That is also
why deleting the file makes the model forget the picture. An image attached in the browser takes
the same road from the other side: `<dataDir>/blobs/` holds the body, the log holds the reference.

`web_search` needs an endpoint, because there is no search provider that works without
one. `config.json`:

```json
{
  "search": { "endpoint": "https://search.example/v1/search", "key": "..." }
}
```

The request is `POST {"query": "...", "count": n}` with `Authorization: Bearer <key>` when
a key is set; the reply may hold its hits under `results`, `data`, `items`, `hits`, `web`
or `web.results`, and each hit may spell its fields `url|link|href`,
`title|name|heading`, `snippet|description|content|text`. Anything else is reported as
`bad_content` with the first 400 bytes of the reply, rather than being silently read as
"no results".

Both network tools are `ask` in `read-only` and `workspace-write`: they run inside the
harness, not inside the sandbox, so the approval prompt is the only gate.

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
- Attachment bodies are stored, not understood: POST /messages writes an upload
  to <dataDir>/blobs/ and the event keeps a reference, but nothing feeds those
  bytes to a model. crates/llm/src/contract.rs declares message content as a
  plain String, so an image is recorded and downloadable and still invisible to
  the model. Feeding pixels needs a content-part type in the provider contract.
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
- Linux containment (Landlock) is verified at runtime on every push: the Ubuntu CI
  job runs `crates/sandbox/tests/linux.rs`, which writes inside the workspace, is
  refused outside it, is refused a write in `read-only`, is granted an `extra_write`
  path by name, and gets `permission denied` from the kernel with `allow_net = false`
  (and `connected` with it). macOS `sandbox-exec` is compile-checked only; `forge
  doctor` says so in its `sandbox` line rather than claiming containment it has not
  shown.
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
  (an explicit empty list means "grant nothing extra"), and
  `sandboxAccess.network: true` is the second switch: without it a contained child
  has no socket at all. Verified on this host by
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
  Deleting hits the same wall: half an hour after the 106 went away, the same command
  answered `57 found, 0 removed, 57 failed` with `0x80070005` on every name, while
  creating was failing too — the service refuses wholesale, it is not one stubborn
  profile. So pruning spends its retry budget once as a probe, and if the first name
  is refused with nothing removed it stops and says "the service is refusing right
  now, retry in a few minutes" (3.9 s) instead of grinding through the list for three
  minutes to report the same number.
  A host that cannot create a
  package identity (or a Windows version without AppContainers) gets
  `filesystem=false` as a WARN and `read-only` is refused rather than downgraded.
  One such host is easier to create than it looks: **a process runs at the integrity
  level of its own image when that label is lower than its parent's**, so every
  binary under a directory labelled `Low` — which is what a browser-extracted
  download produces — runs `Low`, and a Low process cannot write `%LOCALAPPDATA%`
  where the profile service creates profiles. The symptom is `ERROR_ACCESS_DENIED`
  from calls that have nothing to do with the sandboxed command, and it cost this
  project an afternoon of chasing "non-ASCII paths", because every failing test ran
  from inside the labelled tree and every passing one ran from `%TEMP%`. The refusal
  now names the condition and prints the remedy (`icacls <dir> /setintegritylevel
  (OI)(CI)M /T /C`), and `available()` reports the mechanism as absent instead of
  letting the strict modes claim containment they cannot deliver.
- The web transcript was measured in a real browser: 5,000 messages (10,001
  events, 480,130 px of scrollback) scroll at a flat **60 fps** while mounting 27
  rows, load in ~650 ms, and hold ~18 MB of heap. Numbers, method and the three
  browser-only defects the measurement exposed are in `docs/web-ui.md`. It is one
  headless Edge on Windows/amd64; no other browser or GPU was measured.
- No `LICENSE` file yet: all rights reserved, do not redistribute.
- `make check` could not be executed on the authoring host because GNU make is
  not installed. Its component commands were run directly and are the source
  of the expected output above; `check.ps1` runs the same five steps on Windows.
