# crates/ — 二十四个 crate，一个边界一个

每加一个 crate 就是承诺一个别人不能越过的缝。这里不是「按功能分文件夹」，而是**按依赖方向分层**：
低层不知道高层存在，`tools/archcheck` 会把指向上层的 `Cargo.toml` 依赖变成构建失败。

如果你第一次进这个仓库，只想知道「我要改的东西在哪」，跳到下面的
[我想做 X，去哪](#我想做-x去哪)；那张表比按名字猜目录快得多。

## 分层

| 层 | 含义 | 能不能依赖 | 里面有哪些 |
| --- | --- | --- | --- |
| **L0** | 词汇与常量：事件信封、id 规则、规范 JSON、错误种类、配置 | 只能依赖标准库和外部 crate | `core`、`config` |
| **L1** | 与外界打交道的**机制**：起进程、关进程、连网络、说 MCP、读技能、读名册 | 可以依赖 L0 | `exec`、`sandbox`、`mcp`、`skill`、`net`、`experts` |
| **L2** | 会话语义：日志、投影、模型、工具、编排 | 可以依赖 L0、L1 | `session`、`llm`、`tool`、`protocol`、`agent`、`store`、`import`、`subagent`、`goal`、`schedule`、`market`、`plugin`、`team`、`context` |
| **L3** | 产品面：HTTP/WS 服务与 `hearth` 可执行文件 | 可以依赖任何层 | `server`、`cli` |

分层图写在根目录 `architecture.yaml` 里，不是在某个人的脑子里；`crate.undeclared`（用了没声明的 crate）、
`edge.upward`（依赖指向上层）、`edge.forbidden`（明确禁止的边，例如 `core` 不许依赖 `config`）都会让
`cargo run -p archcheck` 以退出码 1 结束。

## 里面有什么

每个 crate 的说明书在自己的目录里，写着七键 frontmatter（`dir`/`layer`/`role`/`public`/`may_import`/
`must_not`/`gate`）和四节中文正文。**frontmatter 是机器读的那一半，四节是人和 agent 读的那一半**，
缺任何一半 `archcheck` 都会报错。

| 目录 | 层 | 一句话职责 |
| --- | --- | --- |
| [`core/`](core/README.md) | L0 | 冻结的词汇：事件信封、id、规范 JSON、摘要、错误种类 |
| [`config/`](config/README.md) | L0 | `config.json`、数据目录、默认值、MCP 条目告警 |
| [`exec/`](exec/README.md) | L1 | 起进程、stdio、超时、杀进程树、沙箱挂载点 |
| [`sandbox/`](sandbox/README.md) | L1 | 真遏制：Windows job object + AppContainer、Landlock、`sandbox-exec` |
| [`mcp/`](mcp/README.md) | L1 | MCP 客户端，stdio 与 Streamable HTTP |
| [`skill/`](skill/README.md) | L1 | 扫描技能目录，被要之前不读正文 |
| [`net/`](net/README.md) | L1 | 阻塞式 HTTP(S)、重定向、HTML 转文本 |
| [`experts/`](experts/README.md) | L1 | 从 Markdown frontmatter 读专家与团队名册 |
| [`session/`](session/README.md) | L2 | 只追加日志、投影、压缩、fork、附件块 |
| [`llm/`](llm/README.md) | L2 | provider 契约与 OpenAI 兼容 SSE 客户端 |
| [`tool/`](tool/README.md) | L2 | 工具注册表、执行器、策略、审批、内置工具 |
| [`protocol/`](protocol/README.md) | L2 | 服务端与界面共享的 wire 类型 |
| [`agent/`](agent/README.md) | L2 | 一轮对话的循环、事件顺序、摘要、两个改写缝 |
| [`store/`](store/README.md) | L2 | 可重建索引：SQLite 行、FTS5 全文 |
| [`import/`](import/README.md) | L2 | 只读导入旧会话树 |
| [`subagent/`](subagent/README.md) | L2 | 有界池委派，子会话各有自己的日志 |
| [`goal/`](goal/README.md) | L2 | 从日志折出目标状态，决定要不要续跑 |
| [`schedule/`](schedule/README.md) | L2 | 重复规则、任务登记、下次何时到点 |
| [`market/`](market/README.md) | L2 | 市场索引、校验摘要再安装、被改过就拒绝删除 |
| [`plugin/`](plugin/README.md) | L2 | 插件宿主：子进程讲 stdio JSON-RPC |
| [`team/`](team/README.md) | L2 | 共享工作板 + 每 worker 一个信箱的并行 |
| [`context/`](context/README.md) | L2 | 可命名、可还原、可搜索的压缩块 |
| [`server/`](server/README.md) | L3 | 日志之上的 HTTP/1.1 与 WebSocket |
| [`cli/`](cli/README.md) | L3 | `hearth` 这个可执行文件 |

## 我想做 X，去哪

| 想干的事 | 改这里 | 顺手要动的 |
| --- | --- | --- |
| 加一个模型能调用的工具 | `tool/src/builtin/` 新文件 + `builtin/mod.rs` 注册 | `tool/src/policy.rs` 给它一个三档决策，否则它会被静默拒绝 |
| 加一个 HTTP 路由 | `server/src/<名字>_route.rs` | `server/src/server.rs` 的 `handle_api` 加一个分支 + `docs/contracts/api.md` |
| 加一种事件类型 | `core/src/event.rs` 的 `EventType::ALL` | 契约表格 + `tests/conformance/cases/events/` 一个用例；三处缺一不可 |
| 加一种 wire 形状 | `protocol/src/protocol.rs` | `web/src/api/protocol.ts`（没有生成器，漂移是静默的） |
| 换一个沙箱后端 | `sandbox/src/` | `docs/contracts/m6-surface.md` 的平台表；做不到就写 `false`，不要声称做到了 |
| 改系统的说话方式 | `cli/src/lib.rs` 的 `USAGE` | `docs/contracts/cli.md`；`cli/tests/cli.rs` 逐字比对 usage |
| 改界面 | `web/src/`（`components/` 放屏幕，`state/` 放 reducer） | `web/README.md`；没有 npm 依赖预算，加库要先问 |
| 加一个平台集成 | 新建一个 L1 或 L2 crate | `architecture.yaml` 分层 + 七键 frontmatter + 四个中文小节 |
| 让某个能力可开关 | `config/` 一个段 | `cli/src/wiring.rs` 接线；**默认必须是关** |

## 新 crate 的规矩

四件事缺一不可，缺了 `archcheck` 会红：

1. `crates/<名字>/Cargo.toml`，把依赖写进根 `Cargo.toml` 的 `[workspace.dependencies]`，不要写版本号。
2. `crates/<名字>/README.md`，七键 frontmatter + `## 做什么` / `## 公开面` / `## 依赖与边界` / `## 不要做`。
3. `architecture.yaml` 里把 crate 放进恰好一层。
4. `crate.duplicate` 与 `crate.undeclared` 都过了：一个 crate 只能在一层，用了没声明的依赖也要红。

依赖预算写在根 `Cargo.toml`，每条都回答「为什么标准库做不到」。加依赖前先问，
这是 `AGENTS.md` 的六条反模式之一。

## 相关

- [AGENTS.md](../AGENTS.md) — 三法则、六反模式、动手前要写的任务卡
- [architecture.yaml](../architecture.yaml) — 分层图的机器可读版本
- [docs/contracts/](../docs/contracts/README.md) — 七份冻结契约，改任何对外行为前必读