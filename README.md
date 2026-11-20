# hearth

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

The workspace is arranged as twenty-four product crates, one per boundary that should be
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
| `hearth-core` | L0 | the frozen vocabulary — event envelope, ids, canonical JSON, typed errors | import any other hearth crate |
| `hearth-config` | L0 | config file, home directory, defaults, MCP entry warnings | know about sessions |
| `hearth-exec` | L1 | spawn, stdio, timeouts, process-tree kill, sandbox hook | know about tools or sessions |
| `hearth-sandbox` | L1 | containment: job objects and AppContainers, Landlock, `sandbox-exec` | know about the model |
| `hearth-mcp` | L1 | external MCP client — stdio and Streamable HTTP | import any hearth crate |
| `hearth-skill` | L1 | skill discovery, and loading a `SKILL.md` body only when asked | recurse into a skill directory |
| `hearth-net` | L1 | blocking HTTP(S) client, redirects, HTML to text | know about tools or the model |
| `hearth-experts` | L1 | expert and team definitions parsed from Markdown frontmatter | reach into skills or sessions |
| `hearth-session` | L2 | append-only log, projection, compaction, fork | perform I/O outside its log |
| `hearth-llm` | L2 | provider contract, OpenAI-compatible SSE client | know about sessions |
| `hearth-tool` | L2 | registry, executor, policy, approvals, builtin tools | write events, or decide policy itself |
| `hearth-protocol` | L2 | wire types shared by server and UI | depend on the server |
| `hearth-agent` | L2 | one-turn loop, event ordering, summarizer, hook seams | keep a history of its own between turns |
| `hearth-store` | L2 | rebuildable index — SQLite rows, FTS5 over boundary messages | write to a session log |
| `hearth-import` | L2 | read-only import of old session trees | overwrite an existing session |
| `hearth-subagent` | L2 | bounded-pool delegation on fresh child logs | run without a depth limit |
| `hearth-goal` | L2 | goal state folded from the log, and the continuation decision | write events itself |
| `hearth-schedule` | L2 | recurrence rules, the scheduled-task registry, due times | run a turn |
| `hearth-market` | L2 | market index, checksummed install, tamper-aware uninstall | trust a tarball before it is digested |
| `hearth-plugin` | L2 | plugin host — child processes speaking JSON-RPC over stdio | spawn anything from a listing |
| `hearth-team` | L2 | parallel workers over a shared board with per-worker mailboxes | depend on a provider or an async runtime |
| `hearth-context` | L2 | compressed blocks that can be named, restored and searched | hide a range no block names |
| `hearth-server` | L3 | HTTP/1.1 + WebSocket over the log | contain agent logic |
| `hearth-cli` | L3 | the `hearth` binary | hold logic that belongs in a crate |

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
  committed `target-dir = "D:/hearth-target/hearth"` looked like the fix and
  broke the first Linux CI job instead, because there `D:/...` is one path
  segment containing a colon. `.cargo/config.toml` therefore pins nothing:
  `check.ps1` derives `CARGO_TARGET_DIR` when this checkout's path is not
  all-ASCII, CI sets `target` per job, and a bare `cargo build` from a shell
  must export it yourself. It has to be an environment variable and not only
  `build.target-dir`, because the e2e harness spawns a real `hearth` binary and
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
  installed. `make release` checks every crate except `hearth-cli`; that one is
  excluded because `rustls`/`ring` needs a target C compiler (`cc`) this host
  does not have. The native CLI is covered by `cargo build --release --offline`.

## Quick start

From a fresh shell:

```powershell
cargo build --release --offline
cargo test --offline
cargo run -q -p archcheck --offline
```

The checkout this was developed in is `D:\hearth`, which is ASCII, so the
in-tree `target/` works. If you move it somewhere with a non-ASCII character in
the path, set an ASCII target directory first (`$env:CARGO_TARGET_DIR`) or every
crate that compiles C fails with `cl: D8050`; `check.ps1` does that for you when
it notices.

`cargo run -q -p archcheck --offline -- -json` emits the CI form; `-root <path>`
checks a tree other than the one above the working directory.

## Commands

Seventeen spellings; the first nine are the ones the Go oracle froze in
`docs/contracts/cli.md`, `goal`/`schedule` are the P2 additions, and
`plugin`/`market`/`skill`/`subagent`/`expert`/`team` came later:

