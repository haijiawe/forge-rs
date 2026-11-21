# ADR-0002 —— harness 用 Rust 写

- **状态：** 2026-11-17 接受并交付；六个波次全部合入，§6 记录每一波必须过什么。本 ADR 取代 ADR-0001（Go + React）中的语言那一半——那份文档在已退役的树里叫 `01-tech-stack-adr.md`，**没有搬过来**——当年导向 Go 的那些测量结果在 §1 里被引用，而不是链接过来，所以这次翻转仍然留有纸面痕迹。
- **决策人：** 用户。本文记录的是后果，不是辩论过程。
- **范围：** `D:\hearth` 处的一棵新树。这次移植的起点那棵 Go 树，**已于 2026-11-17 按用户指示删除**，当时移植已经过了 W6、五个能力 crate 也接进来了。它最后一天以归档形态存在（git 历史完整），充当冻结的参照实现，只为那些会改动 oracle 的 bug 修复而编辑。删掉它**没有改动任何测试**：这里在构建或测试时都不读它，因为 conformance 用例、fixture 和 golden 都是在 W0 期间抄进来的。本文或代码注释里提到 `forge-go/...` 的地方，指的是那棵已退役树里的某个文件，保留下来当出处——构建真正对着被测的是 `docs/contracts/` 里冻结的契约和 `tests/` 下的语料。

## 0. 为什么换语言，一段话说完

当初选 Go 是为了迭代快、依赖面小。Rust 在那些**长期**的轴线上赢——解码吞吐、二进制大小、空闲内存——在每分钟都能感觉到的轴线上输，也就是编译时间。用户权衡之后选了 Rust。让这件事能活下来的原因是：移植里昂贵的部分**不是**代码：契约、事件格式、21 个 conformance 用例、五份 fixture 日志和两个 golden 文件都是语言中立的，而且早就冻结了。一个能复现那些字节的 Rust 读取器不是"大概等价"——它是对着一个 Go 写入器验证过的。

## 1. 这个决策所依赖的实测事实

取自退役树 `06-perf-and-gaps.md` 的 §7（同一台机器，同一个 4,278 字节的事件；那份文档没有搬过来，所以数字抄在这里）：

| 轴 | Go 1.27 | Rust 1.96 | 说明 |
| --- | --- | --- | --- |
| JSON 解码 | 6,718 ns | 1,057 ns | 快 6.4× |
| JSON 编码 | 3,413 ns | 1,473 ns | 快 2.3× |
| Release 二进制 | 13.1 MB | 0.26 MB（hello） | 小 16×，内嵌 UI 之前 |
| 空闲 RSS | 7.9 MB | 4.4 MB | 1.8× |
| 冷启动 | 13 ms | 5 ms | 2.6× |
| 增量构建 | 166 ms | debug 466 ms / release 1,595 ms | **慢 3–10×** |

构建时间的代价是真的，它就是这份账单。§6 是围着它设计的：小 crate、少依赖、只在 release 里开 `lto = "thin"`。

## 2. crate 地图

crate 是 Cargo 在编译期唯一强制的依赖边，所以这张层图用 crate 图谱表达，而不是靠约定。二十四个 crate，每个边界一个——那种审查者希望"被违反了就大声报出来"的边界：

| Crate | 层 | 持有什么 | 不许做什么 |
| --- | --- | --- | --- |
| `hearth-core` | L0 | 事件信封、id、规范 JSON、错误种类 | import 任何其他 hearth crate |
| `hearth-config` | L0 | 配置文件、home 目录、默认值、校验 | 知道会话的存在 |
| `hearth-exec` | L1 | spawn、stdio、超时、进程树清理、沙箱钩子 | 知道工具或会话 |
| `hearth-sandbox` | L1 | 遏制：job object、Landlock、`sandbox-exec` | 知道模型 |
| `hearth-session` | L2 | 只追加日志、投影、压缩、决策、fork | 在自己的日志之外做 I/O |
| `hearth-llm` | L2 | provider 契约、OpenAI 兼容的 SSE 客户端 | 知道会话 |
| `hearth-tool` | L2 | 注册表、执行器、策略、审批、内置工具 | 自己决定策略 |
| `hearth-protocol` | L2 | 服务端与 UI 共用的 wire 类型 | 依赖服务端 |
| `hearth-net` | L1 | 阻塞式 HTTP(S) 客户端、HTML 转文本、重定向 | 知道工具或模型 |
| `hearth-store` | L2 | SQLite 索引、FTS 搜索、重建 | 拥有事件日志 |
| `hearth-import` | L2 | 只读导入旧会话树 | 覆盖一个已存在的会话 |
| `hearth-subagent` | L2 | 跑在自己日志上的嵌套 agent | 无深度限制地跑 |
| `hearth-mcp` | L1 | MCP stdio/HTTP 客户端 | 知道会话或工具 |
| `hearth-skill` | L1 | 技能 frontmatter 扫描 | import 任何 hearth crate |
| `hearth-goal` | L2 | goal 生命周期作为事件、折叠、续跑提示词 | 自己写事件 |
| `hearth-schedule` | L2 | rrule 子集换算成时刻、任务注册表、到期时间 | 跑一个 turn |
| `hearth-market` | L2 | 索引解析、校验和验证安装、感知篡改的卸载 | 在消化 tarball 之前信任它 |
| `hearth-experts` | L1 | 带封闭 YAML 子集的 Markdown 专家/团队名册 | 伸手进技能或会话 |
| `hearth-plugin` | L2 | 走 stdio JSON-RPC 的插件宿主，一套协议，没有第二种传输 | 没有清单就跑插件 |
| `hearth-team` | L2 | 共享任务板上的并行 worker | 让两个 worker 写同一个 scope |
| `hearth-context` | L2 | 事件日志之上的可寻址块：区间、账本、事件 | 写日志 |
| `hearth-server` | L3 | 日志之上的 HTTP/1.1 + WebSocket | 装 agent 逻辑 |
| `hearth-cli` | L3 | `hearth` 二进制 | 持有本该属于某个 crate 的逻辑 |

