# CI：跑什么、抓到过什么、还有什么没演练

仓库是 `github.com/haijiawe/hearth`（2026-11-21 起公开），代码树是 MIT。这里的每个工作流现在都在托管 runner 上执行过，包括 `release.yml`（通过它的 `workflow_dispatch` 干跑模式）和手动的 `bench.yml`。

诚实的头条：**早期的 CI 运行不是走形式。** 头二十四次里有十一次抓到了 Windows 开发机看不见的缺陷，其中好几次是测试本身的缺陷——那种在单台机器上会无限期隐藏下去的缺陷。

## 作业

| 作业 | Runner | 证明什么 |
| --- | --- | --- |
| `changes` | ubuntu-latest | 对 diff 分类，好让下面的作业只在改动可能触达它们时才开（见*每个作业什么时候跑*） |
| `test` | ubuntu-latest、windows-latest | 在最低支持的 rustc（1.89，由 `rusqlite` 定）上构建并跑全部测试 |
| `sandbox-macos` | macos-latest | 在真实内核上跑 `crates/sandbox/tests/macos.rs`：模式矩阵、写范围、`read-only`、`extra_write`、网络两个方向 |
| `lint` | ubuntu-latest | 在当前 stable 上跑 `cargo fmt --check` 和 `cargo clippy --workspace --all-targets -- -D warnings` |
| `web` | ubuntu-latest | `npm ci`、`tsc --noEmit`、`vitest run`、`vite build`，以及 2,000,000 字节的 bundle 上限 |

刻意用两套工具链：`test` 证明下限，`lint` 在开发者干活的地方跑。钉死工具链的 lint 作业看不见只有更新的 clippy 才认识的 lint（`is_multiple_of` 正是那种 lint）。

## 每个作业什么时候跑，以及代价

私有仓库的托管 runner 从一个月度池子里计费——免费版和教育版是 2,000 分钟——而且不是同一个费率：Linux 算 1×，Windows 2×，macOS 10×。这个仓库十一月的用量是 $12.00 的算力：942 分钟 Linux、1,438 分钟 Windows、64 分钟 macOS，也就是大约 4,458 *计费*分钟，对着 2,000 分钟的池子。正是这超出的部分，让每次运行的每个作业在运行创建四秒后、且 `steps=0` 时就报出
`The job was not started because recent account payments have failed or your spending limit needs to be increased`。

于是有了两处改动，而两处都不削弱门禁：

| 改动 | 效果 |
| --- | --- |
| 对 `**/*.md`、`docs/**`、`LICENSE` 加 `paths-ignore` | 纯文档提交什么都不跑。它能弄坏的源码文件一个都没变。 |
| 一个 `changes` 作业给 diff 分类，其余作业据此开启 | `test` 和 `lint` 需要 Rust 改动；`web` 需要 `web/` 改动；`sandbox-macos` 只需要 `crates/{sandbox,exec}/**` 或 `Cargo.*` |

`sandbox-macos` 是这一切存在的原因。按 Linux 十倍的费率，它比另外四个作业加起来还贵，而它只可能在它测的那两个 crate 内部失败。`release.yml` 没动：打标签照样构建一切。

仓库现在是公开的，成本问题因此彻底消失：标准 GitHub 托管 runner 对公开仓库免费，所以那 2,000 分钟的池子不再限制 CI 能做什么。上面两处削减照样留着。一次文档提交去买五个操作系统，是无意义地烧算力，不管是不是别人付钱；而 `sandbox-macos` 对它测的那两个 crate 之外的改动，本来也没什么可说的。计费的历史留着，因为它解释了门禁为什么长成现在这样。

失败的方向是刻意的。`changes` 作业跑在 `set -eu` 下，所以那里出错会让整次运行失败，而不是悄悄跳过门禁；当它找不到基线提交时——新分支、强推、手动触发——它会退回 `git ls-files`，于是每个作业都开。一个被跳过的作业，只有在 diff 证明它对这个改动无话可说时才可接受。

## 每一次运行，以及各自抓到了什么

