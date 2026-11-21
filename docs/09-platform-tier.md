# ADR-0003 —— 平台层：缺什么，用什么语言写

- **状态：** 2026-11-17 接受，P1–P4 已交付。下面每一波的状态行都写明测了什么、没测什么。本 ADR 刻意没造的那一样东西是纯 Rust 的文档写出器（§5，P4）：Office 是一个技能加一个程序探针，而 Rust 的 `.docx` 写出器只有在"装一个二进制就能用"成为硬需求时才值得写。
- **决策人：** 用户。本文记录缺口清单、语言决策与波次计划；它不授权开工。
- **不取代任何东西。** [08-rust-port.md](08-rust-port.md)（ADR-0002）仍然管着内核的语言。本 ADR 回答下一个问题：内核是 Rust，*那些我们从没写过的部分呢？*

## 0. 被纠正的那个判断

ADR-0002 移植的是一组冻结的接口面（`docs/contracts/`），而不是它所取代那棵树的功能全表。那些接口面本身又是个子集：M4 是 store/import/summarizer，M5 是 MCP/skill/subagent，M6 是 sandbox/exec/danger/doctor。**平台层从来没有过一个接口面**，所以平台层从来没被写过——不是被删除，是从未被规定。

这就是"为什么能力少了"的诚实读法。本文剩下的部分，是"要把这东西规定出来并造出来需要付什么"。

## 1. 缺口，在本机上测出来的

证据列说明每一行是怎么查的。"declared（只声明）"意味着存在一份契约或一个常量，而没有任何代码实现它——最糟的状态，因为从外面看它像是做完了。

### 1.1 工具级缺口（一个 `Tool` 实现就行，没有结构性问题）

| 能力 | 原版 DSH | hearth 现状 | 证据 |
| --- | --- | --- | --- |
| `web_search` | 有 | **缺** | 没有任何工具叫这个名字；`crates/tool/src/builtin/` 里只有 5 个文件 |
| `web_fetch` | 有 | **缺** | 同上 |
| `read_image` | 有（`read_image`、`acp_read_original_image`） | **缺** | `crates/llm/src/contract.rs` 的 `Message.content` 是 `String`，所以发不了图片部分 |
| `worktree` | 有（`create_worktree`） | **缺** | 全树没有一处调 `git worktree` |
| `present`（交付一个文件） | 有 | **缺** | 没有这个工具 |
| `POST /messages` 上的附件 | 有 | **已声明，比丢掉还糟** | 服务端校验了 `kind`/大小，然后把**整段 base64 正文写进 `user/message` 事件**：每个日志读取器都要为它付费，`GET /events` 会返回它，分叉会逐字复制它，而只追加日志永远忘不掉它——同时没有任何东西投影它，所以没有任何模型看见过一个字节。P1 已修：正文进 `<dataDir>/blobs/`，事件只留 `{kind,id,mediaType,name,bytes,path}` |

### 1.2 运行时缺口（一个子系统，新 crate）

| 能力 | 原版 DSH | hearth 现状 | 证据 |
| --- | --- | --- | --- |
| 定时任务（rrule） | 有，4 个工具 | **缺** | 没有定时器，没有 rrule 解析器 |
| goal（跨轮自动续跑） | 有，3 个工具 | **缺** | 没有持久化的 goal，没有续跑循环 |
| 插件运行时 | 有，Cordis services/events/slots | **缺** | `architecture.yaml` 里 16 个 crate，没有一个加载器 |
| 插件市场 | 有，索引 + 安装 + 版本 | **缺** | `plugin_manager`/`plugin_market` 没有对应物 |
| agent 团队（并行 + 任务板） | 有，原生派发 + `wait_agent` | **部分** | `crates/subagent` 是*串行*委派，没有任务板，没有 inbox |
| ACP 式可寻址压缩 | 有（compress/decompress/search） | **只有摘要压缩** | `crates/session/src/compact.rs` 折成一条摘要；没有随机访问 |

### 1.3 配置/内容缺口（是文件，不是代码）

| 能力 | 原版 DSH | hearth 现状 | 证据 |
| --- | --- | --- | --- |
| 专家名册 | 有，22 个事业部 | **缺** | `crates/config` 没有 expert 键 |
| 专家团队 | 有，带一个协调者提示词 | **缺** | 同上 |
| agent 预设 | 有 | **缺** | `config.profile` 存在，但只认 `"default"`（`crates/server/src/server.rs` 的 `defaultProfile`） |
| Office 文档 | 有，通过技能 | **缺** | `crates/tool` 里没有 docx/xlsx/pptx 路径 |
| 图像生成 | 有，通过 MCP | 可达 | MCP 客户端已经存在；树内不需要加东西 |

## 2. 语言决策

### 2.1 规则

**凡是随二进制一起发布的，只用一种语言。** Rust。内核的既有属性是 7.0 MB、九个直接依赖、没有 async 运行时、`--version` 5 ms、空闲 6.3 MB——在 [dsh-comparison.md](dsh-comparison.md) 里测的。每多一个用第二种编译语言写的模块，就同时付三样东西：CI 里第二条构建链、第二套交叉编译矩阵、以及一层错误边界上的翻译。下面这些缺口没有一个够 CPU-bound 到值得付这个价。

恰好有三个地方，"另一种语言"才是*对的*答案，而它们都不会把编译器塞进我们的构建：

| 在哪 | 语言 | 为什么 |
| --- | --- | --- |
| 插件*本体* | 插件作者用什么就用什么 | 宿主通过 stdio 上一套有文档的协议通信；见 §2.2 |
| Office 文档 | Python 或 Node，由技能作为外部程序调用 | Rust 的 docx/xlsx 生态不成熟；原版也是委托给库 |
| Web UI | TypeScript（已经如此） | `web/`，35 个文件，78 个测试 |

### 2.2 决策，一个缺口一条