| Command | What it does |
| --- | --- |
| `hearth --version` | prints the version without reading config |
| `hearth help` | the usage block |
| `hearth doctor [--json]` | eleven read-only checks, in a fixed order |
| `hearth chat [flags] [prompt]` | one turn against the model, streaming to stdout |
| `hearth web [flags]` | the local browser UI on loopback |
| `hearth index [--rebuild] [--stats] [--search TEXT]` | query or rebuild the SQLite index |
| `hearth import <old-dir> [--dry-run]` | read-only import of an old-format session tree |
| `hearth session fork <id> [--at N] [--id NAME]` | open a new session at a given seq |
| `hearth session archive <id> [--reason TEXT]` | record that a session starts no new work |
| `hearth goal start\|status\|end` | record, inspect or close a goal |
| `hearth schedule add\|list\|remove\|next\|run` | recurring prompts, and the tick that opens them |
| `hearth plugin list\|start\|stop\|call\|doctor` | inspect and drive out-of-process plugins |
| `hearth expert list\|show` | the Markdown expert and team roster |
| `hearth skill list\|show` | list or show the skills the model was offered |
| `hearth subagent list\|tree\|resolve\|timing` | the children below a session: names, the tree, resolution, timing |
| `hearth subagent prompt\|interrupt\|providers\|worker` | resume a continuable child, stop one, list backends, run the worker |
| `hearth team list\|run` | run a roster team over a shared task board |
| `hearth market list\|search\|install\|remove\|verify` | install plugin bundles, verified by SHA-256 |
| `hearth "<prompt>"` | a bare prompt is shorthand for `hearth chat "<prompt>"` |

`chat` and `web` both build their tool set through one place (`crates/cli/src/wiring.rs`):
the ten always-present tools (`fs_read`, `fs_write`, `fs_edit`, `fs_glob`, `fs_grep`,
`read_image`, `shell`, `todo_write`, `worktree`, `present`) and the two network tools
`web_fetch` and `web_search` (absent when the capability cannot be built — an absent tool is
discoverable, one that always fails is not), every tool a configured MCP server advertises
(each one with its own `ask` rule, because it runs outside hearth), one `<plugin>.<tool>` per
tool a loaded plugin advertises (also `ask`: it leaves this process), an `expert` tool when
the roster has a summonable expert, a `team` tool when `team.maxParallel > 0` and the roster
has a team (`ask`, because every worker spends money), a `skill` tool when at least one skill
is found, and a `subagent` tool when `subagent.maxDepth > 0`. A `goal_end` tool joins them only
inside a goal loop (`hearth chat --goal`), because a verdict nobody reads is not a tool. A
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

## The desktop shell

`apps/desktop` is a Tauri 2 window around the same web UI: it starts `hearth web` as a
child process, waits for the server to print the URL it is serving, and opens a window at
that URL. It is a **separate cargo workspace**, so `cargo test --workspace` and CI never
need a GUI toolkit to compile a window border; build it on its own:

```powershell
cd apps\desktop
cargo build --offline --release
```

Its arguments are forwarded to `hearth web` (`hearth-desktop --model deepseek-chat`), and
`HEARTH_BIN` says which binary to start. Closing the window kills the child server, so no
listener is left behind. Details, including the two window-creation defects this cost, are
in [apps/desktop/README.md](apps/desktop/README.md).

The web UI itself is a three-column workbench: the file tree, a read-only editor with line
numbers, and the agent panel. The editor reads through `GET /api/sessions/{id}/files|file|diff`
— read-only on purpose, because a write from the window would bypass the policy, the
approval prompt and `tool/call`. Everything the model does still shows up in the transcript
and in the event log.

## Documentation map

Four places, each with one job. Reading them in this order is the shortest path
from "what is this" to "what may I change".

| Where | Answers | Open it |
| --- | --- | --- |
| [README.md](README.md) — this file | what the tree is, how to build it, what each crate owns and must not own | first |
| [AGENTS.md](AGENTS.md) | the rules a change must obey: the three laws, the six anti-patterns, the dependency budget | before writing code |
| [docs/README.md](docs/README.md) | the index of every design and execution document, grouped by purpose, each marked as a record or a plan | when the question is *why*, or you need the evidence |
| [docs/contracts/](docs/contracts/README.md) | the seven frozen contracts — event format, CLI text, HTTP/WS, M4–M6 crate surfaces | before changing anything another implementation is held to |