| 运行 | Head | lint | web | ubuntu | windows | 抓到 |
| --- | --- | --- | --- | --- | --- | --- |
| 44528486442 | `4947037` | ✗ | ✓ | ✗ | ✗ | 1、2、7 |
| 44538730334 | `2b36d70` | ✗ | ✓ | ✗ | ✗ | 2、3 |
| 44545921397 | `ddb2a3d` | ✓ | ✓ | ✗ | ✗ | 4、5 |
| 44556272262 | `5732bce` | ✓ | ✓ | ✗ | ✗ | 6 |
| 44558234256 | `7e6e25f` | ✓ | ✓ | ✓ | ✓ | — |
| 44564786342 | `46e9188` | ✓ | ✓ | ✓ | ✗ | 8 |
| 44582690666 | `0065fff` | ✓ | ✓ | ✓ | ✗ | 9 |
| 44585759279 | `d05c4c5` | ✓ | ✓ | ✓ | ✓ | — |
| 44596952813 | `fe762e9` | ✓ | ✓ | ✓ | ✓ | — |
| 44603497179 | `58e6046` | ✓ | ✓ | ✗ | ✓ | 10 |
| 44606471925 | `7aaf73f` | ✓ | ✓ | ✓ | ✓ | — |
| 44618571269 | `f56bda3` | ✓ | ✓ | ✓ | ✓ | —（`sandbox macos-latest` 的第一次运行，绿） |
| 44627249157 | `ce9d56f` | ✓ | ✓ | ✓ | ✓ | —（darwin/arm64 的 bench 记录及其文档） |
| 44638519696 | `3a0acb16` | ✓ | ✓ | ✓ | ✓ | —（能力快照路由、它的测试和文档更正） |
| 44644899266 | `f3276f63` | ✓ | ✓ | ✓ | ✓ | —（事件日志之上可寻址的压缩） |
| 44648922153 | `d5861b3` | ✗ | ✓ | ✗ | ✓ | 11 |
| 44649948765 | `dbd3201` | ✓ | ✓ | ✓ | ✓ | —（缺陷 11 已修） |
| 44651257668 | `6754c9b` | ✓ | ✓ | ✓ | ✓ | —（未完成的分工 worker 报 `null`，不是 `false`） |
| 44656201366 | `50ecca0` | ✓ | ✓ | ✓ | ✓ | —（goal 路由、团队看板、能到达模型的附件） |
| 44657019817 | `64c401c` | ✓ | ✓ | ✓ | ✓ | —（记录那次运行） |
| 44660397468 | `e7a4f96` | ✓ | ✓ | ✓ | ✓ | —（goal 药丸，以及 `nextRound` 作为循环会开启的那一轮） |
| 44661935829 | `72da233` | ✓ | ✓ | ✓ | ✓ | —（浏览器读交接清单） |
| 44663090396 | `790c157` | ✓ | ✓ | ✓ | ✓ | —（数一数二进制实际打印出的命令） |
| 44676607395 | `1924819` | ✓ | ✓ | ✓ | ✓ | —（DSH rc.5 源码对比；一个重复触发被并发取消了） |
| 44767162264 | `4a18fe5` | ✓ | ✓ | ✓ | ✓ | —（`sandbox macos-latest` ✓：分类门禁的第一次运行，走手动触发，所以 `changes` 按设计退回"全都跑"） |
| 44769558010 | `295035b` | ✓ | skip | ✓ | ✓ | —（第一次分类的*推送*：只有 `.github/**` 和 `docs/**`，所以 `sandbox-macos` 和 `web` 被跳过，三个 Rust 作业跑了） |

那十一个缺陷，按出现的顺序：

1. **钉死的 target 目录不可移植。** `.cargo/config.toml` 里带着
   `target-dir = "D:/hearth-target/hearth"`——那是源码路径含非 ASCII 字符时 MSVC 报
   `D8050` 的绕法。在 Linux 上那是一段含冒号的路径，于是 ubuntu 作业死在
   `failed to join paths from $LD_LIBRARY_PATH together`。现在配置里完全不钉 target 目录；`check.ps1` 只在 checkout 路径需要时才推一个 ASCII 的出来，而工作流自己设 `CARGO_TARGET_DIR`。
