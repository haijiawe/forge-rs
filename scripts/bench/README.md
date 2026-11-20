# scripts/bench

This directory holds the committed performance baseline document,
`baseline.json`, when one is deliberately recorded. The harness itself lives
in `tools/bench`; see `docs/bench.md` for flags, metrics and gate policy.

The default `-baseline` path points here. A missing file is an honest skip,
not an error: the report prints `No baseline file at …` and exits 0, even
under `-strict`.

Record only from a settled build:

```powershell
$env:CARGO_TARGET_DIR = "D:\hearth-target\hearth"
$env:HEARTH_BENCH_OFFLINE = "1"   # the harness shells out to `cargo build hearth-cli`
cargo run --release -q -p hearth-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

`HEARTH_BENCH_OFFLINE` exists because the harness builds `hearth-cli` itself. On a
hosted runner the registry starts empty and must be allowed to download, so the
variable is deliberately absent there (see `.github/workflows/bench.yml`).

Gate against the recorded document:

```powershell
cargo run --release -q -p hearth-bench --offline -- -strict
```

Both commands pass `--release` because four of the six metrics are measured in
this process; a debug build is 4–15× slower and would report a regression on
every row against a release baseline. The `Makefile` targets pass it too.

The document is keyed by `os`/`arch`. A run on a platform with no record is
skipped rather than compared, so the file can grow one platform at a time.
