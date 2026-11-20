# DSH 0.2.2-rc.5 vs forge-rs：一次逐层对比

这份对比读的是源码，不是安装包的表面行为。

- 原版：`D:\魔改版Agent框架\deepseek-harness-master\`，根包 `@deepseek-ai/dsh-root` **0.2.2-rc.5**，MIT，Node ≥ 24.9.0，pnpm 11.28.5。目录里没有 `.git`，所以无法给出 commit。
- 重构版：`D:\魔改版Agent框架\forge-rs\`，HEAD `1409bd5`，24 个 Rust crate，MIT，rustc 1.96（MSRV 1.89）。
- 性能数字取自 `docs/dsh-comparison.md`，那次实测用的是本机安装的 **0.2.2-rc.1** npm 包，不是这份 rc.5 源码。版本差一个 rc，数字只作量级参考。

一个容易混淆的点：DSH 自己有个叫 **forge** 的子系统（`packages/experimental/forge*`），它是 GitHub/GitLab 的 PR/MR 集成，跟本项目 `forge-rs` 没有任何关系。下文说"forge"均指 `forge-rs`。

## 1. 一句话结论

DSH 是一个**可重配置的插件操作系统**，forge-rs 是一个**固定内核的确定性运行时**。

前者把"一切皆可替换"做到极致——连 agent loop 本身都是插件，代价是 155 万行 TypeScript、Node 启动链和 4.9 GB 的桌面进程组；后者把边界焊死在编译期——24 个 crate、9 条依赖预算、单二进制 8.8 MB、冷启动毫秒级，代价是扩展只能沿预留的几条缝（MCP、stdio 插件、名册、技能）进行，不能替换内核。

这不是"谁更完整"的问题，是两种工程赌注：DSH 赌的是生态与可塑性，forge 赌的是可控与可验证。

## 2. 规模

| 指标 | DSH rc.5 源码 | forge-rs |
| --- | ---: | ---: |
| TypeScript / TSX | 6,760 + 926 文件，**约 1,549,000 行** | — |
| Rust | 0 | 237 文件，源约 67,300 行 + 测试约 19,200 行 |
| Web 前端 | 62 个 `ui-*` 包 | 56 个文件，约 10,900 行 |
| Python | 84 文件，17,000 行（SDK + runtime） | — |
| workspace 包 | **393** 个 `package.json` | 24 个 crate + 2 个工具 + 2 个测试 crate |
| 根依赖 | 0 runtime / 91 dev（依赖下沉到子包） | 9 条外部依赖 + windows-sys |
| 发布形态 | Electron exe 246 MB + asar 209 MB；CLI 走 Node | 单 exe **8,767,488 B** |
| 启动 `--version` | 冷 1,470 ms / 热 160 ms（rc.1 安装包） | **约 5 ms** |
| 进程内存 | 桌面 10 进程合计 **4,914 MB** | `forge web` idle **6.3 MB / 6 线程** |

DSH 的 155 万行不是注水：393 个子包各自有 README、类型契约、测试和 i18n 文件（`README.md` / `README.zh.md` / `.i18n.yaml` 三件套）。把文件数除以包数，每个包平均不到 10 个源文件——这是刻意拆细的边界，不是巨石。

## 3. 架构范式：一切皆插件 vs 静态分层

这是所有差异的源头。

### DSH：没有特权核心

`docs/architecture.md` 开篇：

> Every part of the product is a plugin, including the model adapter, the tool registry, the session log, and the agent loop itself, so each is replaceable from configuration. There is no privileged core to patch.

运行时的组合方式是 **profile → bundle → patch**：

- **profile** 是命名组合：`web` / `headless` / `sdk` / `sdk-minimal` / `acp`，外加桌面专用的 `desktop`。
- **bundle** 是可分发的一组 Cordis 配置行 + 代码。`dsh-base` 是四个 profile 共享的第一层（模型适配器、工具、持久化、沙箱与审批策略、设置、凭证、遥测），`dsh-web-app` 加浏览器应用，`dsh-headless` 加一次性 runner，`dsh-acp-app` 加自动化服务器。
- **patch** 按 id 定位某一行并**整行替换**配置，或插入新行。叠加顺序：profile 的 bundle → profile 的 `cordis.patch.yml` → home patch → `--patch` 覆盖。最后写入者胜。
- `dsh --profile web --dump-config` 能打印这台机器实际启动的整棵树；打印出来的**每一行**都能被你自己的 patch 换掉。
- HMR：base profile 默认开启配置热重载，headless/SDK/ACP 关闭，`sdk-minimal` 完全不带。

`base` bundle 有 **101 个插件行**，`web-app` 有 **102 行**。这不是"插件_support"，这是产品本身由 100 多个可替换部件拼起来。

### forge-rs：边界在编译期

`architecture.yaml` 把 24 个 crate 分成 L0–L3：

| 层 | crate |
| --- | --- |
| L0 | core, config |
| L1 | exec, sandbox, mcp, skill, net |
| L2 | session, llm, tool, protocol, agent, store, import, subagent, context, experts, market, plugin, team, goal, schedule |
| L3 | server, cli |

`archcheck` 禁止向上依赖和同层依赖，CI 里跑，违者退出 1。每个 crate `#![forbid(unsafe_code)]`（sandbox 例外），依赖预算 9 条 + windows-sys，不引 tokio、不引 async。