| 缺口 | 决策 | 理由（以及落败的备选） |
| --- | --- | --- |
| `web_fetch`、`web_search`、市场抓取 | **Rust**，新的 L1 crate `hearth-net` | TLS 已经在预算里了（`rustls`+`ring`+`webpki-roots`）。把 `crates/llm/src/openai.rs` 里那个阻塞 HTTP 客户端的通用部分抽出来。*落败：* 单独一个 Go 辅助程序——没有吞吐量论据，而且会把工具链翻倍 |
| `web_search` 后端 | **可配置端点**，不内置抓取 | 搜索需要一个服务；无 key 的 HTML 抓取会静默坏掉。配置键 `search.endpoint`/`search.key`，缺了 ⇒ 工具带着清楚的报错拒绝（fail closed） |
| `read_image` | **Rust**，P4 完成 | `Message` 多了 `parts: Vec<ContentPart>`（`serde(skip)`，所以冻结的契约 JSON 逐字节不变），OpenAI 构造器发出带 `data:` URI 的 `{"type":"image_url"}`，`read_image` 通过 `Result.parts` 返回字节——永不走日志，日志里留的是 `meta.imagePath`/`mediaType`/`bytes`，后续轮次重新读文件。端到端实测：第二个请求体里含 `data:image/png;base64,…`，21 行的日志里没有 `iVBOR`，删掉文件后下一轮报告没有图片 |
| `worktree` | **Rust**，shell 出去调 `git` | `crates/tool/src/builtin/shell.rs` 已经通过 `hearth-exec` spawn，所以树杀和沙箱免费继承 |
| `present` | **Rust**，很小 | 写一份 Web UI 会读的 manifest；没有新依赖 |
| 附件（文件/图片） | **Rust**，P1 完成 | 正文住在 `<dataDir>/blobs/`（`crates/session/src/blobs.rs`），事件只保留一个引用，`GET /api/sessions/{id}/attachments/{id}` 提供字节。像素在 P4 到达模型：同一份 `Attachment` 列表通过 `crates/cli/src/attachments.rs` 读回来，以 `ContentPart::Image` 交给 `hearth_llm` |
| `schedule` | **Rust**，新的 L2 crate `hearth-schedule` | rrule 是解析，不是算力。写一个**子集**（FREQ/INTERVAL/BYDAY/BYHOUR/BYMINUTE/COUNT/UNTIL），不要依赖。*落败：* 拉一个完整的 rrule crate（新依赖、新依赖边） |
| `goal` | **Rust**，新的 L2 crate `hearth-goal` | 一个状态机加一个续跑循环；持久化成事件，所以重启后还在 |
| 插件运行时 | **Rust 宿主 + 进程外插件，走 stdio JSON-RPC** | MCP 客户端（`crates/mcp`，L1）已经在做完全一样的事，而且 §2.3 在这套栈上测过往返开销：空载荷 37 µs / 4 KiB 56 µs，对照 1,203 ms 的模型调用。一个插件 = MCP 形状的服务器 + 生命周期钩子。*构造上*就与语言无关，零新依赖，而且沙箱已经能容住子进程。*落败：* 见 §2.4——嵌入引擎那几个选项被真正构建了一次重新定价，而当初用来反对它们的体积论据是错的 |
| 插件市场 | **Rust**，L2 `hearth-market` | HTTP 索引 + tarball + 校验和；`ring` 已经在为哈希服务 |
| 专家、专家团队 | **Rust 加载器**（`hearth-experts`，L1）读 **Markdown + YAML frontmatter** | 与技能同形状（`crates/skill` 今天就在读 frontmatter），所以格式已经被证明过。提示词是内容，不是代码 |
| agent 团队 | **Rust**，把 `hearth-subagent` 扩成 `hearth-team` | 串行委派已经存在；团队加的是并行 worker、共享任务板和一个 inbox |
| Office | **技能 + 外部程序**，运行时探测 | 不发布任何解析器。程序不在时技能就明说（fail closed），而不是写半个文件。*暂时落败：* 纯 Rust 的 `docx-rs`/`umya-spreadsheet`——只有当"单文件安装"变成硬需求时才重新讨论 |
| ACP 式压缩 | **Rust**，新的 L2 `hearth-context` | 有意思的部分是寻址模型（块 id、恢复、搜索）加缓存账本，不是吞吐量 |
| 图像生成 | **MCP**，树内什么都不做 | 今天已经可行 |

### 2.3 一次往返要多少钱（在这套栈上测的）

对进程边界最直接的反问是延迟，所以在选定它之前先测了——然后又正经测了一遍，因为第一次测的是代理指标。

**已作废：** 这里原来的数字（空载 80.6 µs / 4 KB 118.3 µs / 64 KB 343.7 µs）来自两个*Node* 进程互相通话。那测的是 Node 的管道，不是这个宿主的。工具结果上的 `durationMs` 字段也不是替代品：`crates/tool/src/executor.rs:74` 记的是 `as_millis()`，所以每一次亚毫秒调用都读成 0 或 1。

真正的测量是 `crates/plugin/examples/ipc_cost.rs`：Rust 宿主启动一个 Node 插件，然后对每种载荷大小做 2,000 次串行 `tools/call` 往返（先丢掉 100 次预热），用 `Instant` 计时。进程内的那些行是对照组——同样的 `serde_json` 编码/解码，但没有边界——所以管道那些行读出来是*地板 + 边界*。

```text
cargo run --release -p hearth-plugin --example ipc_cost --offline
```

Windows，node v24.21.0，连续两轮（p50 / p95，µs）：

| 测量 | 第 1 轮 | 第 2 轮 |
| --- | --- | --- |
| 管道：空结果 | 37.5 / 71.2 | 36.3 / 71.8 |
| 管道：4 KiB 结果 | 56.0 / 96.1 | 51.7 / 92.8 |
| 管道：64 KiB 结果 | 230.7 / 400.0 | 240.8 / 428.2 |
| 进程内：空结果 | 1.2 / 2.1 | 1.1 / 1.2 |
| 进程内：4 KiB 结果 | 2.8 / 5.8 | 2.8 / 3.5 |
| 进程内：64 KiB 结果 | 24.3 / 59.9 | 24.1 / 61.0 |

两个结论，方向相反：

1. **相对于进程内调用，这个边界很贵。** 空结果约 31×，4 KiB 约 20×，64 KiB 约 9.5×。任何声称进程边界"和函数调用一样快"的人都是错的，而且载荷越小，比值越差。
2. **相对于一个 turn 实际花掉的东西，它看不见。** 这个项目上一次模型往返实测是 **1,203 ms** = 1,203,000 µs。一次 37 µs 的插件调用是它的 **0.003%**。一千次这样的调用花 37 ms——还是一次模型调用的 3%。

决定的是第二条，因为一个 turn 是一次模型调用加几次工具调用，不是一段 IPC 热循环。

**真正要紧的代价是启动，不是调用。** Spawn + 握手在两轮里测到 **44.9 ms** 与 **36.5 ms**——大约是**单次调用的 1,000×**。本机上光 `node --version` 就是 p50 17.8 ms，所以其中大约一半是解释器自己的启动，剩下是模块加载加我们的握手。这就是为什么 `chat` 为整个 turn 持有一个 `Host`，而 `Host::load` 是唯一会 spawn 的东西：一个每次调用都起子进程的设计，会把一个 1,000 次调用的 turn 从 37 ms 变成 45 秒。**在这里保护性能的那个决策是"让子进程活着"；不是传输方式的选择。**

