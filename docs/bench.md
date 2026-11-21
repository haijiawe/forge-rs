# hearth-bench —— 性能测试台与基线门禁

已退役 Go 树里 `scripts/bench` 的 Rust 移植。那棵树在 2026-11-17 删掉了，但它的决策留了下来：指标名、单位、容差、文档形状和门禁策略一律照旧，只把 `goVersion` 改名为 `rustVersion`。

## 为什么要有第二个测试台

`cargo bench` 量的是一个 crate 里的函数。这个测试台量的是用户看得见的产品：`hearth` 可执行文件、磁盘上的会话日志、JSON 编解码器。它只写一份语言无关的文档，所以一次 Rust 运行和一次 Go 运行能在同一平台上比较，而不必共用一套 schema。

## 命令行开关

| 开关 | 默认值 | 含义 |
| --- | --- | --- |
| `-out <path>` | 空 | 把这次运行合并进 `<path>` 处的文档（仓库相对路径） |
| `-baseline <path>` | `scripts/bench/baseline.json` | 与这份文档比较；`""` 表示只记录不比较 |
| `-fail-over <pct>` | `10` | 某一行没有 `tolerance` 时的兜底容差 |
| `-strict` | 关 | 指标超出容差回退时退出码 1 |
| `-require-baseline` | 关 | 本平台没有记录时退出码 1 |
| `-h`、`-help` | — | 打印用法并退出 0 |

`-flag value`、`-flag=value`、`--flag value` 和 `--flag=value` 都能用，和 Go 的 `flag` 包一样。解析在第一个位置参数处停止。

## 指标

| 名称 | 单位 | 越大越好还是越小越好 | 负载 |
| --- | --- | --- | --- |
| `version_startup` | ms | 越小越好 | 20 次冷启动 `hearth --version`，每个阶段一次 |
| `session_append_fsync` | events/sec | 越大越好 | 200 次追加，每条事件一次 fsync，20 个阶段 |
| `session_append_batch` | events/sec | 越大越好 | 2,000 次追加，`sync_every=100`，20 个阶段 |
| `session_replay_10k` | events/sec | 越大越好 | 一份 10,000 × 3,800 字节的日志；重新打开并读回，20 个阶段 |
| `session_projection_10k` | events/sec | 越大越好 | 同一份日志用 `Projection::fold` 折叠，20 个阶段 |
| `event_roundtrip` | ns/op | 越小越好 | 5 个阶段 × 1,000 次对一条 4KB 事件的规范编组加解析 |

每个阶段产出一个样本。`value` 取最快的那个阶段（延迟类取最小值，吞吐类取最大值）；`p50`/`p95` 是最近秩百分位。低于中位数一半的样本在汇总前被丢弃，但只在还剩至少两个样本时才丢。数值保留四位小数。时钟没有前进的阶段最多重试六次；失败的阶段是错误，不是零。整轮运行有 60 秒预算，超时就跳过后续指标并写明一行，而不是把半个数字当成完整结果发出去。

容差**按行**存储：`session_append_fsync` 和 `session_append_batch` 是 `0.35`（35%），其余四个是 `0.10`（10%）。只有在基线行写着 `tolerance == 0` 时才用 `-fail-over`。

下面这段数字是字段形状的占位符，不是实测记录。真实的记录在 `scripts/bench/baseline.json` 里：windows/amd64 来自本机，linux/amd64 来自托管 runner，darwin/arm64 来自 macOS runner。只从稳定的构建里记录新行。

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

规则，全部在 `tools/bench/src/baseline.rs` 里有单元测试：

- version 必须是 1；集合必须非空；每条记录都要有 `os`、`arch` 和至少一个结果。顶层的旧的单份报告会被接受，并包成一个单记录集合。
- 选择同时匹配 `os` **和** `arch`；最新的 RFC 3339 时间戳获胜，比较时考虑时区偏移。可解析的时间戳胜过不可解析的；并列时保留第一条记录。
- upsert 就地替换第一条匹配的记录，丢弃后面的重复项，平台不存在时追加一条新记录。写入器拒绝覆盖一份无法解析的文件。
- 编组用两空格缩进、只用 LF、字段顺序稳定、恰好一个结尾换行。

Rust 的架构名在文档里映射成 Go 的名字（`x86_64` → `amd64`、`aarch64` → `arm64`、`x86` → `386`），这样两个实现共用一套键空间。

## 门禁策略

1. `-baseline ""` → 只打印记录说明，不比较，退出 0。
2. `-baseline` 处没有文档 → 打印 `No baseline file at …`，没有可比较的东西；即使带 `-strict` 也退出 0，只有带 `-require-baseline` 才退出 1。
3. 文档存在但本平台没有记录 → 打印一行诚实的跳过说明，外加它确实记录了的那些平台；退出规则同 (2)。`-strict` 会追加一句说明：什么都没被门禁约束，以及为什么仍然退出 0。
4. 找到记录 → 按 `name` 比较行。`"events/sec"` 和 `"MB/s"` 是越大越好，其余单位都是越小越好。`pct = (current - baseline) / baseline * 100`，越大越好的指标反过来算；变差超过基线行容差就是 `REGRESSED`。基线值为零是 `SKIP`，永远不会是 `REGRESSED`。新增的行和被删掉的行会被报告，但永远不会导致失败。
5. 报告模式下回退也退出 0。只有 `-strict` 退出 1，并给出计数。
6. `rustVersion` 不同会被记录下来；门禁照常适用。

## Windows 记录，以及为什么要重测

`scripts/bench/baseline.json` 原本只有一条记录，2026-11-16 对着 M7 那棵树测的（7.5 MB 二进制、十个 crate）。现在这棵树是 8.5 MB，按设计多了六个 crate。记录在机器稳定下来之后于 2026-11-19 重测；这些差值写在这里，而不是悄悄吸收进容差里：

