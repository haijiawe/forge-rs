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
cargo run -q -p forge-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

Gate against the recorded document:

```powershell
cargo run -q -p forge-bench --offline -- -strict
```

The document is keyed by `os`/`arch`. A run on a platform with no record is
skipped rather than compared, so the file can grow one platform at a time.
