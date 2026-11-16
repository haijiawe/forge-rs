---
dir: crates/subagent
layer: L2
role: M5 U3 subagent runner — serial delegation with fresh child logs and fail-closed children
public:
  - Options
  - Runner
  - CallContext
  - Canceller
  - Logger / NoopLogger
  - ChildRun
  - spec / tool_schema / DelegateArgs
  - TOOL_NAME / CODE_FAILED / CODE_DEPTH_EXCEEDED
may_import:
  - forge-core
  - forge-session
  - forge-llm
  - forge-tool
  - forge-agent
  - serde
  - serde_json
must_not:
  - tokio or any async runtime
  - the parent conversation transcript (children see only their prompt)
  - an approver for children (Decision::Ask must fail closed)
  - editing an event log after append
gate: cargo test -p forge-subagent --offline
---

## 做什么

把 Go 的 `internal/subagent` 原样搬到 Rust。对外只暴露一个 `subagent` 工具：模型给它一段**自包含**的任务描述，它开一个全新的 child session（新 id、新 jsonl），用同一套 provider 跑一轮 agent，然后把 child 日志里**最后一条非空 `assistant/message`** 作为工具结果返回。

关键行为：

- child 日志在 `<data_dir>/sessions/<childID>.jsonl`，`session/start` 携带 `parent`、`depth`、`startedBy:"subagent"`，seq 从 1 连续递增。child 的 id 是**新铸造的**，绝不复用父日志的事件 id（W1 的 fork 陷阱只属于 `forge-session::fork`）。
- 串行执行：容量 1 的信号量，并发调用排队；排队期间被取消会立刻醒来返回 `cancelled`，不会占用 slot，也不会创建日志。
- 深度：`depth >= max_depth`（`max_depth=0` 视为 1）时工具**仍然注册**，但每次调用都返回 `Code="depth_exceeded"`、`Meta{depth,maxDepth}`，让模型读到原因。
- 取消：父会话用 `Canceller::cancel_all`，单个 `callId` 用 `Canceller::cancel_call`。child 写 `turn/end{cancelled}` 和终态 `session/end`，工具返回 `Code="cancelled"`。
- 参数：`prompt` 必填，`system`/`model` 可选，未知字段一律 `bad_args`；任何参数错误都在写日志之前被拒绝。
- child 继承 policy/limits，但**没有 approver**：需要 `Decision::Ask` 的工具直接以 `no_approver` 失败，不会挂起等待人工批准。

## 公开面

```rust
pub struct Options { /* provider, data_dir, workspace, model, system, tools,
   max_steps, depth, max_depth, parent_session_id, clock, now, logger,
   policy, limits, canceller */ }
pub struct Runner;                       // Arc<Runner> 才能取 tool()
impl Runner {
    pub fn new(Options) -> forge_core::Result<Self>;
    pub fn tool(self: &Arc<Self>) -> Arc<dyn forge_tool::Tool>;
    pub fn run(&self, &CallContext, prompt, system_override, model_override)
        -> forge_core::Result<ChildRun>;
    pub fn max_depth(&self) -> usize;
}
pub struct CallContext { cancel, parent_session_id, workspace }
pub struct ChildRun { id, path, model, steps, turns, text, error, cancelled }
pub struct Canceller { /* ... */ }        // cancel_all / cancel_call
pub trait Logger { /* debug/info/warn/error，默认空实现 */ }
pub fn spec() -> forge_llm::ToolSpec;
pub fn tool_schema() -> serde_json::Value;
```

`Runner::run` 是本 crate 唯一超出工具面的公开入口，给需要直接驱动 child 的宿主使用。空字符串 override 表示“沿用 Options 的值”（对齐 Go 的 `TrimSpace` 语义）。准备阶段失败返回 `Err`；child 已经开跑之后的失败写进 `ChildRun::error`，终态 `session/end` 仍然落盘。

## 依赖与边界

只依赖 `forge-core`（事件、id、错误）、`forge-session`（日志文件）、`forge-llm`（provider 契约）、`forge-tool`（registry/executor/policy/limits）、`forge-agent`（一轮 agent 循环）、`serde`/`serde_json`（参数与 schema）。全部是 L2 或以下；不引入日志框架、不引入 chrono、不引入时间/临时文件依赖。

与 Go 的差异（因为 Rust 侧的冻结契约不同，逐条记录）：

1. `forge_tool::Tool::call(Call)` 没有 `context.Context`；`forge_agent::ToolRunner::run` 只返回 `Result<ToolResult>`。因此取消通过 `Options::canceller: Canceller` 传递，每次调用铸造一个 `Cancel` 并按 `callId` 注册；Go 的 `ctx` 参数在 Rust 侧不存在。
2. `forge_agent` 没有 `agent.ToolSet{Registry,Policy,Approver,Limits}` 结构，只有 `Option<Arc<dyn ToolRunner>>`。因此 Options 保留 `tools` + `policy` + `limits` 三个字段，并**完全没有 approver 字段**：child 的 Executor 一律 `approver: None`。
3. Go 的 `Now func() time.Time` 改为 `now: Arc<dyn Fn() -> i64>`（epoch 毫秒）+ 本地 UTC 日历换算；id 形状仍为 `YYYYMMDD-HHMMSS-<8 hex>`。`clock` 单独保留给事件时间戳。
4. Go 的 `*slog.Logger` 改为本地 `Logger` trait + `NoopLogger` 默认实现（依赖预算里没有 log/tracing）。
5. Go 的 `New` 会对负数 depth/max-depth 报错；Rust 用 `usize`，零表示使用默认值，无法表达负数。

## 不要做

- 不要在 child 里复用父会话的 transcript、事件 id 或工具 approver；child 只看得到自己的 prompt。
- 不要在深度到顶时注销 `subagent` 工具；模型必须能读到 `depth_exceeded` 和 `Meta`。
- 不要用 async runtime 或 sleep 实现超时/取消；等待信号量用 Condvar 通知，取消在流边界轮询。
- 不要在库代码里对 I/O/解析结果 `unwrap()`；测试可以。
- 不要编辑 `architecture.yaml`、根 `Cargo.toml` 或任何其它 crate；本 crate 的登记行由集成方添加：
  `forge-subagent: { layer: L2, ... }`。