2. **两个比开发工具链更新的 clippy lint**：`crates/session/src/fork.rs` 里的 `needless_late_init`，以及 `crates/server/src/server.rs` 里的 `chunks_exact_to_as_chunks`。两处都改写成 lint 偏好的形式。
3. **一个只在 `#[cfg(windows)]` 下用到的变量**（`crates/cli/src/doctor.rs`）让非 Windows 构建产生警告（`unused variable: stderr`），而 `-D warnings` 把它变成失败。
4. **清单路径在两个平台上不是一个意思。**
   `C:\evil.js` 在 Windows 上是绝对路径，在 Unix 上只是个普通文件名，于是包含性检查在那边接受了它。一份 bundle 会因此指向两个不同的文件，取决于它装在哪。`entry_stays_inside` 现在在每个平台上都拒绝反斜杠和盘符前缀；`/` 是清单唯一可用的分隔符。
5. **`TEMP` 可能是 8.3 短名。** 托管 Windows runner 给的是
   `C:\Users\RUNNER~1\...`。Rust 通过它建文件毫无问题；把同一个字符串交给 Node 子进程，却报 `Cannot find module`。测试的临时根目录现在被规范化一次（并去掉 `\\?\` 前缀），好让两边看到同一条长路径。
6. **一个测试清理了它的父目录。** 桥接测试用 `root.parent()`——也就是系统临时目录本身——当 `data_dir`，然后在结尾把它删掉。Windows 拒绝删除打开着的文件，所以本地什么坏事都没显出来；Linux runner 拥有 `/tmp`，真的把它删了，顺手带走了其他每个测试的临时目录。现在每个测试拥有 `<base>/plugins` 或 `<base>/experts`，只删自己的 base。
7. **插件宿主报了错误的最后一行 stderr。** 失败的 Node 子进程输出最后一行是 `Node.js v22.23.4`，所以宿主那句一行摘要变成了一个版本号。它现在优先选最后一行以 `Error` 开头或含 `Error: ` 的行。正是这个改动让缺陷 5 变得可读。
8. **一个在读请求之前就先应答的 fixture。** `crates/net` 的测试服务器读了请求头就回话，没有把 POST 正文读干净。在 Windows 上，关闭一个还握有未读输入的 socket 会发 RST，而 RST 可能丢掉落客户端还没读的响应；ubuntu 作业过了，windows 以 `os error 10054` 失败。
9. **一个把握手也包进去的期限。** 插件超时测试在 `load` 之前就设了 200 ms，所以启动 Node 也得挤在里面。在托管 Windows runner 上挤不进去。现在这个期限只覆盖那个本该卡住的调用，也就是测试名字所指的那个调用。
10. **一个断言比较的拼写和代码打印的不一样。** 新的 Landlock 测试第一次真正在 Ubuntu 上跑，五个里过了四个；第五个以 `got "refused: permission denied"` 失败——内核*确实*拒绝了连接，但断言找的是 Rust 的 Debug 拼写 `PermissionDenied`，而辅助函数打印的是 Display 形式。现在比较对文本大小写不敏感。它值得留在名单里，因为产品是对的而测试是错的：CI 红是一次要调查的主张，不是一个判决。
11. **一个只有一个平台需要的 `mut`。** `crates/cli/src/office.rs` 把解释器候选列表写成 `let mut candidates = vec![...]`，并在 `#[cfg(windows)]` 后面 push Windows 的启动器 `py -3`。在其他每个平台上都没有东西改这个绑定，于是 `-D warnings` 把 `unused_mut` 变成构建失败——`lint` 和 `ubuntu` 两个作业都死了，而 `windows` 和 `macos` 过了、本地门禁是绿的。现在每个平台都整体构造自己的列表，所以 `mut` 恰好只出现在有 push 的地方。需要留意的通用形状是：**一个 `#[cfg]` 守卫的修改，在不进入该分支的每个平台上都是 `unused_mut`。**

缺陷 4–9 是同一类：**第二台机器就是第二个读者。** 一个在 Windows 上因为删除悄悄失败、因为反斜杠有特殊含义、或者因为磁盘够快而通过的测试，不是它名字所指那个东西的测试。缺陷 11 属于第二类：**一个 `#[cfg]` 是在主张两份构建有差别，而编译器两份都查。**

## bench 工作流

`bench.yml` 只有 `workflow_dispatch`，因为 runner 硬件是共享的，而基线只在一个平台内部可比。它给每个 runner（ubuntu-latest、windows-latest、macos-latest）各记录一份报告，命令是