| 指标 | 2026-11-16 | 2026-11-19 | 差值 |
| --- | --- | --- | --- |
| version_startup | 5.14 ms | 6.37 ms | 慢了 24% |
| session_append_fsync | 4,501 /s | 4,354 /s | −3% |
| session_append_batch | 76,942 /s | 89,477 /s | 快了 16% |
| session_replay_10k | 159,766 /s | 148,230 /s | −7% |
| session_projection_10k | 92,313 /s | 84,630 /s | −8% |
| event_roundtrip | 2,843 ns/op | 2,849 ns/op | +0.2% |

`event_roundtrip` 相差在 0.2% 以内，说明机器和 release profile 就是当初产出旧记录的那一套；这并不能解释启动那一行的变化。新增的六个 crate 是最明显的候选，但这一点没有被隔离验证，而诚实的说法就是：没有验证。6.4 ms 的启动仍然低于已退役 Go 树的 7.2 ms，也远低于任何 JS harness。记录的作用是抓**今天**这棵树的回退；留一份过期记录，只会得到一个谁都不打算去修的红门禁。

## Linux 记录，以及为什么托管机器的数字只作参考

`linux/amd64` 于 2026-11-19 由 `bench.yml` 在 `ubuntu-latest` 上记录（rustc 1.100.0）。同一个 runner 标签上的同一个作业，隔一小时跑两次，I/O 指标差了不止两倍：

| 指标 | 运行 A | 运行 B |
| --- | --- | --- |
| session_append_batch | 77,828 /s | 37,756 /s |
| session_replay_10k | 77,310 /s | 135,271 /s |

提交进去的记录是运行 B，用单次构建的测试台测的。共享 runner 告诉你的是平台的形状，不是节拍器；正因为如此，工作流里的 `-strict` 那一步是 `continue-on-error`，而真正应该给 Linux 工作设门禁的，是你自己控制的机器上测出的 Linux 记录。

同一工作流产出的 `windows-latest` 产物没有提交：它的 `session_append_fsync` 是 368/s，而产出 Windows 记录的那台机器上是 4,354/s，后来一次手动触发测到 274/s。托管 Windows 的磁盘就是慢这么多，把两台机器塞进同一个 `windows/amd64` 键下，门禁就毫无意义了。

第三次触发（2026-11-20，run 44621655899）又产出了一份 Linux 记录——`session_append_batch` 35,322/s、`session_replay_10k` 125,059/s。它没有被采纳：每次触发都拿一份托管记录换掉另一份托管记录，只会把这个文件变成 runner 噪声的变更日志。运行 B 留着，因为上表描述的就是它。

## macOS 记录，以及它为什么存在

`macos/arm64` 于 2026-11-20 由 `bench.yml` 在 `macos-latest` 上记录（rustc 1.100.0，Apple Silicon）：

| 指标 | 数值 |
| --- | --- |
| version_startup | 4.79 ms |
| session_append_fsync | 1,715 /s |
| session_append_batch | 42,315 /s |
| session_replay_10k | 150,170 /s |
| session_projection_10k | 123,629 /s |
| event_roundtrip | 3,775 ns/op |

这是一个托管数字，上面每一条告诫它都继承——但它是**唯一**一份 macOS 记录，所以选择只有两个：要么拿一份摇摆的参考，要么在那个平台上完全没有门禁。它是因为第二个理由才被提交的，而 I/O 那几行的容差（35%）宽到足以吸收 Linux 那几次运行展示出来的摆动。

拿到它还要求把矩阵从 `macos-14` 改成 `macos-latest`：第一次触发要的是 `macos-14`，作业被取消且没有分配 runner（`runner_id: 0`、步骤为空），而前一天 `release.yml` 的 `macos-latest` 作业跑得好好的。这两个标签不能互换。

## 运行与集成

```powershell
$env:CARGO_TARGET_DIR = "D:\hearth-target\hearth"
cargo fmt -p hearth-bench -- --check
cargo clippy -p hearth-bench --all-targets --offline -- -D warnings
cargo test -p hearth-bench --offline
cargo run -q -p archcheck --offline

# 报告 / 门禁 / 有意记录（永远不要从吵闹的构建里记录）
cargo run --release -q -p hearth-bench --offline --
cargo run --release -q -p hearth-bench --offline -- -strict
cargo run --release -q -p hearth-bench --offline -- -out scripts/bench/baseline.json -baseline ""
```

`--release` 不是优化建议，它是计量单位的一部分。六个指标里有四个是在本进程内测的；debug 构建量的是 debug profile，比每条记录都低 4–15×（同一台机器上 `event_roundtrip` 在 debug 里是 91,792 ns/op，在 release 里是 2,840 ns/op）。`Makefile` 的 `bench-*` 目标与 `.github/workflows/bench.yml` 都会带上它；一个照着不加 `--release` 的命令去跑的运维，会看到每个指标都回退，而没有一个是真实的。

集成已经就位：`tools/bench` 是 workspace 成员，`Makefile` 有 `bench-run` / `bench-record` / `bench-check`，`bench.yml` 给每个 runner 记录一份产物。工作流是手动的（`workflow_dispatch`），所以记录永远不会搭上普通 push 的顺风车。

## 边界

这个 crate 只许 import `hearth-core` 和 `hearth-session`。别的都是 std 加上 workspace 已有的 `serde`/`serde_json`（基线文档和 4KB 载荷）。没有 CLI 框架，没有异步运行时，没有新的 workspace 依赖。这个工具从不编辑 `crates/**`、根 `Cargo.toml`、`Makefile`、`architecture.yaml` 或 `docs/08-rust-port.md`。