它同时让"想要快就用 Rust 写"变成一条可测的主张——而测量纠正了它。这一节的第一版说原生插件会快"大约一个数量级"。那是从 `node --version` 花 17.8 ms 推出的，而且**强了大约 2×**。把 `crates/plugin/src/bin/hearth-plugin-native.rs` 构建出来并且通过 `runtime: "exec"` 加载之后，三轮给出：

| | Node 插件 | 原生插件 |
| --- | --- | --- |
| spawn + 握手，p50 | 45.6 / 44.0 / 43.2 ms | **9.8 / 8.5 / 8.8 ms** |
| 空结果，min | 30.3 / 24.6 / 25.3 µs | **15.7 / 15.8 / 15.8 µs** |
| 4 KiB 结果，min | 33.8 / 32.0 / 32.2 µs | 23.3 / 23.6 / 20.5 µs |
| 64 KiB 结果，min | 106.6 / 100.3 / 107.9 µs | 72.0 / 75.1 / 74.1 µs |

所以原生启动快约 **5×**，不是 10×；每次调用的地板大约减半，分布明显更紧（没有 JIT，没有 GC）。`min` 列是每次调用代价的诚实比较基准：Node 的 p50 在五轮里从 36 µs 漂到 63 µs，而它的 min 一直靠近 25 µs，所以只拿 p50 算出来的比值会是噪声。

**而它仍然没有决定任何事。** 9.8 ms 是一次模型调用的 0.8%；45 ms 是 3.8%。两者都是*每个插件每 turn 付一次*。所以插件语言之间最大的性能差在一次模型往返的 4% 以内——这就是为什么 §2.6 是按分发与编写体验来定插件语言，而不是按速度。

**这没有解决什么。** 这些是不带工具工作的往返。一个做真实 I/O 的插件，代价就是那个 I/O 的代价。如果将来的插件需要一小段小调用的热循环，答案是批量化这些调用或者把那个插件写成原生的——不是搬动宿主。

所以在这个边界上，"哪种语言最快"是错的问题。在 profile 里显现出来的代价是分发（装得上吗）、编写（写得出来吗）、容纳（停得下来吗）——而子进程在这三样上都赢。

### 2.4 被否掉的选项，以及每一条否掉到底值多少

- **调度器用 Go。** 没有吞吐量论据，而且它会把我们刚删掉的工具链重新引进来。Go 唯一给过我们的是构建速度，而一个调度器是 200 行。
- **为"热"路径用第二种编译语言。** 没有热路径：实测到的代价是启动和内存，两者都已经有答案。
- **把 Python 塞进二进制**（比如捆一个解释器）好让 Office 文档离线可用。它把一个 7 MB 的文件变成一个 40 MB 的安装包，外加一个许可证问题。
- **用 Rust/WASM 重写 Web UI。** UI gzip 后 265 kB，门禁全绿；WASM 重写买不到任何可测的东西。
- **`cdylib` 方式加载插件。** ABI 与版本的深渊，而且它会破坏 `panic = "abort"`。没有重测；反对理由是结构性的，不是体积。
 
#### 嵌入引擎：用一次真实构建重新定价

本文第一版否掉嵌入引擎，部分理由就是体积（"引入一个大依赖"、"比整个二进制大一个数量级"）。**那从来没测过，而且对 QuickJS 来说错得离谱，大约差了 30×。**
`rquickjs 0.14` 在离线缓存里，所以在 workspace 外面用同一个 release profile（`lto = "thin"`、`codegen-units = 1`、`strip`、`panic = "abort"`）建了一个临时 crate：

| | 实测 |
| --- | --- |
| 裸 hello 二进制 | 122,368 B |
| 同样，但嵌入 QuickJS | 1,197,056 B |
| **差值** | **1,074,688 B (1.02 MB)** |
| 一次性构建成本 | 13 s |
| 进程内 `eval` + JSON，p50 | **3.0 µs**（管道是 37 µs） |

所以体积那条反对撤回了：一个 8.5 MB 的二进制上多 1 MB，不构成否掉任何东西的理由；而进程内调用比管道快约 12×（§2.3 已经表明，相对于 1,203 ms 的模型调用，这不重要）。

**真正取消它资格的是 API 面。** 探一下插件作者在没有任何宿主绑定的情况下能够到些什么：

```
JSON object   Math object   Promise function   async/await Promise
console undefined   fetch undefined   require undefined
process undefined   setTimeout undefined   TextEncoder undefined
```

它提供*语法*，拿掉*值*。一个写插件的 agent 想抓一个 URL，写下 `fetch(...)`，在 Node 18+ 上就能用；在 QuickJS 上则是 `fetch is not defined`，除非有人手工把它绑上。每一项能力——定时器、编码、文件系统、套接字——都变成我们必须设计、实现、写文档并保持稳定的宿主 API。而容纳能力从操作系统（杀掉子进程）搬进了我们自己的代码（QuickJS 的内存上限与中断处理，还得接对）。

那是**另一个产品**：一个有宿主定义 API 的脚本沙箱，不是一个能用上既有生态的插件宿主。将来为不可信的进程内脚本造它是合理的；它不是"更便宜地拿到这个项目想要的插件"的办法。

**没测过，所以不否掉：** `wasmtime`（WASM）、`boa`（纯 Rust JS）、`rhai`、`mlua`、`deno_core`/V8 都**不在那个 1,668 个 crate 的离线缓存里**，而这台机器上 crates.io 不可达。WASM 尤其能回答 §2.6 的分发问题——一个产物、所有平台、不用装解释器、语言无关——所以有网络时它值得一次真正的评估。没有构建就把它记成"已否掉"，会重犯这一节刚刚纠正的那个错误。

重跑 QuickJS 那个测量不要碰这个 workspace，也不能碰：把 `rquickjs` 加成 dev-dependency 会让它进入每一次 `--all-targets` 构建，并破坏依赖预算。在仓库**外面**建临时 crate，`CARGO_TARGET_DIR` 指到 ASCII 路径，给它 `rquickjs = "0.14"` 和与根 `Cargo.toml` 相同的 `[profile.release]`，然后跟一个 `main` 只有 `println!("hello")` 的 crate 对比：

```rust
let rt = rquickjs::Runtime::new()?;
let ctx = rquickjs::Context::full(&rt)?;
ctx.with(|ctx| {
    let out: String = ctx.eval("'hello, ' + (21 * 2)")?;   // "hello, 42"
    let kind: String = ctx.eval("typeof fetch")?;          // "undefined"
    Ok::<_, rquickjs::Error>(println!("{out} / fetch {kind}"))
})?;
```

体积差值是那两个 strip 过的可执行文件之差，不是任何一方的绝对大小。

### 2.5 怎么做这种选择

"哪种架构最好、最快又不损失太多"在没被变成四个测量之前是没有答案的。这是在这里奏效的顺序，也是从一开头就照做会省下时间的顺序。