`tools/archcheck` 读 `architecture.yaml` 和 crate 清单，当某条边指向上层时让构建失败。它和 Go 那个是同一个工具，是重新推导的而不是翻译的：在 Rust 里这个检查很便宜，因为 `Cargo.toml` 本来就说出了真相。

## 3. 依赖预算

Go 构建只有一条直接依赖。Rust 做不到：std 里没有 JSON、没有 TLS、没有 SQL、没有 CSPRNG。所以预算的定义是"每一行都要回答为什么标准库不行"：

| 依赖 | 为什么 | 它替代什么 |
| --- | --- | --- |
| `serde`、`serde_json` | JSON 就是磁盘上和线上的格式 | std 里没有 |
| `getrandom` | 128 位 id 和会话 token 需要 CSPRNG | `crypto/rand` |
| `ring` | 导入 id 需要 SHA-256，而那个摘要 rustls 本来也要 | `crypto/sha256` |
| `rustls`、`rustls-pki-types`、`webpki-roots` | 到模型端点的 HTTPS；`native-tls` 在 Linux 上会拖进 OpenSSL | `crypto/tls` |
| `rusqlite`（bundled） | 会话索引需要带 FTS5 的 SQL | `modernc.org/sqlite` |
| `regex` | `fs_grep` 承诺支持模式 | `regexp` |

刻意**没有**的：

- **没有异步运行时。** 没有 tokio，没有 hyper。服务端是在 `std::net` 上一个连接一个线程，而那正是 Go 构建实际的形态：每个项目一个进程、少量连接、工作以秒计。加异步运行时会多出约 100 个传递依赖和一个调度器，什么也测不出来地省；而且它会把这个项目余下的生命里每个函数都变成 `async`。
- **没有 `clap`。** 参数解析 200 行，而且必须给出现在位置参数之后的开关重排序——这个行为 `clap` 表达起来别扭，而 Go 构建本来也是手写的。
- **没有 `anyhow`/`thiserror`。** 一个带 `kind` 的 `Error` 类型就够了，而一个 proc-macro crate 是每次构建都要交的税。
- **没有 `chrono`。** 时间戳就是 `i64` 毫秒；给人看的 ISO-8601 格式化三十行。

## 4. 工具链约束，在本机实测（2026-11-16）

这些不是偏好。每一条都是被一次失败的构建发现的：

1. **target 目录必须是全 ASCII 路径。** target 放在 `D:\魔改版Agent框架\...` 里时，每个要编 C 的 crate（`rusqlite`、`ring`）都会死在 `cl: 命令行 error D8050 : 无法执行 c1.dll: 未能将命令行放入调试记录中`
   ——`cl.exe` 把命令行写进一条用 ANSI 代码页编码的调试记录，而 `/Fo` 路径没法表示。源码树可以保留它的名字；只有 target 必须挪走。修复在环境里，不在受版本管理的树里：`check.ps1` 在 checkout 路径非 ASCII 时推导出一个 ASCII target 目录，而每个 CI 作业自己设 `CARGO_TARGET_DIR`。早期草稿在 `.cargo/config.toml` 里钉过 `target-dir = "D:/hearth-target/hearth"`；在 Linux 上那是一段含冒号的路径，把第一次托管的 ubuntu 作业搞挂了，所以删掉了。