结果：任何一次改动的影响面都能从 `cargo check` 和 archcheck 推导出来；代价是"想换掉 agent loop"在 forge 里等于重写，在 DSH 里等于写个插件。

### 对比表

| | DSH | forge-rs |
| --- | --- | --- |
| 组织单元 | Cordis 插件（运行时注册） | Rust crate（编译期链接） |
| 内核可替换 | 是，loop/session/tools 都是服务 | 否，内核是代码 |
| 扩展方式 | `ctx.*` 上注册服务/监听事件/patch 配置 | 实现 trait + 在 `wiring.rs` 接线 |
| 配置叠加 | 四层 patch，最后写入者胜 | 单一 `config.json`，缺字段回落默认值 |
| 热重载 | 支持（HMR） | 不支持，改配置要重启 |
| 边界检查 | 运行时服务依赖 + 文档约定 | `archcheck` 编译后静态检查 |
| 扩展上限 | 理论上无（任何部分可换） | 预留缝：MCP、stdio 插件、名册、技能、hook |

## 4. 扩展机制：service seam vs trait

DSH 的核心抽象叫 **capability seam**，一个能力由三个角色组成：Service Definition（声明接口）、Service Provider（实现）、Consumer（通常是模型可见的工具）。一个包可以同时扮演多个角色，但只有一个角色不算 seam。

这套抽象带来的实际效果：

- **文件系统和子进程共享同一个执行世界**。把 `ctx.fs` provider 指向远程沙箱，Bash、PTY、LSP 一起跟着走，不需要为每个工具做分叉。
- **subagent provider 差异极大但接口统一**：从"新建一个子 agent"到"把一轮委派给另一个产品"，都走 `startActivation`。
- **每个能力一个 `ctx.*` 键**：`ctx.llm`、`ctx.tools`、`ctx.sessions`、`ctx.agents`、`ctx.agentLoop`、`ctx.sandbox`、`ctx.shell`、`ctx.terminals`、`ctx.jobs`、`ctx.goals`、`ctx.workflowEngine`、`ctx.compaction`、`ctx.sessionProjections`、`ctx.webhookRuntime`、`ctx.forge`……架构文档有一张"新行为放哪里"的表，逐项指明注册到哪个键。

forge-rs 对应的是 Rust trait：`Tool`、`Provider`、`Runner`、`Sandbox`、`Policy`、`Enforcer`、`AttachmentHook`、`HiddenHook`。这些 trait 是静态分发的，靠 `Arc<dyn Trait>` 做少量动态。`crates/cli/src/wiring.rs` 是唯一的接线处——好处是所有能力在一个文件里可见，坏处是加一条缝要改代码、重编译、重新发版。

**插件的含义也不同**：

- DSH 插件 = 进程内的 Cordis 插件，可以贡献服务、事件、效果，卸载时效果回滚；可以换掉模型适配器。
- forge 插件 = 进程外 stdio JSON-RPC 子进程，只能贡献工具（`<plugin>.<tool>`）、prompt、hook；协议是固定的，不能换内核。它复用 `forge-mcp` 的 L1 传输，符合"插件语言由作者决定，宿主只讲协议"。

