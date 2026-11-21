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

退役 Go 树的 `scripts/bench` 的 Rust 版本。六项指标、一份基线文档、一条规矩：
**只有 `-strict` 会让回归失败**。

## 做什么

- 测六项：`version_startup`（毫秒，越低越好）、`session_append_fsync`、
  `session_append_batch`、`session_replay_10k`、`session_projection_10k`
  （事件/秒，越高越好）、`event_roundtrip`（纳秒/次，越低越好）。
- 每项跑 20 个阶段（编解码那项 5 个）。公开 p50、p95 以及**最快**那一阶段的值；
  容差就写在每个值旁边：两个 append 指标（fsync/batch）是 0.35，其余是 0.10。
- 把一次运行并入一份语言中立的基线文档，按 `os`/`arch` 索引，并且**只**比较
  当前平台那一条记录。

```
cargo run --release -q -p hearth-bench --offline --                          # 只报告
cargo run --release -q -p hearth-bench --offline -- -strict                  # CI 门禁
$env:HEARTH_BENCH_OFFLINE = "1"   # 只在离线的开发机上需要
cargo run --release -q -p hearth-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

`--release` 是测量本身的一部分，不是「快一点」的提示：六项里有四项跑在这个进程内，
debug 构建会比 release 基线低 4–15 倍（同一台机器上 `event_roundtrip` 是
91,792 对 2,840 ns/op）。

这个工具会自己 `cargo build` 出 `hearth-cli`；`HEARTH_BENCH_OFFLINE=1` 让那次内层构建
带上 `--offline`。托管 runner 上不要设它——空的 registry 配上 `--offline` 正是第一次
bench 工作流失败的方式。

## 公开面

- 二进制 `hearth-bench`；参数 `-out`、`-baseline`、`-fail-over`、`-strict`、
  `-require-baseline`（默认值依次是：空、`scripts/bench/baseline.json`、10、
  关、关）。
- 基线形状：`{"version":1,"baselines":[{os,arch,rustVersion,timestamp,
  results:[{name,unit,value,samples,p50,p95,tolerance}]}]}`。顶层放一份旧式单次
  报告也接受，会被包一层。
- 库模块 `args`、`baseline`、`gate`、`metrics`、`platform`、`stats`、`time` 为
  单测而公开；操作者指南是 `docs/bench.md`。

## 依赖与边界

- 所有会话相关工作都经 `hearth-core`（`Event`、`EventType`、规范 JSON）与
  `hearth-session`（`Store`、`Options`、`Projection`）。没有新增第三方依赖：
  `serde`/`serde_json` 是工作区既有条目，只用于基线文档和那个 4 KB 报文。
- `Projection::fold` 取代了 Go 的 `session.Project(path)`：日志被显式读取，
  而且这次读取算在计时区间内。
- `hearth` 二进制的位置从 `cargo build --message-format=json` 里取，所以自定义
  `CARGO_TARGET_DIR` 也能用。工具会向上找到同时有 `Cargo.toml` 与
  `architecture.yaml` 的目录当作仓库根。
- 基线文档或平台记录缺失时，打印一句诚实的跳过并退出 0，**即使在 `-strict` 下**；
  `-require-baseline` 会让它退出 1。删掉的行和新增的行永远不判失败。报告模式下的
  回归退出 0。

## 不要做

- 不要在吵闹的、还没稳定的构建上记录基线；`-baseline ""` 是显式的记录模式，
  必须是刻意的动作。
- 不要跨平台、跨架构、跨指标名比较。
- 不要把跳过写成 `OK`，也不要把报告模式下的回归变成退出 1。
- 不要加 benchmark 的 CLI 框架、异步运行时或新的工作区依赖；不要从这个 crate 里
  改 `Makefile`、根 `Cargo.toml`、`architecture.yaml`、`docs/08-rust-port.md`
  或 `crates/**`。