# CI: what runs, what it caught, and what is still not rehearsed

The repository is `github.com/haijiawe/forge-rs` (private) and the tree is MIT.
Every workflow here has now executed on hosted runners, including `release.yml`
(through its `workflow_dispatch` dry-run mode) and the manual `bench.yml`.

The honest headline: **the first twelve CI runs were not a formality.** Each one
found something the Windows development box could not see, and several were
defects in tests that would have kept hiding indefinitely on a single machine.

## The jobs

| Job | Runner | What it proves |
| --- | --- | --- |
| `test` | ubuntu-latest, windows-latest | builds and runs the whole suite on the minimum supported rustc (1.89, set by `rusqlite`) |
| `sandbox-macos` | macos-latest | runs `crates/sandbox/tests/macos.rs` on a real kernel: mode matrix, write scope, `read-only`, `extra_write`, network both ways |
| `lint` | ubuntu-latest | `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` on current stable |
| `web` | ubuntu-latest | `npm ci`, `tsc --noEmit`, `vitest run`, `vite build`, and the 2,000,000-byte bundle ceiling |

Two toolchains on purpose: `test` proves the floor, `lint` runs where developers
work. A pinned-toolchain lint job cannot see a lint that only a newer clippy
knows (`is_multiple_of` was exactly that kind of lint).

## Every run it took, and what each one caught

| Run | Head | lint | web | ubuntu | windows | caught |
| --- | --- | --- | --- | --- | --- | --- |
| 44528486442 | `4947037` | ✗ | ✓ | ✗ | ✗ | 1, 2, 7 |
| 44538730334 | `2b36d70` | ✗ | ✓ | ✗ | ✗ | 2, 3 |
| 44545921397 | `ddb2a3d` | ✓ | ✓ | ✗ | ✗ | 4, 5 |
| 44556272262 | `5732bce` | ✓ | ✓ | ✗ | ✗ | 6 |
| 44558234256 | `7e6e25f` | ✓ | ✓ | ✓ | ✓ | — |
| 44564786342 | `46e9188` | ✓ | ✓ | ✓ | ✗ | 8 |
| 44582690666 | `0065fff` | ✓ | ✓ | ✓ | ✗ | 9 |
| 44585759279 | `d05c4c5` | ✓ | ✓ | ✓ | ✓ | — |
| 44596952813 | `fe762e9` | ✓ | ✓ | ✓ | ✓ | — |
| 44603497179 | `58e6046` | ✓ | ✓ | ✗ | ✓ | 10 |
| 44606471925 | `7aaf73f` | ✓ | ✓ | ✓ | ✓ | — |
| 44618571269 | `f56bda3` | ✓ | ✓ | ✓ | ✓ | — (first run of `sandbox macos-latest`, green) |

The ten defects, in the order they surfaced:

1. **A pinned target directory is not portable.** `.cargo/config.toml` carried
   `target-dir = "D:/forge-rs-target/forge-rs"` — a workaround for MSVC error
   `D8050` when the source path contains non-ASCII characters. On Linux that is
   one path segment containing a colon, and the ubuntu job died with
   `failed to join paths from $LD_LIBRARY_PATH together`. The config now pins no
   target directory at all; `check.ps1` derives an ASCII one only when the
   checkout path needs it, and the workflows set `CARGO_TARGET_DIR`.
2. **Two clippy lints newer than the development toolchain**: `needless_late_init`
   in `crates/session/src/fork.rs` and `chunks_exact_to_as_chunks` in
   `crates/server/src/server.rs`. Both rewritten to the lint's preferred form.
3. **A variable used only under `#[cfg(windows)]`** in `crates/cli/src/doctor.rs`
   made the non-Windows build warn (`unused variable: stderr`), which `-D warnings`
   turns into a failure.
4. **A manifest path does not mean the same thing on two platforms.**
   `C:\evil.js` is absolute on Windows and an ordinary filename on Unix, so the
   containment check accepted it there. A bundle would then point at two
   different files depending on where it was installed. `entry_stays_inside` now
   refuses backslashes and drive prefixes on every platform; `/` is the only
   separator a manifest may use.