2. **只用 MSVC。** `stable-x86_64-pc-windows-gnu` 工具链装了但不可用：
   `error calling dlltool 'dlltool.exe': program not found`。不要靠切到 GNU 来"修"构建失败。
3. **MSVC 装在 `D:\Visual Studio\VC\Tools\MSVC\14.51.36231\`**，不在 `PATH` 上。`cc` 通过注册表找到它；什么都不用导出。
4. **crate 来自 rsproxy 镜像**（`D:\Rust\.cargo\config.toml`）。
   `crates.io` 本身在这里不可达（schannel 凭据失败），所以一次需要缓存里没有的 crate 的构建，会以一个看起来像证书问题的网络错误失败。`--offline` 对当前预算够用；缓存里有 1,666 个 crate。
5. **已安装的交叉目标：** `x86_64-unknown-linux-gnu`、
   `aarch64-apple-darwin`。缺：`aarch64-unknown-linux-gnu`、
   `aarch64-pc-windows-msvc`。在这些三元组的链接器出现之前，交叉构建只是编译检查——和 Go 树一样的诚实规则。

## 5. oracle：一次移植是怎么被证明的

Go 树写的它，这棵树在测试里读它。Go 树现在已经没了，所以剩下的就是字节：它们在这里，在 W0 期间逐字抄过来，而它们就是 oracle。

| 产物 | 数量 | 钉住什么 |
| --- | --- | --- |
| `tests/conformance/cases/events/*.json` | 21（9 接受、12 拒绝） | 信封校验、错误措辞、规范字节 |
| `tests/fixtures/events/v1/*.jsonl` | 5 | 真实日志：正常路径、审批、被中止的 turn、未知的未来事件、撕裂的尾部 |
| `tests/golden/projection/basic.json` | 1 | 重放一份已知日志之后折叠出的状态 |
| `tests/golden/agent/text-turn.json` | 1 | 一整个 turn 的事件序列 |
| `tests/conformance/cases/http/*.json` | 34 | HTTP 路由/错误/流行为 |
| `tests/conformance/cases/ws/*.json` | 8 | 快照/事件/增量的分帧、ping、关闭码、升级策略 |
| `docs/contracts/*.md` | 7 | 字段名、状态码、退出码 |

让 oracle 保持诚实的规则：

- **永远不改用例文件去迁就某个读取器。** 如果 Rust 和一个用例不一致，那就是 Rust 错了，或者这个用例对两边都错——而那是要和 Go 树对话的事，不是在这里悄悄改一笔。
- **比字节相等，不比结构相等。** `ok_minimal_event` 断言的是精确的那一行，键序也算，因为它是 Go 写入器产出的。
- **conformance 运行器是共用的，不是复制的。** 它读同一个目录；加一个用例就是同时加给两种语言。

`cargo test -p hearth-core` 已经跑通了全部 21 个用例，并逐字节复现了每一个 `canonical` 数组（W0，2026-11-16）。

## 6. 波次

每一波以一次绿门禁和一个提交收尾；跨波次边界的东西不叫"进行中"。

| 波 | 交付 | 验收 | 状态（2026-11-16） |
| --- | --- | --- | --- |
| **W0** | workspace、`.cargo/config.toml`、`hearth-core`（event、ids、规范 JSON、errors）、conformance 运行器、这份文档 | 21/21 用例；规范字节一致；`cargo test` 绿 | 已交付，`04ebb4f` |
| **W1** | `hearth-session`（存储、恢复、投影）、`hearth-config`、`hearth-exec`、`hearth-sandbox`、`hearth-cli`（version/help/doctor）、`tools/archcheck` | 5 份 fixture 日志重放出 golden 投影；撕裂尾部被修好；`--version` < 30 ms；`doctor` 打印十项检查 | 已交付，`1b5ef75` |
| **W2** | `hearth-llm`（SSE）、带重试/取消/转向的 agent 循环 | fixture 流驱动一整个 turn；事件顺序匹配 `tests/golden/agent/text-turn.json` | 已交付，`1242294` |
| **W3** | `hearth-tool`（注册表、执行器、策略、审批、7 个内置工具、危险清单） | 一次工具调用端到端执行；审批在询问之前就被记录；被拒路径产生 `denied`/`denied_refused` | 随 W2 一起交付，`1242294` |
| **W4** | `hearth-protocol`、`hearth-server`（HTTP + WS）、内嵌 UI、`hearth chat`/`hearth web` | API 契约 §4 满足；WS 帧；React 构建从二进制里提供 | 已交付，`4002c60` |
| **W5** | 索引（`rusqlite`）、import、MCP、skills、subagent | `hearth index --rebuild` / `--search`；`hearth import` 复现旧树；一个假 MCP 服务器的工具出现了 | 已交付，`4002c60` |
| **W6** | bench、release、docs | 可移植的六指标 bench；三个原生发布 runner；本文的每一条主张都被测量 | 已交付，`4002c60` |

### W1 证据（在本机测的，不是从报告里引的）

- `pwsh -File check.ps1`（fmt → clippy `-D warnings` → archcheck → workspace 测试）：**ok**。
- `hearth --version`：**10.5 ms**，二进制 **1.64 MB**（Go：11 ms / 13.1 MB）。
- `hearth doctor`：热态 **4 ms** 完成十项检查。Rust 移植最初为磁盘检查去 spawn PowerShell（664–1379 ms）；现在它从 `windows-sys 0.52` 调
  `GetDiskFreeSpaceExW`——这个依赖已经通过 `ring` 在 lock 文件里了，不是新加的。
- 交叉检查：`x86_64-unknown-linux-gnu` 与 `aarch64-apple-darwin` 退出 0。
- 测试：session 21、exec 25、sandbox 11、cli 12、config 11、core 15、
  archcheck 27。沙箱套件刻意分配 1 GiB 来证明 job object 的上限；它那句
  "memory allocation failed" 是预期的信号，不是失败的测试。
- `tools/archcheck` 用故意违规来证明自己的规则：在 `crates/core/Cargo.toml` 里造一条向上的边，它报出 `edge.upward: hearth-core (L0) depends on
  hearth-exec (L1)` 并以 1 退出。

本机没装 GNU `make`（只有 `nmake`，那只是把配方回显出来），所以 `check.ps1` 用与 Makefile 相同的顺序跑同样的四步。

### W2 的边界，在派活之前就钉死

`crates/llm/src/contract.rs` 归消费方所有，不归写 provider 的那一波。它在三个 agent 并行开工之前就冻结了 `Role`、`Message`、`ToolCall`、`ToolSpec`、
`Request::validate`、`Usage`、`Delta`、`Stream`、`Provider`、`ProviderError` 和
`Aggregator`，好让 agent 循环和 SSE 客户端对着同一批形状编译。`hearth-agent` 同样定义了自己那个窄的 `ToolRunner` trait；W3 的 `hearth-tool` 后来去实现它，而不是两个 crate 互相等。`hearth-agent` 在
`architecture.yaml` 里声明为 L2，这次拆分没有给 workspace 加任何新依赖。

Web UI **没有**移植。它是 TypeScript，讲 HTTP：原封不动搬过来，而那也是最便宜的证明——wire 契约守住了。

### W2/W3 证据（在本机测的）

- `pwsh -File check.ps1`：**ok**，W2/W3 落地后整个 workspace 都过。
- `hearth-llm`：35 个集成测试，包括 🚀 内部发生的 UTF-8 切分、一个 256 KiB 的工具参数片段、精确的 `[0, 1, 0, 1]` 调用索引、秒数与 HTTP-date 两种形式的 Retry-After、key 脱敏，以及 `[DONE]` 门控。`https://` base 用的是 workspace 冻结的 rustls 栈，不是只做 TCP 的桩。
- `hearth-agent`：8 个循环测试 + 9 个摘要器测试。剥掉 `ts`/`id` 之后，golden 的两步 turn 与
  `tests/golden/agent/text-turn.json` 一致。冻结的
  `contract.rs` 没有被 provider 或循环的 agent 编辑过。
- `hearth-tool`：把任务书里那个手写正则换成 workspace 的 `regex`
  crate 之后，31 个测试。那个更笨的匹配器会改变模型从 `fs_grep` 看到的东西——这是移植不能发出去的行为差异。
- `DEFAULT_SUMMARY_BYTES` 最初被 agent 写成 8 KiB；负责人改成了 **2048**，与 Go 的 `session.DefaultSummaryBytes`
  （`forge-go/internal/session/compact.go:513`）和 `m4-surface.md` §4 一致。换一个默认值会静默改变一个被恢复的会话记住什么。
- 刻意的限制：取消是一个协作式的 `Cancel` token，在流增量之间检查，而不是一个 goroutine 去打断阻塞的 `Recv`；私有 CA 的 TLS 端点没有覆盖（信任钉死在 `webpki-roots`）。

### W4–W6 证据（在本机测的）

- `cargo build --release --offline`：退出 0，原生二进制 **4,368,896 B**。Go 的 release 二进制是 13.1 MB；差距主要是 Go 的运行时加两套 HTTP/TLS 栈，不是缺功能。
- `hearth --version` 在 bench 测试台里 **5 ms** 返回；Go 树记录的数字是 7.21 ms。`hearth doctor` 在热态个位数毫秒内打印十项检查；沙箱那项是一个 WARN，精确点名这台主机遏制了什么、没遏制什么。
- 未改动的 React 树构建出的 Web 资源干净（`index-3Y85-UxI.js` 265.72 kB / gzip 81.64 kB），`crates/server/build.rs` 在下一次服务端重建时内嵌 `web/dist`。34 个 HTTP conformance 用例对着真实的 Rust loopback 服务器跑；另有六个服务端测试覆盖 WebSocket 帧、升级前策略、静态缓存头、内嵌 UI、cookie 认证，以及取消/转向。
- `hearth-store` 在反复运行中最好成绩是 **335 ms** 重建 1,000 个会话 / 100,000 个事件（预算 10 s）。`hearth-import` 逐字移植全部六个 fixture 家族，并用 `hearth_core::parse_log` 重新解析每一份输出；37 个测试。`hearth-mcp` 有 43 个测试覆盖 stdio 与 HTTP/SSE，`hearth-skill` 21 个覆盖 fixture 与一次 golden 扫描，`hearth-subagent` 22 个含嵌套委派。
- `hearth chat`/`hearth web` 接进了 `crates/cli`，带上 Go 的开关、退出码、golden 事件顺序、`RecordingApprover` 和每 turn 的适配器。浏览器的答复由 HTTP 层写入，带 `decidedBy:"user"`；被取消/超时的询问由适配器写入，带 `decidedBy:"cancelled"|"timeout"|"error"`，与冻结契约一致。
- `tools/bench` 在一棵稳定的树上把 **windows/amd64、rustc 1.96.0** 记进
  `scripts/bench/baseline.json`：version start 5.14 ms、append fsync 4,501/s、append batch 76,942/s、replay 10k 159,766/s、projection 10k 92,313/s、event roundtrip 2,843 ns/op。之后一次 `-strict` 运行六项指标全过（fsync 摆动了 18%，在它刻意设的 35% 存储容差之内）。debug 测试台的数字被丢弃了：它们量的是测试台，不是 release 构建。
- `.github/workflows/release.yml` 用三个**原生** runner（Windows、
  Ubuntu、macOS arm64），因为这里没有交叉链接器；docs/release.md 写明了哪些产物不产出，以及这个工作流在这台主机上从没跑过。Go 树那六个交叉检查产物，仍然是额外目标仅有的纯编译证据。
- WebSocket golden 语料在两棵树里逐字节镜像，它抓到了 Rust 服务端的三处偏差：合成的 delta `seq`、用 JSON 文本 keepalive 而不是 RFC6455 控制 ping、以及 ping 超时时用关闭码 1011。三处都在 `crates/server/src/server.rs` 里修好了；golden 没有被削弱。Go 的 `TestWS`（8 个子测试）和 Rust 的 `golden_ws_transcripts` 都过。
- 整个 workspace：`cargo fmt --all -- --check`、`cargo clippy --workspace
  --all-targets -- -D warnings`、`archcheck`、以及 `cargo test --workspace
  --offline` 全部退出 0。沙箱套件那句
  `memory allocation of 1610612736 bytes failed` 是 job object 内存上限测试的预期信号，不是失败的测试。

## 7. 哪些是刻意改的

不是每个 Go 习惯都能活下来，假装能只会写出更差的 Rust：

| Go | Rust | 为什么 |
| --- | --- | --- |
| `net/http` 服务器 | 在 `std::net` 线程上手写 HTTP/1.1 | 没有运行时；请求集很小且冻结在 `api.md` 里 |
| `sync.Mutex` + struct | `Arc<Mutex<…>>` 配一个有名字的 owner 类型 | owner 就是那个必须不被意外克隆的东西 |
| 带 `%w` 的 `error` 返回 | 带 `kind` 的 `Result<T, Error>` | 调用方按 kind 分支，人读消息 |
| 每 turn 一个 goroutine | `std::thread` + channel | 每个会话一次一个 turn 本来就是契约 |
| `context.Context` | 显式的 `Cancel` token | 取消是服务端交给 runner 的一个句柄，不是一个环境值 |
| wire 名字用 struct tag | serde 属性 | 同一个想法，同一批名字 |

## 8. W1 里发现的移植陷阱

这些地方，一次小心的 Rust 移植仍然把契约搞错了，留在这里是因为每一种都是会在后续波次里重演的错法。

| 陷阱 | 发生了什么 | 它破了哪条规则 |
| --- | --- | --- |
| 在契约要复制 id 的地方重新铸造 id | `fork` 给每条被复制的记录一个新的 `id`，还配了一句听起来合理的注释（"两份日志不能共用一个 id"）。events-v1 §4.1 只允许每条被复制记录有**两处**信封改动——`session` 和 `seq`——而 §9 把唯一性定为**每日志**的不变量，所以共用一个 id 不是撞车，而是读取器赖以关联的句柄。已在 `crates/session/src/fork.rs` 修好，并由一条对父记录的逐记录 id/ts/turn/step/payload 比较钉住。 | events-v1 §4.1、§9 |
| 把"唯一性"当成全局的 | 同样的混淆会让 `import` 拒绝一份 id 来自其他工具的旧日志，或者让 W5 的索引键在 `(session, id)` 而不是 `(session, seq)` 上。 | events-v1 §9 |
| 一个"看起来已完成"的占位 | `crates/{llm,protocol,server,tool}` 里只有一句点名自己属于哪一波的文档注释。它们的 README 写着 `public: []` 并说明**计划**做什么，因为半成品比空的更难被发现。 | AGENTS.md 法则 3 |
| 一个投影出空的 fixture | `web_compaction_folds_closed_turns_into_one_summary` 的第一版给每条记录都塞了 `{"turn": n}`。摘要器投影的是 `text`，所以历史出来是空的，`summarize_messages` 走了它那条"一条消息（系统提示词）"的分支，**返回了空字符串且没有报错**——测试败在载荷上，不是败在压缩上。塞产品真正会写的东西，否则 fixture 什么都没量。 | m4 表面（`text` 才是被投影的字段） |
| 两个写者，一道守卫 | `hearth_session::compact` 拒绝空摘要（`ERR_EMPTY_SUMMARY`），而 `hearth chat` 通过 `compact_log` 继承了这道守卫。`hearth web` 自己手工拼摘要事件并直接调摘要器，所以它没有守卫：一份记录投影不出任何消息的日志，会把几百条记录换成一个空摘要。现在守卫也在 `crates/cli/src/web.rs` 里，由
`web_compaction_refuses_an_empty_summary` 钉住（前后日志字节完全相同）。一个共用的 helper 本可以让这不可能发生；对同一个库的两次调用不是一条代码路径。 | 会话压缩不变量 |
| 一个把被它守的 bug 编码进去的测试 | 握手测试断言 RFC 样例的 `Sec-WebSocket-Accept` 时**不带**它末尾的 `=`，而 WebSocket 语料在比较前把两边的 `=` 都裁掉了。于是一个丢掉填充的服务端能通过 100% 的套件，却对每个浏览器都失败：浏览器是逐字节比较 accept 的。SHA-1 是 20 字节，所以正确的值永远以单个 `=` 结尾。现在两处断言都要求它。当一个测试比较某种编码时，就比整个编码。 | RFC 6455 §4.2.2、api.md §3 |
| 一个真实客户端做不出来的流程 | `api.md` 描述了一个浏览器：在 WebSocket 升级时发 `X-Hearth-Protocol`，并出示一个"带外设好的" cookie。页面两样都做不到：它不能设置握手头，而产品里也从来没有任何东西设过那个 cookie。所有测试都过了，因为测试自己写裸握手、想设什么头就设什么头。这个问题是靠把发出去的 bundle 放进真实浏览器驱动才发现的；见 `docs/web-ui.md`。一个只有手写客户端才能执行的协议步骤，等于没实现，只是被写进了文档。 | api.md §2、§3 |
| 把抖动 API 怪到优化器头上 | 一次重建之后，release 二进制创建包 profile 时以 `ERROR_ACCESS_DENIED` 失败，而 debug 二进制好好的——而且发生了两次，于是"thin LTO 加一个 codegen unit 会误编译我们的 FFI"看起来已经坐实。隔离矩阵随后显示每种配置都过：那两次失败是**同一个**瞬时的 profile 服务错误，而且只有在新路径上的第一次运行才会触发它。之后十六次新路径运行：十六次成功。破绽在于那个"永久"失败在没有改代码的情况下消失了。在拿 profile 去钉一个疑似 codegen bug 之前，先复现 N 次；codegen 假设形成起来很便宜，发出去很贵。 | |
| 一个会改动被测对象的探针 | `appcontainer_available()` 会创建一个以临时目录命名的包 profile。几个测试进程同时问，一个拿到 `ERROR_PATH_NOT_FOUND`，于是执行器在一台能正常建容器的机器上报出 `filesystem: false`——有十分钟沙箱看起来是坏的，`read-only` 被无缘无故拒绝。探针现在只从固定名字**推导**一个 SID：一个答案，不是副作用。能力检查不能有能力改变答案。 | m6 表面（§Windows） |
| 一个还没工作就先给答案的 API | `DeriveAppContainerSidFromAppContainerName` 对一个**没有** profile 的名字返回 `S_OK` 和一个 SID。把这个 SID 交给 `CreateProcessW` 会以 `ERROR_FILE_NOT_FOUND`（2）失败——一条既不点名 profile 也不点名 SID 的消息——或者以 `ERROR_INVALID_PARAMETER`（87）失败。所以 `package_sid` 先创建 profile，只在 `ERROR_ALREADY_EXISTS` 时才去推导。当两个 Win32 调用描述同一个对象时，那个不创建它的一侧可能什么也没在描述。 | m6 表面、`crates/sandbox/src/appcontainer.rs` |
| 一个以权限错误形式出现的配额 | 攒够 `hearth.sandbox.*` profile 之后（大约九十个），`CreateAppContainerProfile` 开始对**新**名字报 `ERROR_ACCESS_DENIED`，而每个已存在的名字继续可用，几分钟后自己恢复了。消息里没有一个字说"被限流"；它读起来就像权限问题，所以头一个小时花在审 ACL 和 token 上。修法在结构上，不是更大的重试预算：靠目录查找收养已存在的 profile、永不重建它，对新名字重试大约 8 s，然后退到一个共享身份，而不是拒绝执行任何命令。一个每个名字只能**创建**一次的资源，让创建变成一次性成本而不是每次运行的成本——这么对待它，失败模式就缩小到第一次运行。 | m6 表面（§Windows）、`crates/sandbox/src/appcontainer.rs` |

Windows AppContainer 又带来四个陷阱，全都对编译检查和只检查自己参数的测试不可见：

| 陷阱 | 发生了什么 | 规则 |
| --- | --- | --- |
| "关闭"的句柄不是零 | `GetStdHandle` 对被重定向掉的流返回 `INVALID_HANDLE_VALUE`（**-1**）；代码测的是 `!= 0`，于是 `STARTF_USESTDHANDLES` 带着 -1，每次 spawn 都以 87 失败。Win32 里的可空性是按 API 定的，不是"零表示缺失"。 | 同时检查 `0` **和** `INVALID_HANDLE_VALUE`。 |
| 空 vector 仍然有地址 | `SecurityCapabilities.capabilities` 总是取 `vec.as_mut_ptr()`；对空 `Vec` 那是一个悬垂的对齐指针（0x8），内核拒绝了该属性。空时必须给 `null`。指向"无"的指针只有在它真的是 null 时才合法。 | `if empty { null_mut() }`。 |
| 包装层丢掉了让它安全的那面旗 | `crates/sandbox/src/lib.rs:wrap_command` 为 re-exec 路径重建命令（`*command = wrapped`），而 `CREATE_SUSPENDED` 标志挂在旧命令上。于是子进程在 `after_start` 把它收进 job 之前就跑了，`AssignProcessToJobObject` 以 `ERROR_ACCESS_DENIED`（5）失败。`CommandExt` 只有 setter 没有 getter，所以标志必须在包装**之后**应用。 | 顺序：构造 → 包装 → 设标志。 |
| re-exec 上的 `env_clear()` 是个 bug | 包装层被以净化过的环境启动，于是它没有 `SystemRoot`，它自己的 `CreateProcessW` 以 `ERROR_ENVVAR_NOT_FOUND`（203）失败——而手工跑同一条命令却好的，因为手工的 shell 有环境。`env_clear` 的意思是"没有环境"，不是"没有额外变量"：改为复制调用方显式的 `env`/`env_remove` 调用。 | 先继承，再减。 |
| 一个没有工具能用的正确沙箱 | 遏制生效了，而 `git` 坏了，因为 AppContainer 起来时什么都不能读，而 Go 树从来不需要授予**读**权限。内核层的修复（一个 `extra_read` 列表，按 `extra_write` 的方式解析）本身不够：得有人决定**哪些**路径，而那个决定属于一个可审的文件，不属于一个 C struct。 | 机制在
`crates/sandbox`，策略在 `crates/cli/src/grants.rs`，外加一个能替换它的配置键。 |
| 悄悄禁用了严格路径的残留物 | 攒了 106 个 `hearth.sandbox.*` profile，profile 服务开始拒绝新名字——沙箱于是退到共享身份，那比完全访问**窄**，但不再是每 workspace 一份。profile 服务没有枚举调用，所以这些残留物直到去数目录才被发现。 | `hearth doctor --prune-profiles`，以及一个被公告出来而不是悄悄采取的退路。 |
| 一个正确但不可用的授权 | 只给 `~/.gitconfig` 授读仍然失败：工具会先 stat 路径的每一段，所以 `AppData\Local` 上缺一个 `FILE_TRAVERSE`，就在 `git` 打开任何东西之前把它杀了。缺的那个权限在**另一个对象**上，不是被授权的那个。 | 授权要走上层目录，而测试的文件放在三层之下。 |
| 一次要花几分钟的权限变更 | 让那次上行在**写**的时候变便宜才是陷阱：设置 DACL 会让 Windows 重新评估整个子树的继承——`%TEMP%` 9 s，`AppData` 20 s，profile 根 25 s，`E:\` 160 s。每条命令要花几分钟的正确代码，是一个带着合理借口的坏产品。 | 跳过 ACL 已经给了权限的目录（通过 `GetEffectiveRightsFromAclW` 询问）、永不碰卷根、把 workspace 授权限制在目录本身，并且当真发生停顿时把停顿打印出来。 |
| 一个活得比真相更久的重试 | 清理 57 个残留 profile 花了 182 s 却一个都没删掉：每个名字都被 `0x80070005` 拒绝，因为 profile 服务在那一刻拒绝**一切**，而一次全新的创建证明了这一点。按项重试把一个共享条件当成了 57 个独立失败。 | 用完整预算探一次，第一次探测失败且什么都没删时就停下，并说明服务正在拒绝——3.9 s 和一个运维能据以行动的答案。 |
| 一个你没测过的相关性 | 两个变量一起动：在 `D:\魔改版Agent框架\...` 下 profile 失败，在 `%TEMP%` 下成功，于是路径里的非 ASCII 字符看起来像原因，还造了一个**永远不可能生效**的绕法（从 ASCII 副本 re-exec）——失败的那个进程连副本的目录都建不出来。真正的变量是强制完整性标签：项目树被标成 `Low`，它的二进制以 Low 完整性运行，而一个 Low 进程既不能写 `%LOCALAPPDATA%`，也不能读一个 Medium 对象的 DACL。每个"语言"实验都是在带标签的树里跑的；每个"对照"都在树外。 | 一次只改一个变量；当一个假设预言了某个机制时，去看那个机制（`whoami /groups` 显示出 `Mandatory Label\Low` 只差一条命令）。然后让失败点名那个条件、并打印修法，而不是留一个把它藏起来的绕法。 |
| 一个用错边界去比较的身份检查 | `integrity_rid() < 0x1000` 永远不会触发，因为 Low **就是** `0x1000`；那条提示恰好在它为它而写的那台主机上保持沉默。 | 对着你真正想表达的那条边界比较（`< Medium, 0x2000`），并用含义而不是例子给常量命名。 |
| 一个清理自己父目录的测试 | 桥接测试把插件根目录直接建在 `%TEMP%` 里，结束时删掉 `root.parent()`——那正是 `TEMP` 本身。Windows 拒绝删除打开的文件，所以这个破坏几个月都不可见；托管的 Linux 作业拥有 `/tmp`，把它删了，顺手带走其他每个测试的临时目录。 | 每测试一个 base 目录（`<base>/plugins`、`<base>/experts`）；测试只许删自己的。 |
| 一个把某台机器网络状况编码进去的构建开关 | `hearth-bench` 会调 `cargo build -p hearth-cli` 并总是带 `--offline`，这在有镜像的开发机上是对的，在 registry 为空的 runner 上是致命的（`failed to download aho-corasick`）。 | 由调用方决定：离线机上 `HEARTH_BENCH_OFFLINE=1`，工作流里不设。 |
| 一个在这里是一段、在那里是两段的路径 | `.cargo/config.toml` 钉了 `target-dir = "D:/hearth-target/hearth"` 来躲 MSVC 的 `D8050`。对 Linux 的加载器来说那是一段含冒号的字符串，ubuntu 作业于是死在 `failed to join paths from $LD_LIBRARY_PATH together`。 | 受版本管理的文件里不钉任何机器相关路径；在本地脚本里推导，让 CI 设自己的。 |

## 9. 风险

| 风险 | 缓解 |
| --- | --- |
| 编译时间让迭代难受 | 十个小 crate、只在 release 里开 `thin` LTO、不用异步、除了 serde 不加 proc-macro crate |
| 两棵树并存期间漂移 | 已解决：Go 树于 2026-11-17 删除；现在改动一个 oracle 是对 `tests/` 的一次有意编辑加上两个读取器的测试，而不是静默的移植漂移 |
| 只有 Windows 才有的 C 工具链怪癖再次冒头 | ASCII target 目录不再被提交（那是我这台机器需要的东西，不是这个项目需要的）；`check.ps1` 在 checkout 路径非 ASCII 时推导它，CI 设自己的 |
| Rust 重写卡在 60% | 各波可独立发出；W0–W1 已经替换了存储层，而 Go 二进制在此期间照常工作 |
| agent 写的 Rust 能编译但不地道 | 每个 crate 都有一份带 `may_import`/`must_not` 的 README，而 `archcheck` 强制的是图谱，不是风格 |