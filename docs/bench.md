# forge-bench — performance harness and baseline gate

Rust port of `archive/forge-go/scripts/bench`. The Go tree stays the oracle: metric
names, units, tolerances, document shape and gate policy are preserved, with
`goVersion` renamed to `rustVersion`.

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

## Running and integrating

```powershell
$env:CARGO_TARGET_DIR = "D:\forge-rs-target\forge-rs"
cargo fmt -p forge-bench -- --check
cargo clippy -p forge-bench --all-targets --offline -- -D warnings
cargo test -p forge-bench --offline
cargo run -q -p archcheck --offline

# report / gate / deliberate recording (never record from a noisy build)
cargo run -q -p forge-bench --offline --
cargo run -q -p forge-bench --offline -- -strict
cargo run -q -p forge-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

Required integration (outside this crate's write scope, done by the lead):

- root `Cargo.toml` workspace `members`: add `"tools/bench"`;
- `Makefile`: add `bench-run`, `bench-record`, `bench-check` targets calling
  the three `cargo run` lines above, and add `bench-check` to `check` if CI
  should gate on regressions;
- update the stale line in `README.md` ("no benchmarks yet") and the W6 row in
  `docs/08-rust-port.md` once a baseline has been deliberately recorded.

## Boundaries

The crate may import `forge-core` and `forge-session` only. Everything else
is std plus the workspace's existing `serde`/`serde_json` (baseline document
and 4KB payload). No CLI framework, no async runtime, no new workspace
dependency. The tool never edits `crates/**`, the root `Cargo.toml`, the
`Makefile`, `architecture.yaml` or `docs/08-rust-port.md`.
