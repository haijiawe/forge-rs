# Handover — where this is, and what will bite you

This is the document to read on your first day with the tree. It is a record, not
a decision: it describes the state at the handover commit, the things that are
deliberately not built, and the traps that cost someone hours so they do not cost
you the same hours.

Everything here was measured on this box. Where a claim could not be measured
here, it says so instead of guessing.

## What this is

`hearth` is a local-first coding agent harness: one Rust binary that drives a
model over an append-only event log, plus a local web UI served by the same
binary. Every fact about a session is a line in `<dataDir>/sessions/<id>.jsonl`,
in order, never rewritten — see the three laws in [../AGENTS.md](../AGENTS.md).

Twenty-four crates in four layers, enforced by [../architecture.yaml](../architecture.yaml)
and [../tools/archcheck](../tools/archcheck). The frozen surfaces are
[contracts/](contracts/README.md); the corpora under [../tests](../tests) are what
proves the readers keep them.

## State at the handover commit

| | |
| --- | --- |
| HEAD | `f4ccae4`, identical to `origin/master`, working tree clean |
| commits | 88 on `master` (the tree lost and regained its history once — see traps) |
| crates | 24 (`crates/*`), plus `tools/archcheck` and `tools/bench` |
| Rust | 279 tracked `.rs`, about 109k lines including tests |
| tests | 1425 passing, 0 failing, across the workspace (`cargo test --workspace --offline`) |
| binary | `bin/hearth.exe`, 9,180,160 B, `hearth 0.0.0-dev (unknown) windows/amd64 rustc 1.96.0` |
| gate | `check.ps1` → `check: ok` (fmt, clippy `-D warnings`, archcheck, workspace tests) |
| CI | six jobs; last run on this commit green (ubuntu, windows, lint, what-changed; web and macos skipped by design) |
| licence | MIT (`LICENSE`, `Cargo.toml`) |

`bin/hearth.exe` is checked in and is **not** a cargo artifact: it embeds the
built web bundle. See "rebuilding the binary" below before you trust it.

## Where things live

```
D:\hearth            this tree (git repo, remote haijiawe/hearth)
D:\reference         read-only material: DSH rc.5 source, brand assets, tooling
D:\_agent-scratch    archived logs from earlier sessions (safe to delete)
```

`D:\reference\deepseek-harness-master` is the DSH 0.2.2-rc.5 source. It is the
reference for [dsh-vs-hearth.md](dsh-vs-hearth.md) and
[agent-comparison.md](agent-comparison.md); nothing in this tree imports it.

## Build and verify

```powershell
cargo test --workspace --offline
cargo run -q -p archcheck --
pwsh -NoProfile -File .\check.ps1      # all gates, in CI order; ~40 s warm
```

Two facts, both measured, both non-negotiable:

- **MSVC only, offline.** The GNU toolchain is installed but broken
  (`dlltool.exe: program not found`) and crates.io is unreachable; the rsproxy
  mirror is what `--offline` reads.
- **The build path must be ASCII.** `D:\hearth` is fine and nothing needs to be
  set. If you move the tree somewhere with a non-ASCII character in the path,
  point `CARGO_TARGET_DIR` at an ASCII directory first or every crate that
  compiles C fails with `cl: D8050`.

### Rebuilding the binary

```powershell
cd web; npx vite build; cd ..
cargo build --release -p hearth-cli
Copy-Item .\target\release\hearth.exe .\bin\hearth.exe -Force
```

The first step is not optional: `hearth-cli` embeds `web/dist`, so a binary built
without it serves the previous UI. `npx tsc --noEmit` and `npx vitest run` live
in `web/` and must be run from there — from the repo root, `npx` finds the wrong
`tsc`.

## Publishing

`git push` and `git fetch` both die against github.com:443 on this network
(connection reset). `origin/master` is maintained through the REST API instead:

```powershell
node scripts/sync.cjs "commit message"   # commit, push, align
node scripts/sync.cjs --no-commit        # push + align the current HEAD
node scripts/sync.cjs --dry "message"    # say what would happen, write nothing
```

The third step is the one people forget. A commit created through the API is a
new object, so its SHA differs from the local one even when the tree is
byte-identical; `sync.cjs` rebuilds that object locally (the API normalises
timestamps to `Z` and drops the message's trailing newline, so it tries both
dimensions) and **refuses to move the branch** unless the rebuilt object hashes
to the SHA GitHub reported. It reads the token from `$GITHUB_PAT`, or from
`GITHUB_PAT:` in `$HEARTH_CREDENTIALS` (default `D:\dsh\.credentials.yaml`), and
never prints it.

If you ever get a working `git push`, delete `scripts/sync.cjs` and go back to
plain git — the script exists to work around one machine's network.

## What is built

Every wave below is closed and has its own record in this directory. "Closed"
means the acceptance criteria in the record were run, not that the feature is
exhaustive.