```
cargo run --release -q -p hearth-bench -- -out "baseline-<os>.json" -baseline ""
```

并把每份上传成产物。要在新平台上武装门禁，就下载产物，把它那条唯一记录抄进 `scripts/bench/baseline.json`（这个文件是一个 `{version, baselines: [...]}` 集合，`hearth-bench` 只比较匹配 `os`/`arch` 的那条记录），连测量日期一起提交。工作流的最后一步带 `continue-on-error: true` 跑 `-strict`：一次还无法比较的运行不是失败，它只报告。

这个工作流头两次运行各抓到一个缺陷，而且两个在有热 target 目录和可用 registry 的开发机上都看不见：

1. **内层构建被钉成离线。** `hearth-bench` 会调 `cargo build -p hearth-cli` 拿一个可启动的二进制，而那次调用总是带 `--offline`。开发机什么 crate 都缓存着；托管 runner 从空 registry 起步，于是死在 `aho-corasick` 上。现在这个开关取决于 `HEARTH_BENCH_OFFLINE=1`，所以网络策略归调用方管（Makefile 设它，工作流不设）。
2. **构建被算进了测量预算。** `version_startup` 在冷 runner 上要付那次构建的钱；它花了大约六十秒，把测试台六十秒的运行预算耗尽，于是第二次运行六项指标只记了一项。`run_all` 现在在开始计时之前先构建一次，产物里六项齐全。

`record macos-14` 最初是 `cancelled`，没有步骤也没有 runner 名字：那个标签无法分配给这个账号，而 `release.yml` 的 `macos-latest` 作业在 2026-11-20 确实跑过。所以 bench 矩阵现在要 `macos-latest`；能不能产出一份 `darwin/arm64` 记录，由下一次触发决定，而不是假定这两个标签可以互换。

## 一次被做成等待的抖动

`tests/e2e/tests/acceptance.rs` 里的
`http_surface_matches_oracle_protocol_and_event_contract` 失败过两次，之后连续六次以上完整运行都通过，且没有捕获到 `left`/`right` 的值。这个断言比较 `page["lastSeq"]` 与 `page["events"].len()`。两者都出自
`handle_events`（`crates/server/src/server.rs`），它在同一个请求内先调 `fold_session`（读整个日志）再调 `read_raw_events`（又读一遍）——所以落在两次读之间的写入会让它们短暂不一致，而契约对不一致的答案是用 `nextAfter` 再翻一页。

测试现在重新轮询直到两者一致，用它本来就在用的同一个 20 秒期限兜住，一直不一致就以打印两个值的方式失败。那是一个更弱的瞬时断言，和一个更强的关于契约的陈述：`turn/end` 之后日志是静止的，所以一致是永久的，而不一致仍然算失败。

## 演练过的，以及它仍然不能证明什么

- **`release.yml` 现在已经真的跑过。** 一次 `workflow_dispatch` 干跑
  （[44588872748](https://github.com/haijiawe/hearth/actions/runs/44588872748)）
  构建、校验并上传了全部三个原生产物——
  `hearth-windows-amd64.exe` 4,183,350 B、`hearth-linux-amd64` 4,584,901 B、
  `hearth-darwin-arm64` 3,985,780 B——并生成了过 `sha256sum --check` 的 `SHA256SUMS`。`Publish GitHub release` 那一步被跳过，因为 ref 不是标签。这证明了构建路径；它不证明 `gh release create` 本身，那需要真实的 `v*` 标签，而且撤不回来。
- **沙箱在三个家族上都被测到。** `test` 在 Windows 上跑共享套件、在真实 Ubuntu 内核上跑 `crates/sandbox/tests/linux.rs`；`sandbox-macos` 在 Apple Silicon 上跑 `crates/sandbox/tests/macos.rs`。仍然没覆盖的是 Ubuntu 之外的内核，或者比 `macos-latest` 镜像更老的 macOS。
- **三份 bench 基线都在**，2026-11-19 记录：linux/amd64 在 17:58:45Z，windows/amd64 在 17:36:04Z，macos/arm64 在 21:44:26Z。没有记录的平台是跳过而不是失败，所以缺第四条是覆盖缺口，不是门禁坏了。