**1. 优化分子之前先找到分母。** 一个 turn 是一次模型往返加几次工具调用。在这个项目上那是 1,203 ms 对 37 µs。对那 37 µs 的任何优化都是舍入误差，不管比值看起来多漂亮。先把分母写下来；它会不靠基准就取消掉大多数候选。

**2. 冷路径和热路径分开测。** 这里它们差 1,000×（spawn 36–45 ms 对调用 37 µs）。一个每次调用看起来慢的架构可能是对的，而一个每次调用看起来快的架构如果每次都付启动代价，可能是灾难性的。这个设计里保护性能的决策是*让子进程活着*，不是传输方式。

**3. 给"这个选择去掉的东西"定价，不只给它加的东西定价。** 嵌入引擎会去掉进程边界，并加上：1.02 MB 二进制（§2.4 测过；这段文字的早先草稿写的是"几十兆"，错了 30×）、第二条构建链、第二套交叉编译矩阵、失去 `panic = "abort"`、失去"杀掉子进程"这个容纳叙事，以及为运行时过去提供的每一项能力手工造一套 API 面。那些不是微秒，而且它们是在每一次构建、每一次事故上付费，不是每次调用付一次。

**4. 问哪条属性才真正稀缺。** 对一个插件系统来说，稀缺的属性是*陌生人能不能写一个*、*能不能装上*、*能不能停掉*。延迟是充裕的——那儿就摆着 1,203 ms 的预算。选那个花充裕资源去买稀缺资源的设计。

**5. 检查哪条轴你压根没测。** 四次时间测量产生了自信的语言建议，而内存——这次重写真正被推销的那条属性——一次都没测过。§2.7 就是测了之后发生的事：每个 Node 插件 19.1 MB 私有内存，插件之间零共享，而复用的节省比语言之间整个差距还大。建议活了下来，但它的*推理*是不完整的，而下一次决策会在错的轴上做。

这个项目踩到的三种失败模式，值得点名，因为它们看起来都像进展：

| 错误 | 它看起来像 | 什么修好了它 |
| --- | --- | --- |
| 测了一个代理指标 | "80 µs 往返"，来自两个 Node 进程，被引用得像是我们的宿主 | 测真正的那一对；把脚本留在仓库里，让这个主张可重跑 |
| 把粗粒度字段当细粒度读 | 把 `durationMs: 2` 当作关于微秒的证据 | 引用之前先在源头看分辨率（`as_millis()`） |
| 优化你碰巧测了的那条轴 | 四个延迟数字、零个内存数字、一个自信的建议 | 列出这个设计赖以推销的属性，逐条确认有数字（§2.7） |

还有那个被做出的选择的诚实代价：Rust 的迭代循环比 Go 或 TypeScript 慢——这里一次完整 workspace 测试约 30 s，而借用检查器会把设计打回来重做。那是对*编写*速度的真实税，它在上面任何一个运行时数字里都看不见，而它是为了换 §2.1 那些运行时属性才被接受的。假装它不存在会把这份文档变成推销话术而不是决策记录。

### 2.6 一个插件该用什么语言写

§2.2 说"插件作者用什么就用什么"，当作者是一个已经会某门语言的人时，这是对的。当作者是一个 **agent** 时，这是不完整的，因为 agent 没有需要保留的熟悉度。它的代价是一次迭代的价格和一个产物的发布价格。

**首先，三个被当成一个问题来回答的问题。** "用什么语言"实际上是：

1. 什么**运行时**执行插件代码？（`node`、`python`，或者 entry 本身）
2. 作者敲的是什么**语言**？（JS、TS、Python、Rust……）
3. 分发的是什么**产物**？（源码 tarball，或者按平台分的二进制）

协议已经把这三者分开了：`runtime` 是一个三元的封闭集合，entry 是任意文件，所以**宿主与语言无关，这里的任何选择都不是排他的**。真正在定的事情小得多——*哪一条修成柏油路*：脚手架、示例、文档、陌生人最先找到的东西。选一条柏油路不会去掉另外两条。

**树里实际有几套栈**（`git ls-files`，按扩展名）：

| 文件 | 是什么 | 状态 |
| --- | --- | --- |
| `.rs` 218 | 内核，23 个 crate | 主要 |
| `.tsx` 21 + `.ts` 19 | Web UI | 主要 |
| `.js` 2 | `crates/plugin/tests/reference-node/{index,slow}.js` | 协议参照，不是我们开发的栈 |
| `.py` 1 | `crates/plugin/tests/reference-node/smoke.py` | 18 行的第二运行时证明 |
| `.ps1` 2、`.sh` 1 | `check.ps1`、`scripts/install.{ps1,sh}` | 胶水 |
| `.yml` 2、`.yaml` 1 | CI 工作流、`architecture.yaml` | 配置 |
| C | 通过 `rusqlite` bundled 的 SQLite、`ring` | 今天就已经在编译 |

所以有**两套主要栈——Rust 与 TypeScript**——而 JS/Python 是同一套协议的两个参照实现。因此"加 JS 插件"不算明显多了第三套栈，但它*确实*是前端 TypeScript 旁边的第二种编写语言，而这个代价是真的。

**分发曾经是硬约束；它已经被解除了。** 原来的 `crates/market/src/index.rs` 把 bundle 描述成
`name/version/description/author/keywords/homepage/tarball/sha256/size/engines`，**没有 `os`/`arch` 字段**，并且要求 `name` + `version` 唯一，于是 `myplugin 1.0.0` 的六个平台构建连列都列不上去——一个原生插件根本没法分发。现在改了：`Bundle` 带可选的 `os`/`arch`（两者都缺 = 平台无关源码，两者都有 = 原生构建，只声明一半的拒绝），唯一键是
name + version + os + arch，`resolve` 在选版本之前先按宿主平台过滤。接受的拼写就是发布流水线的那些
（`windows`/`linux`/`darwin`、`amd64`/`arm64`），所以市场不会拒绝这个项目自己构建的产物。

缺口的另一半是，装好的 bundle 落在 `market/bundles/`，而插件宿主只读 `plugins/`，于是 `market install` 结束在一个没有任何东西加载的目录。宿主现在读**两个根**（`Host::add_root`），而两个根里都出现的名字会被拒绝、并同时点出两条路径，而不是按顺序解析。在发布二进制上端到端验证过：
`market install native` → `plugin list` 看得到它 → `plugin call native echo` 有回答；一个 linux-only 的 bundle 在 Windows 上被拒，并点名已发布的平台。源码插件仍然是"一个 tarball，在它运行时存在的地方都能装"。