实测 stdio JSON-RPC 往返（Node 父↔Node 子 2,000 次）：空 result **80.6 µs**、4 KB **118.3 µs**、64 KB **343.7 µs**。对照一次模型往返 1,203 ms，一次插件调用占 **0.007%**——这就是 forge 敢把插件放进程外的依据。

## 5. 事件与持久化

两边都把"事件日志是唯一真相"当作第一原则，但复杂度和承诺不同。

### DSH

三类事件：

| 类别 | 例子 | 用途 |
| --- | --- | --- |
| session 持久事件 | `turn/*`、`user/message`、`assistant/message`、`tool/*`、`compaction/*` | 必须跨重启存在的事实 |
| agent 活事件 | `agent/created`、`agent/pre-step`、`agent/request`、`agent/turn-stopping` | 观察或**拦截**进行中的工作 |
| capability 事件 | `fs/*`、`tools/*`、`telemetry/*` | 把策略和适配器接到缝上，不 import loop |

其中 `agent/pre-step`、`agent/request` 和三个 `tools/*` 是 **waterfall**：监听器必须调用 `next()` 才能委托给下一个。这意味着插件可以接受、改写或拒绝一次输入，可以拦截工具执行前后——forge 的只读观测模型完全没有这个层级。

`assistant/message` 内嵌产生它的**完整计时流**；失败的、重试的、取消的、流错误的尝试记成 `assistant/attempt`，不进模型历史。硬崩溃发生在结算前就不留持久尝试流。

会话格式有**版本迁移链**：v0 → v1 → v2 → v3 → v4，各有独立迁移包（`session-format-v0-to-v1` … `v3-to-v4`）。存储支持 JSONL 和 SQLite，v0 用 `session.jsonl[.zstd]`，v1+ 用 `session.vN.jsonl[.zstd]`；header 只读的 `stat`/`list` 可以扫描目录而不加载事件；写打开会编码、校验、然后**独占发布**一个新的版本命名文件，旧文件永不改名、替换或删除。

### forge-rs

事件类型冻结在 `crates/core/src/event.rs`，当前 19 种：

```
session/start, session/end, turn/start, turn/end, step/start, step/end,
user/message, assistant/message, tool/call, tool/result, llm/call,
error, compaction-ish…, goal/created, goal/continued, goal/ended,
context/compressed, …
```

三法则：**日志只追加**；**未知类型是数据、未知版本是错误**；该 fail closed 就 fail closed 并说明没做什么。新事件类型必须进 `EventType::ALL`、配一条 conformance case、写进 `events-v1.md`，不复用旧名字。

投影在 `crates/session/src/projection.rs`；`Store::read_from` 逐行读，撕裂尾在 store 层按路径 truncate。fork 时逐字段复制，**只改 session 和 seq 两个信封字段**（契约 `events-v1.md:232-239`），id/ts/type/payload 原样保留——这一点早期实现曾做错（给每条记录重新铸 id），是按契约修回来的。

没有会话格式版本迁移：遇到未知版本直接拒绝。这是"未知版本是错误"的直接后果——好处是不会静默误读，坏处是格式演进必须停服、必须双读。

### 对比

| | DSH | forge-rs |
| --- | --- | --- |
| 活事件拦截 | waterfall，可改写/拒绝 | 无，只有只读投影 |
| 失败尝试 | `assistant/attempt` 独立记录 | `error` 事件 + tool/result 的 is_error |
| 会话格式 | v0–v4 迁移链 + zstd + 独占发布 | 单一 v1，未知版本拒绝 |
| 存储后端 | JSONL / SQLite 可换 | SQLite（rusqlite bundled）|
| 分叉 | 存储父前缀 + 子事件，按生命周期发布 | 逐字段复制，只改 session/seq |
| 模型可见性 | "Model-visible means logged" | 同等承诺，log 是唯一真相 |
| 契约测试 | 类型等价块 drift-check | 23 个事件 conformance 用例 + 34 HTTP + 8 WS |

## 6. 上下文与压缩

这是两边理念分歧最有意思的一处。

**DSH**：压缩是**可选能力**（capability seam），不是 agent loop 的脊椎。三个事件 `compaction/start` / `compaction/summary` / `compaction/end` 构成一把**日志记录的锁**——start 最先写，end 最后写，所以中途崩溃留下的是"孤立的 start"（可检测），而不是"谎称完成的 end"。摘要本身不走事件进模型，而是挂在一条 `user/message` 上，带 `surfaceOp: { op: 'replace', startSeq, endSeq }`——这是 surface 唯一的变更操作。此外还有 `compaction-image-offload`（图片卸载）和 `compaction-tool-result-pruner`（工具结果裁剪）两个兄弟 provider。token 数、模型、raw 输出、usage 全部记进 summary 事件，保证"一次请求可从日志+代码重建"。

