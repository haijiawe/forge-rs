# CI: what was rehearsed, and what was not

Every number here came from a command run on the development host
(Windows 11, `x86_64-pc-windows-msvc`). The honest headline first: **no GitHub
Actions runner has ever executed these workflows.** This checkout has no git
remote and no `gh` CLI, so `ci.yml` and `release.yml` are configurations that
were step-checked locally, not a pipeline anyone has watched go green.

## The three jobs, and how each step was rehearsed

| Job | Step | Local substitute | Result |
| --- | --- | --- | --- |
| `test` | `cargo run -p archcheck` | `cargo run -q -p archcheck --offline` | `archcheck: ok (.)` |
| `test` | `cargo test --workspace` on 1.89 | `cargo +1.89.0 test --workspace --offline` | 64 suites, exit 0 |
| `test` | same, on 1.96 | `cargo test --workspace --offline` | exit 0 |
| `lint` | `cargo fmt --all -- --check` | same | exit 0 |
| `lint` | `cargo clippy --workspace --all-targets -- -D warnings` | same, with `RUSTFLAGS=-D warnings` | exit 0 |
| `web` | `npm ci`, `tsc --noEmit`, `vitest run`, `npm run build` | same, in `web/` | 12 files / 78 tests, 265 kB bundle |
| `web` | bundle budget | `find dist/assets -name '*.js' -printf '%s\n' \| awk ...` | 265,720 B raw, under the 2,000,000 B ceiling |

The `ubuntu-latest` half of the `test` matrix is a compile check, not a test run:
`cargo check --workspace --exclude forge-cli --target x86_64-unknown-linux-gnu`
passes, but this host has no Linux linker, so nothing Linux has ever executed
here. The same goes for `aarch64-apple-darwin`. `docs/08-rust-port.md` §4 records
that constraint; it is not a claim about runtime behaviour.

## Three defects this rehearsal found

The point of running the steps by hand was to catch what a written workflow hides.

1. **The pinned toolchain could not build the dependencies.** `ci.yml` pinned
   `1.85` and `Cargo.toml` said `rust-version = "1.85"`; both were aspirational.
   `cargo +1.85 check --workspace` stops before compiling:
   `libsqlite3-sys@0.40.0 requires rustc 1.88.0`, `rusqlite@0.43.0 requires
   rustc 1.89.0`. The real floor is 1.89, and it is now what both files say.
   Verified by installing 1.89.0 and running the whole suite on it.
2. **`--offline` in CI would fail on the first run.** The flag exists because
   this host's registry is an rsproxy mirror; a fresh runner has an empty
   `~/.cargo/registry` and needs the network. CI no longer passes it — the
   Makefile and `check.ps1` still do, which is where it means something.
3. **Lint and test on the same toolchain hides new lints.** Running clippy on
   1.96 after the 1.89 pin failed on
   `manual implementation of .is_multiple_of()` in
   `crates/sandbox/src/job_windows.rs` and `crates/server/src/server.rs` — lints
   introduced after 1.89. Hence the split: `test` proves the minimum compiler
   builds and passes, `lint` runs current stable where developers actually work.

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

## Not rehearsed at all

- `release.yml` — needs a tag and a runner: cross-compilation of six targets,
  `SHA256SUMS`, `gh release create`. Only its YAML and its `actionlint` pass were
  checked (see `docs/release.md` §8).
- `actions/cache@v4` behaviour, `dtolnay/rust-toolchain` resolution, and whether
  the ubuntu runner has the C toolchain `bundled` SQLite needs.
- Anything the `web` job does on a machine without this host's npm cache.