**发布那一半现在也有了，所以环路闭合了。** `market` 以前没法*产出* bundle——它能装一个，而没有任何东西能造一个。`pack_dir`
（`crates/market/src/pack.rs`）用消费者读的同一套编解码写 tarball：读取器会检查的 ustar 头，包在 stored-block gzip 里，用索引校验器用的同一个 `sha256_hex` 算哈希。这是刻意的，不是为了方便——一个单独的打包器会是第二个实现，得靠人手跟第一个保持一，而往返测试把
`pack_dir` 的输出直接喂给 `install_bytes`，所以漂移会让测试挂掉，而不是让发布者挂掉。

命令是 `hearth market pack <dir>`：它读 `plugin.json`，写出
`<name>-<version>[-os-arch].tar.gz`，带 `--base-url` 时把一条记录合并进
`index.json`，用的就是索引强制的那个 name+version+os+arch 键（所以重新打包是替换而不是重复）。公开示例是
`examples/plugins/hello/`——一个工具、零依赖，就是该复制的那份文件。

在发布二进制上对着真实的 `python -m http.server` 端到端验证过：
`pack` → `search` → `install` → `plugin list` → `plugin call hello greet` →
`hello, end to end`。用一个脚手架生成的插件重做一遍，走到了合并路径，然后踩到了重复守卫：同一个名字同时存在于
`plugins/`（手写的）和 `market/bundles/`（装进去的），宿主拒绝并点名两条路径，而不是挑一个。那是守卫在工作，也值得当工作流里的坑记住——发布者应当从一个不在宿主搜索路径上的目录打包。

**TypeScript 也不需要构建步骤——这是一个选项，不是 agent 的默认。** 这是测出来的，不是假设的：Node 24.21.0 直接执行 `.ts`，而一个 manifest 写着 `runtime: "node"`、`entry: "index.ts"` 的插件——里面带 `interface`、类型标注和 `as Error`——在**发布二进制**上走完了整条路：

```
hearth plugin start tspeak   -> tspeak 0.1.0 speaks protocol 1
hearth plugin call tspeak greet --args {"name":"no build step"}
                            -> hello from typescript, no build step
```

**零宿主改动。** `Runtime::Node` 本来就是在跑 `node <entry>`；entry 是 `.ts` 不是特例。所以支持 TypeScript 不额外花什么，也不会在前端旁边多出一门语言。

它的限制也是实测的——Node 只是剥掉类型，并不编译它们，所以只有*可擦除*语法能用：

| 构造 | 结果 |
| --- | --- |
| `interface`、标注、`as`、泛型 | 可用 |
| `import { x } from './lib.ts'` | 可用（`.ts` 扩展名是必须的） |
| `enum` | `SyntaxError [ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX]: TypeScript enum is not supported in strip-only mode` |
| 构造函数参数属性 | 被拒，同一类错误 |

更老的 Node 需要 `--experimental-strip-types` 或者一次构建；市场索引里的 `engines.runtime` 存在的意义就是声明这一点。

所以建议，按顺序（agent 编写的默认在下面那一小节里定）：

| 场景 | 作者写什么 | 为什么 | 每个插件私有内存（§2.7） |
| --- | --- | --- | --- |
| **柏油路（agent 默认）** | **JavaScript**（`.js`，无构建） | 对 agent 最宽容：没有要写错的类型语法，写出来就能跑，一个产物通吃所有平台 | 19.1 MB |
| 类型对 agent 有帮助 | **TypeScript**（`.ts`，Node ≥ 23.6 上无构建） | 类型是一份约束输出的契约；但 strip-only 模式拒绝 `enum`/参数属性，所以它是选项，不是默认 | 19.1 MB |
| 工作已经住在某个库里 | **Python** | pandas/openpyxl/requests 就是待在那儿的理由；分发故事相同 | 7.5 MB |
| CPU 密集，或者不能假设有运行时 | **Rust**，走 `runtime: "exec"` | 冷启动约快 5×、每次调用地板减半、没有运行时依赖，**比宿主自己还便宜**；市场现在承载平台构建，宿主也读它们 | **0.7 MB** |
| 将来：不可信的进程内脚本 | 嵌入引擎 | 见 §2.4：1.02 MB，但没有 `fetch`/`console`/`require`——另一个产品 | 未测 |
| 一个产物、无解释器、任意语言 | **WASM** | 最强的未探索选项；**未测**，`wasmtime` 不在离线缓存里 | 未测 |

Rust 那一行是真的、也测过：`crates/plugin/src/bin/hearth-plugin-native.rs`
是一个能用的原生插件，`crates/plugin/tests/host_it.rs` 里三个集成用例通过
`exec` 加载它。在它存在之前，`Runtime::Exec` 只被当成一个能解析的字符串测过——其他每个用例都 spawn 一个解释器，所以一个坏掉的、不用解释器的 argv 会悄悄通过整个套件。

**不变的是：内核留在 Rust。** 这两个决策互相独立，§2.3 说明了为什么：边界值一个 turn 的 0.003%，所以插件的语言动不了内核自己的 8.5 MB 二进制、5 ms 启动或约 6 MB 空闲。为插件选 TypeScript 不是从 Rust 部分撤退——它跟"把 Web UI 放在 TypeScript 里"是*同一个*决策，所以不新增栈。

**但"内核保持小"不等于"机器保持小"。** 这一段早先的版本让 6 MB 空闲那个数字当作整个内存答案站着。不是的：每个启用的插件都是一个独立进程，有自己的解释器，而 §2.7 测了那会加上多少。两段要一起读。

#### 这套栈，是按"agent 能写插件"选的

决定性的需求，说白了就是：**稳定的性能，以及一个 agent 能轻松写的插件生态。** 这两条并不冲突，因为它们活在不同的层，而进程边界正是把它们分开的东西：

| 需求 | 活在哪 | 选择 | 为什么 |
| --- | --- | --- | --- |
| 性能稳定 | **宿主/内核** | **Rust**（保持） | 实测：8.5 MB 二进制、启动约 5 ms、空闲约 6 MB、插件调用 37 µs。插件的语言动不了这些（§2.3） |
| agent 好写 | **插件语言** | **解释型：JS / TS / Python** | 没有编译步骤、运行时立即反馈、agent 最熟练的语言 |
| agent 能写的最简单的东西 | **技能** | **Markdown** | 完全没有代码；技能系统已经在加载它 |
| 接上最大的既有存量 | **互操作** | **MCP** | 语言无关；10 个官方 SDK，而宿主已经在说它（`hearth-mcp`） |

所以选择是 **Rust 宿主 + 解释型插件 + Markdown 技能，通过 MCP 互操作**。
Rust *不是*默认的插件语言，而这正是要点："编译难"落在写宿主的人或者*选择*原生插件的人身上，永远不落在写 JS/Python 插件的 agent 身上，也永远不落在运行那个单一二进制的终端用户身上。