**forge-rs**：`forge-context`（L2）用一条 `context/compressed` 事件寻址 **seq 闭区间**，payload 为 `{blockId, from, to, summary, messages?}`。不重叠、id 唯一且是 32 hex、区间必须已被日志闭合。三个工具 `compress` / `decompress` / `search_context` 放在 CLI 层（因为 archcheck 禁同层边），通过 `HiddenHook` 接进 agent；`to_llm_messages_hiding` 做**两遍配对闭合**——隐藏一次工具调用必须带走它的结果，反之亦然，否则会发出"有 tool 消息没 call"的请求被 provider 拒。取回是读，不是改写，日志永不缩小。

| | DSH compaction | forge forge-context |
| --- | --- | --- |
| 事件 | 3 个（start/summary/end）+ surface replace | 1 个（context/compressed） |
| 并发安全 | 日志锁，崩溃可检测 | 工具侧拒绝重叠 + 区间闭合检查 |
| 模型侧 | `user/message` + `surfaceOp.replace` | 隐藏 hook，历史里直接少发 |
| 日志 | 摘要 + token + 模型 + raw 输出全记 | 只有摘要 + seq 区间 |
| 取回 | 通过 surface 投影 | `decompress` 工具逐行 canonical 还原 |
| 搜索 | 不在 compaction 内，另有 session-query | `search_context` 词频 AND 排序 |
| 兄弟能力 | image offload、tool result pruner | 无（图片走 ContentPart，不卸载） |

一个实测细节值得记：forge 的第二轮请求里旧消息确实不再作为 user 消息出现，但**全文 grep 仍能看到旧文本**——那是摘要正文里的引用。判断"压缩生效"必须看 message role，不能 grep 全文。

## 7. 能力清单

### DSH 有、forge 完全没有的

| 类别 | DSH 包 / 子系统 | 说明 |
| --- | --- | --- |
| ACP | `packages/bundle/acp-app`、`packages/acp` | 自动化专用 ACP 服务器，独立 profile |
| 持久终端 | `packages/terminal/*`（2 个）、`tool-terminal` | PTY 会话、按精确 owning Agent 授权、wait 原因区分 silence/timeout/session_exit |
| 后台作业 | `packages/jobs/*`（3 个）、`tool-jobs` | `job_*` 工具读/停后台任务 |
| Web 自动化 | `packages/browser-use`、`packages/computer-use` | 浏览器与桌面控制 |
| LSP | `packages/lsp/*`（3 个）、`tool-lsp` | 语言服务，跟文件系统共享执行世界 |
| SSH | `packages/ssh/*`（5 个） | 远程连接 |
| Workflow | `packages/workflow/*`（3 个） | 模型编写编排脚本，通过共享 Node PTC 进程运行 |
| Webhook | `docs/subsystems/webhook.md` | 外部事件启动 Session |
| 文档办公 | `packages/document/*`、`office-kit` | Office 渲染与工具 |
| 遥测 | `packages/telemetry`、`session-telemetry-otel` | OpenTelemetry |
| 账户/凭证 | `packages/account/*`、`credentials/*`（5 个） | 登录、授权、凭证存储 |
| Liveshare / 语音 | `liveshare`、`voice-input` | 协作与语音输入 |
| 翻译 / 反馈 | `translation`、`feedback` | i18n 与用户反馈 |
| 会话查询 | `packages/session-query/*`（4 个） | 对日志的结构化查询 |
| 插件市场 UI | `ui-plugin-manager`、`ui-plugin-market` | 浏览器里管理插件 |
| 会话格式迁移 | v0→v4 五个包 | 见上 |
| Code-host forge | `experimental/forge*` | GitHub/GitLab PR/MR 集成（与本项目无关）|

### 两边都有的（实现形态不同）

