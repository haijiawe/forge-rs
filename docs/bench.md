# forge-bench — performance harness and baseline gate

Rust port of the retired Go tree's `scripts/bench`. That tree was deleted on
2026-11-17, but its decisions stayed: metric names, units, tolerances, document
shape and gate policy are preserved, with `goVersion` renamed to `rustVersion`.

## Why a second harness

`cargo bench` measures functions inside one crate. This harness measures the
user-visible product: the `forge` executable, the session log on disk, and the
JSON codec. It writes one language-neutral document so a Rust run and a Go run
can be compared on the same platform without sharing a schema.

## Flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `-out <path>` | empty | merge this run into the document at `<path>` (repo-relative) |
| `-baseline <path>` | `scripts/bench/baseline.json` | compare against this document; `""` records without comparing |
| `-fail-over <pct>` | `10` | fallback tolerance when a row has no `tolerance` |
| `-strict` | off | exit 1 when a metric regresses beyond tolerance |
| `-require-baseline` | off | exit 1 when this platform has no record |
| `-h`, `-help` | — | print usage and exit 0 |

`-flag value`, `-flag=value`, `--flag value` and `--flag=value` all work, as in
Go's `flag` package. Parsing stops at the first positional argument.

## Metrics

| Name | Unit | Better | Workload |
| --- | --- | --- | --- |
| `version_startup` | ms | lower | 20 cold `forge --version` launches, one per phase |
| `session_append_fsync` | events/sec | higher | 200 appends, one fsync per event, 20 phases |
| `session_append_batch` | events/sec | higher | 2,000 appends, `sync_every=100`, 20 phases |
| `session_replay_10k` | events/sec | higher | one 10,000 × 3,800-byte log; reopen and read back, 20 phases |
| `session_projection_10k` | events/sec | higher | the same log folded with `Projection::fold`, 20 phases |
| `event_roundtrip` | ns/op | lower | 5 phases × 1,000 canonical marshal + parse of one 4KB event |

Each phase produces one sample. `value` is the fastest phase (minimum for
latencies, maximum for throughput); `p50`/`p95` are nearest-rank percentiles.
Samples below half the median are dropped before summarising, but only while at
least two samples remain. Values are rounded to four decimals. A phase whose
clock does not advance is retried up to six times; a failed phase is an error,
not a zero. The whole run has a 60-second budget and skips later metrics with
an explicit line rather than publishing a partial number as complete.

Tolerances are stored **per row**: `0.35` (35%) for `session_append_fsync` and
`session_append_batch`, `0.10` (10%) for the other four. `-fail-over` is used
only when a baseline row has `tolerance == 0`.

The numbers below are placeholders for the field shape; no baseline has been
recorded yet. Record one only from a settled build.

```json
{
  "version": 1,
  "baselines": [
    {
      "os": "windows",
      "arch": "amd64",
      "rustVersion": "rustc 1.85.0 (…)",
      "timestamp": "2026-01-01T00:00:00Z",
      "results": [
        {
          "name": "version_startup",
          "unit": "ms",
          "value": 0,
          "samples": 20,
          "p50": 0,
          "p95": 0,
          "tolerance": 0.1
        }
      ]
    }
  ]
}
```

Rules, all unit-tested in `tools/bench/src/baseline.rs`:

- version must be 1; the set must be non-empty; every record needs `os`,
  `arch` and at least one result. A legacy top-level single report is accepted
  and wrapped in a one-record set.
- selection matches `os` **and** `arch`; the newest RFC 3339 timestamp wins,
  with offset-aware instant comparison. A parseable timestamp beats an
  unparseable one; a tie keeps the first record.
- upsert replaces the first matching record in place, drops later duplicates
  and appends a new record when the platform was absent. The writer refuses
  to overwrite an unparseable file.
- marshalling is two-space indent, LF only, stable field order, exactly one
  trailing newline.

Rust arch names are mapped to Go names in the document (`x86_64` → `amd64`,
`aarch64` → `arm64`, `x86` → `386`) so both implementations share one key space.

## Gate policy

1. `-baseline ""` → recording notice, no comparison, exit 0.
2. No document at `-baseline` → `No baseline file at …`, nothing to compare;
   exit 0 even with `-strict`, exit 1 only with `-require-baseline`.
3. Document exists but has no record for this platform → print the honest skip
   line plus the platforms it does record; same exit rule as (2). `-strict`
   adds a note explaining that nothing was gated and why exit is still 0.
4. Record found → compare rows by `name`. `"events/sec"` and `"MB/s"` are
   higher-is-better; every other unit is lower-is-better. `pct =
   (current - baseline) / baseline * 100`, inverted for higher-is-better;
   `REGRESSED` when worse exceeds the baseline row's tolerance. A zero baseline
   value is `SKIP`, never `REGRESSED`. New and removed rows are reported but
   never fail.
5. Regressions exit 0 in report mode. Only `-strict` exits 1, with a count.
6. A differing `rustVersion` is noted; the gate still applies.

## The Windows record, and why it was refreshed