在解释型那一行里，具体到 **agent** 作者：

- **JavaScript 是默认的柏油路**（脚手架当前 `node` 输出）。它最宽容：没有要写错的类型语法，写出来就能跑，训练语料巨大。这修正了早先偏向 TypeScript 的建议——一旦"agent 好写"成为明确标准，没有类型语法陷阱比与 UI 的栈对齐更重要。
- **类型有帮助时 TypeScript 可用**（`.ts` 在 Node ≥ 23.6 上零构建运行，在 `host_it.rs` 里证明过）。类型是一份约束 agent 输出的契约，但 strip-only 模式拒绝 `enum` / 参数属性 / namespace——这些是 agent 会踩的陷阱，所以它是选项不是默认。
- **工作已经住在 Python 库里时用 Python**（pandas/openpyxl/…）。

这不是从 Rust 撤退，也不新增栈：Web UI 已经是 TypeScript，而插件边界本来就接受任何语言。生态证据指向同一方向——最完整的存量（MCP）是语言无关的，且有官方 Rust SDK（`rmcp`，crates.io 约 51.9M 次下载），所以一个 Rust 宿主能插进去，而不用把 Rust 强加给插件作者。单一 harness 里最大的存量（DSH，"Everything is a Plugin"）主要靠它那套语言无关的 MCP + Markdown 技能层增长，而这套宿主已经能消费；只有它进程内的 Cordis 模块够不着，那是一个子集，不是增长点。

**决策：保留 Rust 内核；把解释型插件那条路铺成柏油（默认 JS，可选 TS/Python）再加 Markdown 技能；通过 MCP 互操作。** 脚手架已经在输出对 agent 友好的默认，所以为了兑现这个选择，不需要重建任何东西。

### 2.7 一个插件要花多少内存（实测）

§2.3 里每个数字都是*时间*。内存是另一条轴，而它正是这次重写赖以推销的那一条——`hearth web` 空闲约 6 MB，而 Electron 原版占着好几个 GB。每个插件一个解释器可能悄悄花掉这个优势，所以它被测量了，而不是朝任何方向假设。

复现方式：

```text
cargo build --release -p hearth-plugin --bins --offline
cargo run --release -p hearth-plugin --example memory_cost --offline -- 120 3
```

它会保持子进程打开并打印自己的 pid；从另一个 shell 采样那个 pid 及其子进程（`Get-CimInstance Win32_Process -Filter "ParentProcessId=<pid>"`
加 `Get-Process` 取 private bytes，或者 `ps -o rss= --ppid <pid>`）。**三种运行时用同一种方法**，这样比较才公平。工作集包含共享的映像与 DLL 页；**private bytes 才是诚实的增量代价**，所以两个都记。

每种运行时一个空插件，外加持有它们的宿主：

| 进程 | 工作集 | **Private bytes** |
| --- | --- | --- |
| `node.exe`（一个插件） | 55.9 MB | **19.1 MB** |
| `python.exe`（一个插件） | 12.6 MB | **7.5 MB** |
| 原生 `exec` 插件 | 4.4 MB | **0.7 MB** |
| hearth 宿主本身 | 6.1 MB | 1.8 MB |

Node 自己的记账对得上，也解释了钱花在哪：`rss 55.9 MB`、
`heapTotal 6.0 MB`、`heapUsed 4.7 MB`、`external 1.9 MB`。**在一个 56 MB 的进程里，插件的代码只占 6 MB**；其余是解释器和 V8。那是一份为*"是 Node"*付的地板价，不是为作者写了什么付的。

**边际插件的花费和第一个一样。** 同时持有三个 Node 插件：55.9 + 55.9 + 55.9 = **167.7 MB 工作集，57.5 MB 私有**。什么都不共享。所以代价是按*进程*算的，而进程数才是要紧的乘数。

**多路复用是测出来的，不是推出来的。** 一个 Node 进程持有三个插件实例（各自通过工厂拥有自己的模块状态）是
**55.6 MB 工作集 / 19.7 MB 私有**——对比三个进程的 167.7 / 57.5。那是**省下 112 MB 工作集和 37.8 MB 私有**，把每插件私有成本从 19.1 MB 变成 6.6 MB。节省是真的，而且比语言之间整个差距还大。

多路复用要付什么，这也是为什么它没有被简单地做掉：

- **崩溃隔离。** 今天一个段错误或退出的插件只带走自己；宿主报 `child_exited`，这个 turn 继续。共享进程会让一个坏插件对同组每个插件都致命。
- **CPU 与卡死隔离。** 一个永远空转的插件今天是一个可杀的 pid，带 10 s 调用预算。在共享进程里它会阻塞别人。
- **一次协议改动。** 帧需要一个插件 id，因为一条 stdout 流会承载好几个插件的应答。`PROTOCOL.md` 今天没有这个字段。

**启用一个插件就立刻为它付费，不管有没有被调用。**
`crates/cli/src/wiring.rs:808-840` 在 wiring 期间加载 `plugins.enabled` 里的每个名字，因为模型在第一次请求之前就需要工具 schema，而那些 schema 来自活管道上的 `tools/list`。延迟 spawn 需要一份带失效故事的缓存工具列表；那是一个设计决策，不是疏忽，本文不做。

**这对 §2.6 有什么影响。** 建议对一两个插件仍然成立，而上面的内存列现在是它的一部分。它改变的是剩余工作的*优先级*：选 Python 而不是 TypeScript，每个插件省 11.6 MB 私有；而共享一个解释器每插件省约 12.5 MB，原生插件省 18.4 MB——所以**在这条轴上，进程架构压过语言选择**，而一个必须常驻的插件是 `exec` 或 WASM 最强的理由。

**DSH 对比，重新诚实地说一遍。** 五个 Node 插件加宿主大约是 **97 MB 私有 / 286 MB 工作集**，不是 6 MB。那仍然比原版在十个进程上占的 4,914 MB 少约 50×，所以优势仍在——但那个头条"6 MB"只在一个插件都没启用时成立，不带上这个限定就引用它，跟把 Node 对 Node 的往返当成我们自己的，是同一个错误。

## 3. crate 计划与层图

新 crate，全部遵守既有规则（依赖边只能指向更低或相同的层），并带上 `archcheck` 要求的 `README.md` frontmatter：