| Wave | What it added | Record |
| --- | --- | --- |
| M0–M7 | the port itself: envelope, log, projection, tools, sandbox, server, CLI, release pipeline | [08-rust-port.md](08-rust-port.md) |
| P1–P5 | `hearth-net`, `web_fetch`/`web_search`/`worktree`/`present`/`read_image`, attachments out of the log, capabilities endpoint, plugins, market, expert rosters, teams | [09-platform-tier.md](09-platform-tier.md) |
| W1–W6 | structured subagent output, goal revision/CAS, compaction `surfaceOp` + pruner, two rewrite seams, team task graph | [10-agent-parity.md](10-agent-parity.md) |
| A–E | header-level delegation depth, per-step goal fence, compaction transaction, `request-error` seam, cross-process `interrupt` | [10-agent-parity.md](10-agent-parity.md) |
| S1–S5 | subagent identity (`catalog`/`descriptor`), bounded pool, cold resume, settled notifications, out-of-process worker backend | [12-subagent-port.md](12-subagent-port.md) |
| G1–G8 | attempt accounting, general listener surface, resident phase machine, persistent inbox queues, steer, async settlement, named backends, team board projection | [agent-gaps.md](agent-gaps.md) |
| H1–H7 | `request/context`, `cancelledBy` causes, goal `domain`/`fold`/`runtime` split, queue drain, session archive, immutability evidence, one resident agent per web session | [agent-gaps.md](agent-gaps.md) |
| I1–I3 | incremental projection behind a byte cursor, incremental transcript scan, and the workspace read API (`files`/`file`/`diff`) | this file, §"The IDE turn" |
| D1 | the Tauri 2 desktop shell, and the three-column workbench it opens | [apps/desktop/README.md](../apps/desktop/README.md) |

### The IDE turn

After the handover the work turned towards a workbench rather than a chat page,
which changed three things and left the harness alone:

- **A turn no longer re-reads the whole log.** `Agent::snapshot` used to call
  `read_all` and re-project every event each turn — measured at 88,916 events/sec
  on this host, so 100,000 events cost 1.8 s *before* the model was asked
  anything, and 500,000 cost 9 s. `Store::read_after` plus a `Projection` held
  behind a byte cursor makes a turn cost what the turn added, and a torn trailing
  line is left for the next call instead of being consumed.
- **The transcript is scanned incrementally.** `compressedBlocks` walked every
  event on every animation frame; `collectBlocks`/`blocksFromScan` keep a scan
  position, and the `toBeyondHead` verdict is recomputed rather than cached
  because it flips as the head grows.
- **The window can read the workspace** over `GET /api/sessions/{id}/files`,
  `/file` and `/diff`, which resolve the workspace from `session/start`, refuse
  `..` and symlink escapes, cap text at 1 MiB, and never write.

## What is deliberately not built

These are decisions, not backlog. Closing one changes what the artifact claims,
so each needs its own ADR rather than a quiet commit.

- **A pure-Rust Office writer.** The `office` skill drives an external program
  and says so; [office-run.md](office-run.md) records what was produced.
- **The non-goals in [09-platform-tier.md](09-platform-tier.md) §6**: in-browser
  terminal multiplexer, ACP agent protocol, Office *rendering*, image generation
  models, and any DSH/DS server dependency. This tree is in-process Rust by
  design. The desktop shell was on that list and was built later on request; the
  editor it opens stays read-only, because a write from the window would bypass
  the policy, the approval prompt and `tool/call`.
- **`archive-admission` as DSH implements it** was replaced by a real
  `session/archived` event plus checks at the two doors where new work enters
  ([agent-gaps.md](agent-gaps.md) explains why there is no veto surface here).

## Real limits — claims this build does not make

- `gh release create` in `.github/workflows/release.yml` has **never executed**.
  Cross-target builds from this host are compile checks; CI uses native runners.
- Kernels older than the CI ubuntu image and macOS versions older than the runner
  are unmeasured.
- The ETXTBSY spawn retry is verified on CI's ubuntu only. Windows and macOS take
  the pass-through branch, so a green local gate proves "no regression", not
  "the retry works".
- `scripts/sync.cjs` depends on the API's metadata reproducing a commit object
  exactly. If GitHub changes that formatting it fails closed (no branch move) and
  needs a new candidate format, not a hand-edited SHA.
- No live request was made to the example model endpoint during the port; the
  provider is exercised against scripted local servers (see [../README.md](../README.md)
  "Real limits" for the full list).
- **`web/src/api/protocol.ts` is no longer generated.** It mirrors
  `crates/protocol/src/protocol.rs` and the two must change in one commit; the Go
  generator that used to produce it did not come across. Two frozen documents
  ([contracts/m6-surface.md](contracts/m6-surface.md) and
  [contracts/api.md](contracts/api.md) §1) still name `go run ./scripts/gen-protocol`
  because they describe the Go oracle, which is what they were frozen against;
  they are not instructions for this tree.

