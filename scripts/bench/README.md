# scripts/bench

This directory holds the committed performance baseline document,
`baseline.json`, when one is deliberately recorded. The harness itself lives
in `tools/bench`; see `docs/bench.md` for flags, metrics and gate policy.

The default `-baseline` path points here. A missing file is an honest skip,
not an error: the report prints `No baseline file at …` and exits 0, even
under `-strict`.

Record only from a settled build:

```powershell
$env:CARGO_TARGET_DIR = "D:\forge-rs-target\forge-rs"
$env:FORGE_BENCH_OFFLINE = "1"   # the harness shells out to `cargo build forge-cli`
cargo run -q -p forge-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

`FORGE_BENCH_OFFLINE` exists because the harness builds `forge-cli` itself. On a
hosted runner the registry starts empty and must be allowed to download, so the
variable is deliberately absent there (see `.github/workflows/bench.yml`).

Gate against the recorded document:

```powershell
cargo run -q -p forge-bench --offline -- -strict
```

The document is keyed by `os`/`arch`. A run on a platform with no record is
skipped rather than compared, so the file can grow one platform at a time.