5. **`TEMP` can be an 8.3 short name.** The hosted Windows runner hands out
   `C:\Users\RUNNER~1\...`. Rust creates a file through it happily; the Node
   child, given the same string, reported `Cannot find module`. Test scratch
   roots are canonicalised once (and the `\\?\` prefix trimmed) so both sides
   see one long path.
6. **A test cleaned up its parent.** The bridge tests used `root.parent()` — the
   system temp directory itself — as `data_dir`, then removed it at the end.
   Windows refused to delete open files, so nothing bad showed locally; the
   Linux runner owns `/tmp` and really deleted it, taking every other test's
   scratch directory with it. Each test now owns `<base>/plugins` or
   `<base>/experts` and removes only its own base.
7. **The plugin host reported the wrong last stderr line.** A failed Node child
   ends its output with `Node.js v22.23.4`, so the host's one-line summary was a
   version number. It now prefers the last line that starts with `Error` or
   contains `Error: `. That change is what made defect 5 legible at all.
8. **A fixture that answers before reading the request.** `crates/net`'s test
   server read the request head and replied without draining the POST body. On
   Windows, closing a socket that still holds unread input sends RST, and the
   RST can discard the response the client has not read yet; the ubuntu job
   passed while windows failed with `os error 10054`.
9. **A deadline that covered the handshake.** The plugin timeout test set
   200 ms before `load`, so starting Node had to fit inside it. On a hosted
   Windows runner it did not. The deadline now covers only the call that is
   supposed to hang, which is what the test is named after.
10. **An assertion that compared a different spelling than the code printed.**
    The first real Ubuntu run of the new Landlock tests had four of five pass;
    the fifth failed with `got "refused: permission denied"` — the kernel *had*
    refused the connect, but the assertion looked for Rust's Debug spelling
    `PermissionDenied` while the helper prints the Display form. The comparison
    is now case-insensitive on the text. Worth keeping in the list because the
    product was right and the test was wrong: a red CI run is a claim to
    investigate, not a verdict.

Defects 4–9 are all one class: **a second machine is a second reader.** A test
that passes on Windows because a delete silently fails, because a backslash is
special, or because the disk is fast enough, is not a test of the thing it names.

## The bench workflow

`bench.yml` is `workflow_dispatch` only, because runner hardware is shared and a
baseline is only comparable within one platform. It records one report per
runner (ubuntu-latest, windows-latest, macos-latest) with

```
cargo run --release -q -p forge-bench -- -out "baseline-<os>.json" -baseline ""
```

and uploads each as an artifact. To arm the gate on a new platform, download the
artifact, copy its single record into `scripts/bench/baseline.json` (the file is
a `{version, baselines: [...]}` set, and `forge-bench` compares only the record
matching `os`/`arch`), and commit it with the measurement date. The final step of
the workflow runs `-strict` with `continue-on-error: true`: a run that cannot be
compared yet is not a failure, it only reports.

The workflow's first two runs each found one defect, and both were invisible on
a developer machine that has a warm target directory and a working registry:

1. **The inner build was pinned offline.** `forge-bench` shells out to
   `cargo build -p forge-cli` to have a binary to launch, and that call always
   passed `--offline`. A dev box has every crate cached; a hosted runner starts
   with an empty registry and died on `aho-corasick`. The flag is now
   conditional on `FORGE_BENCH_OFFLINE=1`, so network policy belongs to the
   caller (the Makefile sets it, the workflow does not).
2. **The build was inside the measurement budget.** `version_startup` pays for
   that build on a cold runner; it took about sixty seconds and exhausted the
   harness's sixty-second runtime budget, so the second run recorded exactly one
   metric out of six. `run_all` now builds once before starting the clock, and
   the artifact has all six.

`record macos-14` was originally `cancelled` with no steps and no runner name:
that label could not be assigned to this account, while `release.yml`'s
`macos-latest` job did run on 2026-11-20. The bench matrix therefore asks for
`macos-latest` now; whether a `darwin/arm64` record can be produced is decided
by the next dispatch, not by assuming the labels are interchangeable.

## A flake that was made into a wait

`http_surface_matches_oracle_protocol_and_event_contract` in
`tests/e2e/tests/acceptance.rs` failed twice, then passed on more than six
consecutive full runs, with no captured `left`/`right` values. The assertion
compared `page["lastSeq"]` with `page["events"].len()`. Both come from
`handle_events` (`crates/server/src/server.rs`), which calls `fold_session`
(reads the whole log) and then `read_raw_events` (reads it again) inside one
request — so a write landing between the two reads makes them disagree for a
moment, and the contract's answer to disagreement is to page again with
`nextAfter`.

The test now re-polls until the two agree, bounded by the same 20 s deadline it
already used, and fails with both values printed if they never do. That is a
weaker instant assertion and a stronger statement about the contract: after
`turn/end` the log is quiescent, so agreement is permanent, and a permanent
disagreement is still a failure.

## Rehearsed once, and what it still does not prove

- **`release.yml` has now run for real.** A `workflow_dispatch` dry run
  ([44588872748](https://github.com/haijiawe/forge-rs/actions/runs/44588872748))
  built, verified and uploaded all three native artifacts —
  `forge-windows-amd64.exe` 4,183,350 B, `forge-linux-amd64` 4,584,901 B,
  `forge-darwin-arm64` 3,985,780 B — and generated a `SHA256SUMS` that passed
  `sha256sum --check`. The `Publish GitHub release` step was skipped because the
  ref was not a tag. That proves the build path; it does not prove
  `gh release create` itself, which needs a real `v*` tag and cannot be undone.
- Anything about a machine that is neither Windows nor the runners above, e.g.
  Linux sandbox confinement under a non-Ubuntu kernel, or macOS versions other
  than the `macos-latest` image (which is Apple Silicon).
- A macOS *bench* record: the matrix now asks for `macos-latest` because
  `macos-14` could not be assigned, but no dispatch has produced a record yet.