## Traps that already cost someone hours

Read these before your first edit. Each one is a real incident, not a warning in
the abstract.

1. **Never build Rust or Markdown strings with PowerShell string interpolation.**
   Backtick and backslash are escapes there, and the damage is silent: a lone
   CR, BEL (`0x07`) or FF (`0x0C`) landed in `agent.rs` and `cli/lib.rs` comments
   and survived fmt, clippy and review because `clippy` does not inspect those
   bytes. Use the editor tools or a script file. If you suspect it,
   `Select-String -Pattern '[\x00-\x08\x0b\x0c\x0e-\x1f]' -Path crates -Recurse`.
2. **`ETXTBSY` on Linux.** `exec` is refused while *any* process holds the file
   open for writing, and a sibling thread's `fork` inherits the writer's
   descriptor for the instant before `exec`. `spawn_retrying_busy` in
   `crates/plugin/src/connection.rs` retries for a second. The same shape will
   appear anywhere a freshly written binary is run immediately.
3. **Parallel test flakes that are not bugs.** `hearth-mcp --test http` and
   `hearth-ws --lib` can fail inside `check.ps1` (port races) and pass alone.
   Re-run before you "fix" them. Fixing a flake by loosening an assertion is how
   a real regression gets in.
4. **`Event::new` leaves `seq = 0`.** A log you write by hand must assign dense
   `seq`, a 32-hex `id` and a millisecond `ts` itself, or the reader reports
   `not dense` and the test looks like "the record was never written".
5. **`Config::load` does not call `validate`.** Negative-value and contradiction
   tests must call `cfg.validate()` explicitly.
6. **Moving the tree can destroy `.git`.** `Move-Item` with the shell's cwd
   inside the source degrades to a half-finished copy; deleting the副本 took
   `.git` with it and 83 local commits had to be rebuilt from the remote. Move
   the cwd out first, then compare file counts and sample hashes, and check
   `.git` separately.
7. **The rename left the docs consistent, the files sometimes not.** When
   `forge` became `hearth`, prose references were rewritten but a file name was
   not, so the index pointed at `dsh-vs-hearth.md` while the file was still
   `dsh-vs-forge.md`. If you rename anything, grep for both spellings afterwards.
8. **`cargo fmt` reformats the test you are about to edit.** Read the file back
   after formatting before you anchor an edit on an exact line.
9. **A CSS rule and a `className` that disagree fail silently.** The tool card's
   head was `className="tool-head"` with the stylesheet defining `.tool-card-head`,
   and `.tool-callid` against a JSX `tool-call-id`; nothing errors, the `gap`
   simply never applies, and the rendered result was `fs_writecall_smoke_1`.
   Layout bugs that do not throw are found by looking at a screenshot, not by
   reading either file.
10. **A flex item will not shrink below its longest word.** `.session-head` sits in
    a flex row with the default `min-width: auto`, so one long error message
    pushed the whole document wider than the window and shoved the agent panel off
    screen. `min-width: 0` plus `overflow: hidden` on the growing item is the fix;
    a CSS audit will not find this, a screenshot at the real window size will.
11. **A WebView2 window must be created on the thread that runs the event loop.**
    Building it from a worker thread produces a window that loads the page
    completely — `#root` had 7,819 bytes of markup, 267 CSS rules, an empty error
    list, all read through the debugging port — and paints pure black forever.
    Waiting for a child process off the main thread is fine; creating the window
    there is not, so `apps/desktop` hops back with `run_on_main_thread`.
12. **A screenshot can agree with the wrong layer.** The same page rendered
    correctly in headless Edge while the desktop window was black, which said the
    UI was fine; the opposite trap is reading a blank DOM and blaming the browser.
    When a picture and the API disagree, query the page itself (`--remote-debugging-port`
    plus `Runtime.evaluate`) before changing either side.

## First three things to do

1. `pwsh -NoProfile -File .\check.ps1` — if it is not `check: ok`, stop and fix
   the environment before reading any code.
2. `.\bin\hearth.exe doctor` and `.\bin\hearth.exe web --addr 127.0.0.1:8811`
   — the UI is the fastest way to see whether the log, the projection and the
   tools agree. The token is printed to **stderr**.
3. Read [agent-gaps.md](agent-gaps.md) before planning anything. It is the ledger
   of what DSH has that this tree does not, with a cost next to each item, and it
   is the only place that comparison is written down.

## Handing it back

The shape this tree expects, from [../AGENTS.md](../AGENTS.md): report `done`
(files + line counts), `evidence` (command and output tail), `open` (what is not
done and why), `risk` (what could bite the next person). A gate you did not run
is not a gate that passed.