| crate | 层 | 可 import | 用途 |
| --- | --- | --- | --- |
| `hearth-net` | L1 | `hearth-core` | 通用阻塞 HTTP GET/POST：重定向、大小上限、超时。`hearth-llm` 只保留 SSE/chat 那部分整形 |
| `hearth-experts` | L1 | — | 解析专家/团队定义（Markdown + frontmatter） |
| `hearth-schedule` | L2 | `hearth-core` | rrule 子集、下一次触发、持久化 |
| `hearth-goal` | L2 | `hearth-core`、`hearth-session` | goal 状态机、续跑轮次 |
| `hearth-market` | L2 | `hearth-core`、`hearth-net`、`ring` | 索引抓取、安装、校验 |
| `hearth-plugin` | L2 | `hearth-core`、`hearth-mcp` | 插件宿主，走 stdio JSON-RPC；复用 MCP 传输 |
| `hearth-context` | L2 | `hearth-core`、`hearth-session` | 可寻址压缩块、恢复、搜索 |
| `hearth-team` | L2 | `hearth-core`、`hearth-session`、`hearth-subagent` | 并行 worker + 共享任务板 |

`hearth-mcp` 留在 L1，也不获得任何插件知识：是插件 crate 依赖它，不是反过来。`tools/archcheck` 与 `architecture.yaml` 在加每个 crate 的同一个提交里改——那是规则，不是建议。

新增工具（`crates/tool/src/builtin/`）：`web_fetch`、`web_search`、
`read_image`、`worktree`、`present`。这把 builtin 数量从 7 变成 12，而这个数字写在四个必须一起动的地方：

- `docs/contracts/cli.md:295` —— "Seven tools are registered unconditionally …
  `tools={n}` … is at least seven"；
- `crates/tool/README.md:16` —— 七个名字，逐个拼出来；
- `crates/tool/src/builtin/mod.rs:1` 与 `:21` —— 模块文档与 `all()`；
- `crates/cli/src/wiring.rs:14` —— "the frozen seven names"。

## 4. 为什么事件日志不需要升版本

新子系统想要新事件类型（`goal/*`、`schedule/*`、`team/*`、
`plugin/*`）。`AGENTS.md` 里的法则 #2 已经覆盖了这一点：**未知事件类型是数据——跳过并计数；未知*版本*是错误。** 所以类型是可加的，一个遇到 `goal/created` 行的 v1 读取器继续工作。

随之而来的义务：

1. 把每个类型加进 `EventType::ALL`（`crates/core/src/event.rs:50`），那是
   `validate` 查的地方（`:73`、`:138`）——不在 `ALL` 里的类型会被当成"does not match the v1 envelope"拒绝，所以这个添加是可测的。
2. 在 `tests/conformance/cases/events/` 下为每个新类型加一个 conformance 用例
   （今天 21 个：9 个接受，12 个拒绝）。
3. 永远不要把一个类型名复用于不同含义。如果载荷形状变了，那就是一个新类型。

## 5. 波次

每一波都可独立发布、独立验证。上一波的门禁没绿，下一波不开始。

**P1 —— 工具（最有用的最小一步，约一周）。**
`hearth-net`；`web_fetch`；`web_search` 挂在可配置端点后面；`worktree`；
`present`；附件按 §1.1 描述的方式做完（正文移出日志）。
*验收：* 在一个带本地 fixture 服务器的测试里通过 `hearth-net` 真实抓取一个真实 URL；`web_search` 未配置时带着具名理由拒绝；`git worktree list` 显示出新树；
`cargo test --workspace` + `check.ps1` 全绿；`Cargo.toml` 里没有新的依赖行。
*状态：已交付（`a02e61f`、`abeb8cc`、`d218b03`）。`hearth-net` 在零新依赖的情况下解析、抓取并渲染 HTML；八个端到端测试打在真实的 loopback 监听器上；附件正文租用 `<dataDir>/blobs/`，事件只留引用。*

**P2 —— 时间与记忆（约两周）。**
`hearth-schedule` + `hearth goal` + 续跑循环。*验收：* 对一份固定 rrule 语料的下一次触发表（rrule 子集就是 oracle）；一个 goal 能活过重启，因为它在日志里；续跑轮次有界，并在 `blocked` 停下。
*状态：已交付（`608ce96`）。59 个 schedule 测试，rrule 表在
`crates/schedule/tests/oracle.rs`；`goal/created|continued|ended` 在
`EventType::ALL` 里，所以重启后读到同样的预算；`--max-rounds` 数的是 turn，不是续跑（验收跑发现的一个 bug：第一轮就是创建 goal 的那一轮）。*

**P3 —— 平台层（1–2 个月）。**
`hearth-plugin`（协议文档 + 生命周期 + 一个用*非 Rust* 语言写的参照插件，用来证明那个主张）、`hearth-market`、`hearth-experts`、
`hearth-team`。*验收：***一个**完整的 Node 参照插件（DSH 的插件作者已经在用的语言，所以它的摩擦就是迁移路径）外加一个小的 Python 冒烟测试，只证明第二个运行时能加入协议——两个完整参照插件会是为了重复证明一条属性而买下两处维护面；一次市场安装校验一个校验和并拒绝被篡改的 tarball；一个专家团队派发两个成员并把他们的结果写到共享板。
*状态：已交付（`2b90ab5`）。那四个 crate 全绿（plugin 30+12、market 110、
experts 77、team 48），而 `hearth plugin|expert|team|market` 是 CLI 命令。
验收证据，全部在本机跑过：* 参照 Node 插件应答
`initialize`、四个工具、一个提示词和一个钩子；`tests/reference-node/smoke.py`
加入同一套协议——**18 行代码**（带注释块 31 行），不是验收句最初要求的十行，而这个差距是诚实的：那张封闭表加 id 回显加 `shutdown` 应答塞不进十行；市场的拒绝路径是对着真实的 `python -m http.server` 跑的，先用一个过大的、再用一个被篡改的 tarball（每次拒绝都点名两个数字，没有任何东西落进 `bundles/`）；
`hearth team run release-squad --max-parallel 2` 把三个名册成员派到一块板子上，每个任务由同名 worker 认领，转录在
`<board>/workers/` 下。

**P4 —— 上下文与内容（可选）。**
`hearth-context`（可寻址压缩）、Office 技能、若"单文件安装"成为需求则写纯 Rust 文档写出器。

为什么这是最后而不是最先，三行：

1. **P1–P3 做加法；P4 做改动。** 可寻址压缩需要在日志之上加块 id、恢复与
   搜索，这会动到 `projection`、`fork` 和转录的渲染语义。早先的波次是给日志加兄弟节点，不是给读它加新规则。
2. **早先的波次改变它的输入。** 压缩策略是照着"哪些会话会变长"调的，而 `schedule`、`goal`、`team` 都会让会话更长。拿今天的增长曲线去优化明天的负载，就是压缩预算最终只对不需要它的会话正确的方式。
3. **Office 是内容，不是架构。** 它是一个技能加一个程序探针：
   零内核风险，任何时候都能做。它排在最后是因为不紧急，不是因为难。

*状态：`hearth-context` 已交付；Office 与文档写出器没有开始。* 发出去的是 ADR 要求的那个寻址模型，之上的什么都没做：

