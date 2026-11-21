# scripts/bench

这个目录放的是**提交进仓库的**性能基线文档 `baseline.json`——只在有人刻意记录过
之后才有这个文件。测量工具本身在 `tools/bench`；参数、指标与门禁策略见
`docs/bench.md`。

`-baseline` 的默认路径就指向这里。文件不存在是一次诚实的跳过，不是错误：报告会打印
`No baseline file at …` 并以 0 退出，**即使在 `-strict` 下**。

只在构建稳定之后记录：

```powershell
$env:HEARTH_BENCH_OFFLINE = "1"   # 工具会自己 shell 出去跑 `cargo build -p hearth-cli`
cargo run --release -q -p hearth-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

`HEARTH_BENCH_OFFLINE` 之所以存在，是因为工具要自己构建 `hearth-cli`。在托管 runner
上 registry 是空的、必须允许下载，所以那个变量是刻意不设的（见
`.github/workflows/bench.yml`）。

拿记录下来的文档当门禁：

```powershell
cargo run --release -q -p hearth-bench --offline -- -strict
```

两条命令都带 `--release`，因为六项指标里有四项是在本进程内测的；debug 构建慢 4–15
倍，对着 release 基线会让每一行都报成回归。`Makefile` 里的目标也带它。

文档按 `os`/`arch` 索引。在没有记录的平台上跑会被跳过而不是比较，所以这个文件可以
一次长一个平台。