| 能力 | DSH | forge-rs |
| --- | --- | --- |
| 模型适配 | `ctx.llm` + 多 adapter，运行时可换 | `forge-llm` 内 openai 兼容，静态 |
| 工具 | 40+ 工具包，schema DSL + 输出契约 + UI presenter | 10 个无条件 + 3 个网络/上下文 + 插件/MCP/专家/团队等条件工具 |
| 沙箱 | `sandbox` / `sandbox-local` / `sandbox-policy` / `sandbox-windows-acl` | `forge-sandbox`：Windows job object + AppContainer、Linux Landlock、macOS sandbox-exec |
| MCP | `packages/mcp/*`（4 个） | `forge-mcp`，stdio + HTTP |
| 技能 | `packages/skill/*`（5 个）、`ui-skill` | `forge-skill` + `forge skill` 命令 + Office skill |
| 子代理 | `packages/subagent/*`（9 个）、4 种后端 | `forge-subagent` + `forge-team`，单一本地 runner |
| 团队 | `team/agent-team` + 共享任务板 + `ui-agent-team` | `forge-team` + 共享板 + `GET /api/teams` + 浏览器面板 |
| 目标 | `goal/goal` + event-sourced + CAS revision | `forge-goal`，事件+续跑循环 |
| 定时 | `schedule/*`（2 个） | `forge-schedule`，rrule 子集 |
| 压缩 | 4 个 compaction 包 | `forge-context` 单 crate + 3 工具 |
| 附件 | `packages/attachment/*`（2 个）、`ui-attachment` | 存 `blobs/` + 引用进日志 + 模型可见 + 浏览器上传 |
| present | `deliverables/tool-present` + `ui-deliverables` + git 工作树差异 | `present` 工具 + `GET /api/presents` + 浏览器面板 |
| worktree | `experimental/tool-worktree` + worktree service | `forge worktree` 工具，经 `forge-exec` |
| Web UI | 62 个 `ui-*` 包 | 少量组件：transcript、能力面板、团队板、goal 药丸、交接面板、附件 |
| 桌面壳 | Electron 应用 + Desktop Host | 无，只有 CLI + 本地 Web |

### forge 工具清单（当前二进制 14 个）

`fs_read` / `fs_write` / `fs_edit` / `fs_glob` / `fs_grep` / `shell` / `todo_write` / `worktree` / `present` / `read_image` 是无条件的；`web_fetch`（+ 配置后的 `web_search`）、`compress` / `decompress` / `search_context`（有会话日志时）、`expert`（有名册时）、`team`（有 team 且 maxParallel>0）、`skill`、`subagent`（maxDepth>0）、`goal_end`（只在 goal 循环内）、以及每个 MCP / 插件工具。

DSH 的本会话可见原生工具超过 60 个（bash / pwsh / persistent 版本、LSP、terminal、jobs、browser、computer、office、workflow、forge、session-query、subagent 控制、agent-team、webhook 等）。

## 8. 沙箱与安全

| | DSH | forge-rs |
| --- | --- | --- |
| 抽象 | `ctx.sandbox` capability seam，backend 可换 | `Enforcer` trait，按 OS 编译期选择 |
| Windows | 4 个包含 `sandbox-windows-acl` | job object + AppContainer（手写 Win32 FFI，不用 windows-sys） |
| Linux | sandbox-local 等 | Landlock ABI，abi≥4 时可断网 |
| macOS | sandbox-local | `sandbox-exec` profile，CI `sandbox macos-latest` job 实测五个用例 |
| 网络开关 | 按 backend 能力 | `sandboxAccess.network` 贯通三平台；Windows 无 internetClient 时连 loopback 都拒 |
| 读授权 | 按 backend | `sandboxAccess.read/.write`；祖先 traverse 逐组件授权；已授权就跳过（DACL 重算代价分钟级）|
| 完整性级别 | 未见处理 | 检测 Low mandatory integrity 并给出 `icacls` 修复提示（这是踩过的真坑）|
| 文档立场 | SAFETY.md 明说 experimental、未审计、不可当安全边界 | `m6-surface.md` 逐平台写清实测/仅编译级，不声称未验证的事 |

两边都强调"沙箱不是安全边界"。DSH 的 SAFETY.md 措辞更重：developer preview、未经过安全审计、不得视为 secure 或 production-ready，建议在一次性 VM / 容器 / 专用环境里跑。