- 一个块是**一个封闭的 `seq` 区间加一份摘要**，写成一条
  `context/compressed` 事件（`crates/context/src/block.rs`、`wire.rs`）。区间必须已经在日志里存在——块不能指向还没写下的记录——而且块不能重叠，所以"哪些块覆盖这个 seq"是单一答案而不是一条策略；
- **账本**把这些事件折回成区间（`ledger.rs`），这正是块能活过重启的原因：轮次之间内存里什么都不留；
- `to_llm_messages_hiding`（`crates/agent/src/history.rs`）丢掉被隐藏的记录，
  **并在工具配对关系上取闭包**——隐藏一次调用也隐藏它的结果，反之亦然，因为带着一条没有对应调用的工具消息的请求，会被每个 provider 拒绝。正是这一部分，本来会让第一个长会话的用户撞上一个 400；
- `compress` / `decompress` / `search_context` 住在 `crates/cli` 里并且永不写入：循环是唯一的写者，所以 `compress` 把载荷放在
  `result.meta.contextBlock` 里返回，循环去追加它，跟审批者返回一个决策完全一样。恢复是读；日志从不缩小。

*证据：`hearth-context` 14 个测试，`hearth-cli` 133 个 lib 测试（其中 8 个是给那三个工具的），新事件类型一个 conformance 用例（23 个用例），以及
`archcheck: ok`。块工具在所有模式下都是 `allow`，因为它们既不碰 workspace、不碰网络，也不碰子进程。*

**重新审视的触发器。** 如果长会话在模型的窗口填满之前就丢细节，`hearth-context` 就移到 P2 前面——损失在上下文窗口里，不在渲染里（10,001 个事件已经能 60 fps 滚动，在 `docs/web-ui.md` 里测的）。

**夹在 P3 与 P4 之间的一波，因为浏览器当时是瞎的。** §1.1 里 P1 没覆盖的四项最后是一件工作：harness 没法给模型看一张图，而 UI 没法给操作者看模型被提供了什么。

| 项目 | 实际错在哪 | 改了什么 |
| --- | --- | --- |
| `read_image` | 不存在，而它的阻塞点是传输：工具结果是一条文本消息 | `Message.parts`（`ContentPart::{Text, Image}`，`serde(skip)` 所以冻结契约的 JSON 不动）、OpenAI 数组形式编码器、`Result.parts`，以及每批图片一条后续用户消息 |
| 附件到达模型 | 记录了、可下载了，但从未被看见 | `crates/cli/src/attachments.rs` 为投影后的消息以及 `tool/result` 的 `meta.imagePath` 读 blob 存储 |
| 浏览器知道工具集 | 没有端点，也不读日志 | `web/src/state/capabilities.ts` 读最新 `llm/call` 的 `tools` 并分组，而 `GET /api/capabilities` 让面板也能列出"配置了但没提供"的那批 |
| `sandboxAccess.network` | `Spec.allow_net` 存在，但从来没被设过 | 认 `sandboxAccess.network`，在 Windows 上双向测过 |

*状态：已交付。97 个 CLI lib 测试（其中六个是新的桥接测试，它们找到了
一个真实缺陷：`expert` 工具注册时没有策略规则，于是模式兜底在三种模式里的两种里拒绝了它），74 个工具测试，14 个文件里 97 个 web 测试，`check.ps1` 全绿。端到端：第二次 provider 请求带着
`data:image/png;base64,…`，21 行的会话日志里没有 `iVBOR`，删掉文件后下一轮报告没有图片——这就是"从磁盘重读，不存进日志"在它为真时的含义。*

**仍然没做，而且点名而不是暗示：** 能力面板报告的是上一次请求的工具列表和"配置了但未使用"的集合；它不显示插件自己的工具 schema，因为那不在能力快照里，也没有路由暴露它：`hearth plugin start <name>` 列出名字，而 schema 只在插件协议上传输。暴露它们是一个关于 wire 格式的决策，不是一个缺失的字段。这一段过去列为"未展示"的两样东西现在各自有了读者：`GET /api/teams` 喂会话列表上的一个板子面板
（`web/src/components/TeamBoards.tsx`），而 `GET /api/presents`
喂它旁边的一个交接面板（`web/src/components/Presents.tsx`）。

## 6. 这个不买到什么

- **插件生态的价值。** 一个运行时不是市场：DSH 的优势是装好的插件，不是加载器。发出 P3 给我们的是"拥有插件的能力"，不是插件。
- **与 DSH 的功能对齐。** P1–P4 关掉的是*能力*清单，不是*界面*清单（xterm 终端、ACP agent 协议、Office 渲染、图像
  模型）。这张清单上的桌面壳后来按需造了——见
  [apps/desktop/README.md](../apps/desktop/README.md)——因为套在既有 UI 外面的一个 Tauri 窗口只需要 workspace 外一个 crate，而
  harness 里什么都不改。那个窗口里的编辑器曾经是只读的，现在能保存：一次保存走 `PUT /api/sessions/{id}/file`，带
  `expectedSha256`，追加一条 `file/write` 事件；会话只读、路径逃逸和摘要过期都在写之前拒绝。它刻意不是 `tool/call`——模型没有参与，策略和审批约束的是模型在无人看管时能做什么——而 `file/write` 这条记录正是让它可复核的东西。
- **一份许可证。** 已缓解：`Cargo.toml` 写着 `license = "MIT"`，`LICENSE`
  放着 MIT 文本。这个 crate 可以再分发；一个公开发布的包仍然需要关于名字的决策和一条版本策略。

## 7. 风险

| 风险 | 为什么是真的 | 缓解 |
| --- | --- | --- |
| 插件协议在任何人写第二个插件之前就僵化了 | MCP 这个形状是一个好的猜测，不是一次测出来的契合 | 在同一波里发一个非 Rust 的参照插件；把它的摩擦当成设计输入 |
| `read_image` 破坏 provider 契约 | `Message.content: String` 在 `contract.rs` 里是冻结的 | 可加的内容部分变体，把 `content` 保留为纯文本简写，每种形状一个 conformance 用例 |
| 新事件类型悄悄腐烂 | 读取器跳过它们不认识的东西 | 每个类型一个 conformance 用例，再加 `doctor` 一行统计活动数据目录里的未知类型 |
| 波浪工作跑得比门禁快 | 8 个新 crate 就是 8 份新 README 加 8 条 archcheck 边 | `archcheck` 让构建失败，所以漂移很响；永远不要把两个 crate 塞进一个波次项 |
| 力气落在没人用的功能上 | 缺口清单来自一份功能*列表*，不来自使用 | 先 P1（日用工具），然后让使用情况在 P2 与 P3 之间选 |