`scripts/bench/baseline.json` originally held one record, taken on 2026-11-16
against the M7 tree (7.5 MB binary, ten crates). The current tree is 8.5 MB and
has six more crates by design. The record was re-taken on 2026-11-19 after the
machine had settled; the deltas are written down here rather than absorbed
silently into a tolerance:

| metric | 2026-11-16 | 2026-11-19 | delta |
| --- | --- | --- | --- |
| version_startup | 5.14 ms | 6.37 ms | +24% slower |
| session_append_fsync | 4,501 /s | 4,354 /s | −3% |
| session_append_batch | 76,942 /s | 89,477 /s | +16% faster |
| session_replay_10k | 159,766 /s | 148,230 /s | −7% |
| session_projection_10k | 92,313 /s | 84,630 /s | −8% |
| event_roundtrip | 2,843 ns/op | 2,849 ns/op | +0.2% |

`event_roundtrip` matching within 0.2% says the machine and the release profile
are the same ones that produced the old record; it does not explain the
startup row. The six added crates are the obvious candidate, but that has not
been isolated, and the honest statement is that it has not. Startup at 6.4 ms
is still below the retired Go tree's 7.2 ms and far below a JS harness.
The record's job is to catch regressions from *today's* tree; keeping a stale
record produces a gate that is red for reasons nobody intends to fix.

## The Linux record, and why hosted numbers are informational

`linux/amd64` was recorded on 2026-11-19 by `bench.yml` on `ubuntu-latest`
(rustc 1.100.0). Two runs of the same job on the same runner label, an hour
apart, disagreed by more than a factor of two on the I/O metrics:

| metric | run A | run B |
| --- | --- | --- |
| session_append_batch | 77,828 /s | 37,756 /s |
| session_replay_10k | 77,310 /s | 135,271 /s |

The committed record is run B, taken with the single-build harness. Shared
runners tell you the shape of a platform, not a metronome; the workflow's
`-strict` step is `continue-on-error` for this reason, and a Linux record from a
machine you control is what should gate Linux work.

The `windows-latest` artifact from the same workflow is not committed: its
`session_append_fsync` was 368/s against 4,354/s on the machine that produced
the Windows record, and a later dispatch measured 274/s. Hosted Windows disks
are that much slower, and putting two machines under one `windows/amd64` key
would make the gate meaningless.

A third dispatch (2026-11-20, run 44621655899) produced yet another Linux
record — `session_append_batch` 35,322/s, `session_replay_10k` 125,059/s. It was
not adopted: swapping a hosted record for another hosted record every dispatch
turns the file into a changelog of runner noise. Run B stays, because it is the
one described in the table above.

## The macOS record, and why it exists at all

`macos/arm64` was recorded on 2026-11-20 by `bench.yml` on `macos-latest`
(rustc 1.100.0, Apple Silicon):

| metric | value |
| --- | --- |
| version_startup | 4.79 ms |
| session_append_fsync | 1,715 /s |
| session_append_batch | 42,315 /s |
| session_replay_10k | 150,170 /s |
| session_projection_10k | 123,629 /s |
| event_roundtrip | 3,775 ns/op |

This is a hosted number and inherits every caveat above — but it is the *only*
macOS record there is, so the choice was between a wobbly reference and no gate
at all on that platform. It is committed for the second reason, and the
tolerance on the I/O rows (35%) is wide enough to absorb the wobble the Linux
runs demonstrated.

Getting it required changing the matrix from `macos-14` to `macos-latest`: the
first dispatch asked for `macos-14` and the job was cancelled with no runner
assigned (`runner_id: 0`, empty steps), while `release.yml`'s `macos-latest`
job had run fine the day before. The two labels are not interchangeable.

## Running and integrating

```powershell
$env:CARGO_TARGET_DIR = "D:\forge-rs-target\forge-rs"
cargo fmt -p forge-bench -- --check
cargo clippy -p forge-bench --all-targets --offline -- -D warnings
cargo test -p forge-bench --offline
cargo run -q -p archcheck --offline

# report / gate / deliberate recording (never record from a noisy build)
cargo run --release -q -p forge-bench --offline --
cargo run --release -q -p forge-bench --offline -- -strict
cargo run --release -q -p forge-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

`--release` is not an optimisation hint, it is part of the unit. Four of the six
metrics are measured inside this process; a debug build measures the debug
profile and lands 4–15× below every recorded number (`event_roundtrip` was
91,792 ns/op in debug against 2,840 ns/op in release on the same machine).
`Makefile`'s `bench-*` targets and `.github/workflows/bench.yml` both pass it;
an operator following a command that does not pass it will see a regression for
every metric and none of them will be real.

Integration is in place: `tools/bench` is a workspace member, the `Makefile`
has `bench-run` / `bench-record` / `bench-check`, and `bench.yml` records one
artifact per runner. The workflow is manual (`workflow_dispatch`) so recording
never rides along with an ordinary push.

## Boundaries

The crate may import `forge-core` and `forge-session` only. Everything else
is std plus the workspace's existing `serde`/`serde_json` (baseline document
and 4KB payload). No CLI framework, no async runtime, no new workspace
dependency. The tool never edits `crates/**`, the root `Cargo.toml`, the
`Makefile`, `architecture.yaml` or `docs/08-rust-port.md`.