forge 的独特之处在于**把限制的成本摆在明面上**：每个 workspace 一个永久 AppContainer profile（这台机器曾积累到 87 个，profile 服务会成段时间拒绝新名字并伪装成 `ERROR_ACCESS_DENIED`）、容器读不到 `%USERPROFILE%`（需要显式 extra_read）、共享身份不保证两个 workspace 隔离。这些都被写进契约文档而不是藏在实现里。

## 9. 性能

同一台机器、同一个自定义端点、同一个模型（`deepseek-v4.1-flash-1017`）、同一个 prompt，三次运行的中位数（rc.1 安装包 vs forge-rs）：

| | DSH rc.1 | forge-rs |
| --- | ---: | ---: |
| 完整一次模型往返 | **5,724 ms** | **1,203 ms** |
| 最好一次 | 5,491 ms | 1,044 ms |
| `--version` 冷 | 1,470 ms | ~5 ms |
| `--version` 热 | 160 ms | ~5 ms |
| idle 内存 | 桌面进程组合计 4,914 MB | `forge web` 6.3 MB |

注意这是**量级对比，不是公平基准**：DSH 那 5.7 秒包含 Node 启动、profile 组装和插件激活；forge 的 1.2 秒是原生 CLI 直连同端点。DSH 的 4.9 GB 是十个进程（主进程 1,558 + 渲染 825 + 本会话 node agent 1,510 + utility/GPU）之和，与单进程 RSS 不可直接比。

forge 自己的六项 bench（`bench -strict` 三平台全绿）记录：`fsync` 4,839/s、`batch` 87,697/s、`replay` 248,997/s、`projection` 82,186/s、`roundtrip` 6,183 ns/op（这一组是本机 Windows/amd64 基线，linux 与 darwin 另有记录）。

## 10. 交付与可移植性

| | DSH | forge-rs |
| --- | --- | --- |
| 安装 | npm / Electron 安装包 / Python wheel | 单二进制 + `scripts/install.ps1` / `.sh` |
| 跨平台 | Electron 三平台 + Node 任意平台 | 六平台交叉编译产物（release workflow），CI 用 native runner 出 artifact |
| 前端更新 | HMR 热重载（部分 profile） | 改 `web/src` 必须 `vite build` → `cargo build --release` → 手工复制 `bin/forge.exe`（它不是 cargo 产物）|
| 版本演进 | rc 阶段明确声明会有破坏性变更 | `0.0.0-dev`，契约冻结在 `docs/contracts/` + 语料 |
| 可审查性 | 运行时组合需 `--dump-config` 才可见 | 源码即结构，archcheck 强制层图 |

## 11. 该怎么选

**选 DSH**：需要替换内核某部分（换模型适配器、换会话存储、拦截每一步 agent 行为）、需要 PTY/LSP/SSH/浏览器自动化/ACP、需要插件市场和 HMR、愿意为可塑性付启动时间和内存。

**选 forge-rs**：需要单文件分发、毫秒启动、可预测的内存、编译期可验证的边界、契约驱动的行为一致性（同一份 conformance 语料锁住事件与 HTTP 形状）、或者要把 harness 嵌进别的系统里当库用。

forge 刻意不买的东西写在 `docs/09-platform-tier.md` §6：桌面壳、xterm 终端、ACP agent 协议、Office 渲染、图片生成模型——这些是 DSH 的**表面**，不是能力。纯 Rust docx/xlsx 写入器也刻意延后，只有"必须单文件安装"时才值得写；Office 目前是 skill + 程序探测（本机 `docx=python 3.14 xlsx=python 3.14 pptx=python 3.14 convert=missing`）。

## 12. 证据索引

- DSH：`deepseek-harness-master/docs/architecture.md`、`docs/subsystems/*.md`（60+ 主题中英双语）、`packages/bundle/base/cordis.patch.yml`（101 行插件）、`package.json`（rc.5 / 393 包）、`SAFETY.md`。
- forge：`docs/09-platform-tier.md`（ADR-0003，P1–P5）、`docs/08-rust-port.md`（ADR-0002，24 crate 表）、`docs/contracts/`（7 份冻结契约）、`docs/dsh-comparison.md`（rc.1 实测）、`docs/bench.md`（三平台基线）、`docs/ci.md`（22 次 run、11 个真实缺陷）。
- 所有 forge 侧数字都来自本会话的磁盘实测与 CI 记录；DSH 侧数字来自源码统计，性能数字来自 rc.1 安装包实测，已在开头标注版本差。