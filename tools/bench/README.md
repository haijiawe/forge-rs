---
dir: tools/bench
layer: L3
role: six-metric performance harness and per-platform baseline gate
public: [hearth-bench binary, baseline JSON shape, five flags, gate policy]
may_import: [hearth-core, hearth-session, serde, serde_json]
must_not: [edit crates/**, edit Makefile, edit Cargo.toml, add third-party crates, tokio]
gate: cargo fmt -p hearth-bench -- --check && cargo clippy -p hearth-bench --all-targets --offline -- -D warnings && cargo test -p hearth-bench --offline
---

# hearth-bench

The Rust port of the retired Go tree's `scripts/bench`. Six metrics, one baseline
document, one rule: **only `-strict` fails a regression**.

## 做什么

- Measure `version_startup` (ms, lower), `session_append_fsync`,
  `session_append_batch`, `session_replay_10k`, `session_projection_10k`
  (events/sec, higher) and `event_roundtrip` (ns/op, lower).
- Run 20 phases per metric (5 for the codec). Publish p50, p95 and the value
  from the **fastest** phase; tolerance sits beside every value: 0.35 for the
  two fsync/batch append metrics, 0.10 elsewhere.
- Merge a run into a language-neutral baseline document, keyed by `os`/`arch`,
  and compare **only** the record for the current platform.

```
cargo run --release -q -p hearth-bench --offline --                          # report
cargo run --release -q -p hearth-bench --offline -- -strict                  # CI gate
$env:HEARTH_BENCH_OFFLINE = "1"   # only on the offline development box
cargo run --release -q -p hearth-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

`--release` is part of the measurement, not a speed hint: four of the six
metrics run inside this process, and a debug build lands 4–15× below the
release baseline (91,792 vs 2,840 ns/op for `event_roundtrip` on one machine).

The harness builds `hearth-cli` with its own `cargo build`; `HEARTH_BENCH_OFFLINE=1`
makes that inner build pass `--offline`. Hosted runners leave it unset — an empty
registry plus `--offline` is how the first bench workflow failed.

## 公开面

- Binary `hearth-bench`; flags `-out`, `-baseline`, `-fail-over`, `-strict`,
  `-require-baseline` (defaults: empty, `scripts/bench/baseline.json`, 10,
  off, off).
- Baseline shape: `{"version":1,"baselines":[{os,arch,rustVersion,timestamp,
  results:[{name,unit,value,samples,p50,p95,tolerance}]}]}`. A legacy single
  report at the top level is accepted and wrapped.
- Library modules `args`, `baseline`, `gate`, `metrics`, `platform`, `stats`,
  `time` are public for unit tests; `docs/bench.md` is the operator guide.

## 依赖与边界

- All session work goes through `hearth-core` (`Event`, `EventType`, canonical
  JSON) and `hearth-session` (`Store`, `Options`, `Projection`). No new
  third-party dependency: `serde`/`serde_json` are existing workspace entries
  and are used only for the baseline document and the 4KB payload.
- `Projection::fold` replaces Go's `session.Project(path)`: the log is read
  explicitly and the read is inside the timed phase.
- The `hearth` binary is located from `cargo build --message-format=json`, so a
  custom `CARGO_TARGET_DIR` works. The tool walks up to `Cargo.toml` +
  `architecture.yaml` to find the repo root.
- A missing baseline document or platform record prints an honest skip and
  exits 0, even under `-strict`; `-require-baseline` makes it exit 1. Removed
  and new rows never fail. Report-mode regressions exit 0.

## 不要做

- Do not record a baseline from a noisy/unsettled build; `-baseline ""` is the
  explicit recording mode and must be deliberate.
- Do not compare across platforms, architectures or metric names.
- Do not turn a skip into `OK`, or a report-mode regression into exit 1.
- Do not add a benchmark CLI framework, async runtime or new workspace
  dependency; do not edit `Makefile`, root `Cargo.toml`, `architecture.yaml`,
  `docs/08-rust-port.md` or `crates/**` from this crate.
