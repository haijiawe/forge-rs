---
dir: crates/agent
layer: L2
role: one-turn agent loop and transcript summarizer; owns event ordering and the consumer-owned tool hook
public: [hearth-agent]
may_import: [hearth-core, hearth-llm, hearth-session]
must_not: [hearth-tool, hearth-server, hearth-cli]
gate: cargo run -q -p archcheck --offline
---

## 做什么

从已退役的 Go 树移植过来的单轮 agent 循环与转录摘要器。`Agent::run` 驱动一个 user 轮次：它通过 hearth-session 重新投影那份只追加日志（不变量 I1），按冻结的 events-v1 顺序发事件（信封里的 step 从 1 开始，payload 里的 index 从 0 开始），只在第一个文本片段到达之前重试 provider 失败，取消时恰好关闭一次流，同时仍然写下 `turn/end`，并把工具调用结果送回模型。`ProviderSummarizer` 把一段有序事件折叠成消息并对其摘要——不用工具、不重试、不回退，且剥掉思考标记。

## 公开面

`Agent`、`Options`、`RunResult`、`Cancel`，`ToolRunner` 与 `EventLog` 两个 trait 和 `ToolResult` 形状（这就是那个刻意收窄、由消费方提供的工具钩子，如今由 `hearth-tool` 实现），payload 常量与 stop/reason 助手，`history` 助手，`err` 元数据助手，两个扩展面——`Hooks`/`Hook`/`HookCall`/`HookOutcome`/`HOOK_PHASES`（改写接缝，见 `docs/11-adr-agent-hooks.md`）与 `Dispatch`/`EventView`/`Listener`/`Problem`（纯观察：每追加一行就 `emit`、在轮次边界 `serial`），常驻面——`Agent::pending`/`enqueue`/`kick`/`when_idle`/`run_maintenance`/`phase` 建在 `inbox` 里的持久队列上（`Inbox`/`Msg`/`Target`，其行是 `agent/inbox`，其维护窗口是 `agent/status`）——以及 `ProviderSummarizer` / `Summary` / 摘要器的错误常量。

## 依赖与边界

只依赖 hearth-core（事件与错误）、hearth-llm（冻结的 provider 契约）、hearth-session（store 与投影）和 serde_json。绝不导入 hearth-tool 或任何更高层；工具由 `ToolRunner` 提供。历史永远从日志重建——本 crate 不保存会话真相的第二份副本。没有 async 运行时，没有新依赖，没有 unsafe。

## 不要做

- 不要在内存里保留队列：一个已经被接受的 prompt 必须活过进程，所以它要进日志。
- 不要为普通轮次写 `agent/status`：每轮两行诊断没有任何读者。
- 已设置 `PromptRecorded` 时不要再写一遍 prompt。
- 不要在文本片段之后重试。
- 不要为被取消的轮次发 `error` 事件。
- 不要把工具调用或推理内容送给摘要器所用的模型。
- 不要退回到编造的摘要文本。
- 不要允许观察者反驳改写者：listener 绝不能改变循环发出的内容——那是 `Hooks` 的事，而且它有记录。
- 不要加 `hearth-tool` 依赖，也不要改 `Cargo.toml` / `architecture.yaml`。