`docs/` holds four kinds of document and the index says which is which:
**decisions** ([08](docs/08-rust-port.md) why Rust and which crate sits where,
[09](docs/09-platform-tier.md) what the platform tier is missing,
[11](docs/11-adr-agent-hooks.md) the two rewrite seams), **execution records**
([10](docs/10-agent-parity.md) the wave plan, [12](docs/12-subagent-port.md) the
subagent port, [agent-gaps](docs/agent-gaps.md) the gap ledger),
**comparisons** ([dsh-vs-hearth](docs/dsh-vs-hearth.md) layer by layer from source,
[dsh-comparison](docs/dsh-comparison.md) measured on this host,
[agent-comparison](docs/agent-comparison.md) the agent loops), and
**operations** ([bench](docs/bench.md), [ci](docs/ci.md),
[release](docs/release.md), [web-ui](docs/web-ui.md),
[office](docs/office-run.md)).

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
cargo run --release -q -p hearth-bench --offline -- -strict
```

A clean architecture run ends with `archcheck: ok (.)`; on a complete tree the
test run ends with `test result: ok`. `make release` compile-checks the
installed cross targets only — it does not link, because this host has no linker
for them.

`.github/workflows/ci.yml` runs four jobs on every push and pull request: `test`
on ubuntu and windows against the **minimum** rustc (1.89), `sandbox macos-latest`
(the `sandbox-exec` evidence, on a real macOS kernel), `lint` on current stable
(`fmt --check`, `clippy --all-targets -D warnings`), and `web` (`npm ci`,
`tsc --noEmit`, `vitest run`, `npm run build`, 2 MB raw bundle budget). Two
toolchains because clippy grows lints in new releases: a developer on stable can
otherwise write code that a pinned-toolchain lint job never sees — `% 2 != 0`
becoming `is_multiple_of` was exactly that. CI deliberately does **not** pass
`--offline` (a cold runner has an empty registry); the local `Makefile` and
`check.ps1` do, because this host cannot reach crates.io. `.github/workflows/release.yml`
builds and checksums the artifacts on version tags.

Both workflows have run on GitHub runners. The runs, what each one caught, and
the eleven real defects CI found that the local gate could not are recorded in
`docs/ci.md`.


The sections below separate evidence from limits on purpose: a measurement
made on this host is not a promise about another machine, and a thing that
was never run is not a thing that works.

## Verified here, with the method

Each of these was produced by a command on this host or on a hosted runner,
and the document named in the paragraph holds the numbers and the way they
were taken.

- Compaction is wired on both surfaces (`hearth chat` before each turn, `hearth web`
  at the top of a turn, both at the Go original's 200-record floor). The web
  surface folds only turns that are already closed, so the summary covers history
  the user can no longer see and lands inside the turn that triggered it — the
  open prompt is projected after the summary, exactly as on the CLI. A summary
  that comes back empty is refused rather than appended (`web_compaction_refuses_an_empty_summary`
  in `crates/cli/src/web.rs`): an empty fold would delete history and record
  nothing in its place.

- Attachment bodies are stored *and* understood. POST /messages writes an upload
  to `<dataDir>/blobs/` and the event keeps a reference (never the bytes: the log
  is append-only and a fork copies it), and `crates/cli/src/attachments.rs`
  turns an `image` reference back into a `ContentPart::Image` when a turn is
  built, so the model receives a `data:` URI and can see the picture. The bytes
  are re-read from the blob store on every turn rather than cached in history,
  so the log stays free of base64 and the picture stays visible for the whole
  session. `read_image` does the same for a file in the workspace. Nothing turns
  a `diff` or a `file` into pixels — those stay references a tool can open.

- Benchmarks are ported in `tools/bench` and three baselines are recorded:
  windows/amd64 from a settled tree on this host, linux/amd64 and darwin/arm64
  from `bench.yml` on GitHub runners. A platform with no record prints a skip
  line rather than comparing against another platform's numbers. The hosted
  Windows numbers are *not* the baseline, because its `fsync` runs 16× slower
  than this host's — a runner is a different machine, not a different day.

- Language-neutral HTTP, event and WebSocket golden corpora are shared with the
  Go oracle and run in both trees. WS caught three real Rust server bugs (delta
  seq, control-ping framing, close code 1000) which are fixed.

- Linux containment (Landlock) is verified at runtime on every push: the Ubuntu CI
  job runs `crates/sandbox/tests/linux.rs`, which writes inside the workspace, is
  refused outside it, is refused a write in `read-only`, is granted an `extra_write`
  path by name, and gets `permission denied` from the kernel with `allow_net = false`
  (and `connected` with it). macOS `sandbox-exec` gets the same treatment: the
  `sandbox macos-latest` CI job runs `crates/sandbox/tests/macos.rs`, which asserts
  the mode matrix, workspace write scope, `read-only`, `extra_write` and both
  network directions on a real macOS kernel.

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
  the path); `hearth doctor --prune-profiles` deletes them — 106 were removed on this
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

- `LICENSE` is MIT. The `UNLICENSED` that `Cargo.toml` carried while this was a
  private port is gone; the tree is redistributable under the terms in `LICENSE`.


## Real limits

These are properties of this build, not backlog items. Closing one means
changing what the artifact claims, so none of them is a bug to be fixed
quietly.

- The Rust provider accepts `https://` through the pinned `webpki-roots` bundle,
  plus an optional `ca_file` PEM (see `crates/llm/README.md`) for a private CA.
  No live request was made to the example endpoint during the port.

- Cross-target release builds from this host are compile checks only; see the
  toolchain section and `docs/release.md`. CI uses native runners for the
  artifacts it claims.

- `archcheck` checks layer direction and the README contract only. It does not
  inspect Rust source, enforce the dependency budget, or check that a crate's
  public API matches its README; those are review and `Cargo.toml` concerns.

- `make check` could not be executed on the authoring host because GNU make is
  not installed. Its component commands were run directly and are the source
  of the expected output above; `check.ps1` runs the same five steps on Windows.


## Caveats about the reference documents

- The frozen contracts (`docs/contracts/`) were written while Go was the
  reference, so their code samples are Go; the behaviour they pin is what the
  Rust crates implement, and the corpora under `tests/conformance/cases/` are the
  proof. `src/` has no `unsafe` (every crate denies it).
