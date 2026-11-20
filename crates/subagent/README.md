---
dir: crates/subagent
layer: L2
role: M5 U3 subagent runner — bounded-pool delegation with fresh child logs, durable identity, and fail-closed children
public:
  - Options
  - Runner
  - CallContext
  - Canceller
  - Logger / NoopLogger
  - ChildRun
  - DEFAULT_MAX_ACTIVE / DEFAULT_MAX_DEPTH / DEFAULT_MAX_STEPS
  - spec / tool_schema / DelegateArgs
  - TOOL_NAME / CODE_FAILED / CODE_DEPTH_EXCEEDED / CODE_BAD_OUTPUT
  - structured_spec / STRUCTURED_OUTPUT_TOOL / CODE_TERMINATED
  - CatalogEntry / CATALOG_META / SUBAGENT_CATALOG_VERSION
  - Descriptor / Mode / ToolFilter / SUBAGENT_DESCRIPTOR_VERSION
  - is_agent_name / join_agent_path / parent_agent_path / NameError / ROOT_AGENT_NAME
  - is_child_id / child_log_path / format_stamp
  - Backend / ProcessBackend / BackendRequest / BackendOutcome / BACKEND_PROTOCOL_VERSION / DEFAULT_BACKEND_TIMEOUT
  - modules catalog / descriptor / list / names / backend / cancel_file (fold + render + spawn + cross-process interrupt markers)
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
- 并发：一个计数信号量，容量由 `Options::max_active` 决定（0 取默认 **1**，即 Go oracle 钉住的串行行为；对齐 DSH 的 8 就把它设大）。同时最多 N 个 child 在跑，其余排队；排队期间被取消会立刻醒来返回 `cancelled`，不占用 slot，也不创建日志。**与 DSH 的差别**：DSH 在超限时抛 `ACTIVATION_LIMIT_REACHED`，这里改为排队——本工具是同步的，模型已经要求了这份工作，排队比拒绝更诚实。
- 深度：`depth >= max_depth`（`max_depth=0` 视为 1）时工具**仍然注册**，但每次调用都返回 `Code="depth_exceeded"`、`Meta{depth,maxDepth}`，让模型读到原因。
- 取消：父会话用 `Canceller::cancel_all`，单个 `callId` 用 `Canceller::cancel_call`。child 写 `turn/end{cancelled}` 和终态 `session/end`，工具返回 `Code="cancelled"`。
- 参数：`prompt` 必填，`system`/`model`/`schema`/`continuable`/`label`/`resume` 可选，未知字段一律 `bad_args`；任何参数错误都在写日志之前被拒绝。`resume:<childId>` 让这次调用去续跑那个 continuable child（见下），此时 `schema` 不可再设（创建时已定）。
- 结构化结果：`schema`（或 `Options::structured_schema`）存在时，child 注册 `structured_output` 终结工具，必须靠调用它交回结果；调用即视为完成，其后任何工具调用都由“已终结”守卫拒绝（`Code="already_terminated"`）。父侧从 child 日志的 `tool/call.args` 取回该对象放进 `ChildRun::structured`；若 child 结束时从未调用 `structured_output`，工具结果以 `Code="bad_output"` 失败（不是成功）。
- child 继承 policy/limits，但**没有 approver**：需要 `Decision::Ask` 的工具直接以 `no_approver` 失败，不会挂起等待人工批准。
- **持久身份（S1）**：child 起跑时，它自己的日志在 `session/start` 之后写一条 `subagent/descriptor`（`version:3`，`mode:"one-shot"`，`provider:"local"`），log-only、永不进模型历史、survive compaction。父日志一侧的 `subagent/catalog` **由父会话的 agent loop 写，不由本 crate 写**：`Runner` 只是从 `Options::parent_log`（只读）里 fold 已用的同名、用 `names::reserve` 分配一个 `agent-N`、把 `CatalogEntry` 放进 `ChildRun::catalog`，`tool.rs` 再把它塞进 `ToolResult.meta["subagentCatalog"]`，loop 看到这个 meta 才追加事件（同 `contextBlock` 的通道）。没有父日志（例如 team worker）就没有名字、没有 catalog 行——它的 transcript 属于 board，不属于某个 session。
- **冷恢复（S3）**：`continuable:true` 时 child 的 descriptor 写 `mode:"continuable"`，且**不写终态 `session/end`**——它的最后一个 `turn/end` 就是休息态。此后 `Runner::resume(childId, prompt, system, model)` 打开**同一个**子日志、追加一个新 turn，全程只靠 durable 日志、不依赖任何内存中的 activation，所以换一个进程也能续跑。one-shot child、已写 `session/end` 的 child、形态不是 child id 的字符串（`is_child_id`：≤64 字节、仅 ASCII 字母数字与 `-`、非 `.`/`..`）都在写任何事件之前被拒并说明原因。这就是 child 能跨重启活下来的原因：承载它的是日志，不是活着的进程。
- **多后端（G7）**：`Providers` 是具名注册表，`Options.backend_name` 选默认那一个；`config.json` 的 `subagent.providers[]` 让操作员把任意讲这套单行协议的程序注册成后端，`forge subagent worker` 只是默认的那一个。重名注册被拒（`Conflict`）——「哪份代码跑了我」不该取决于注册顺序。
- **进程外后端（S5）**：`Options::backend: Option<Arc<dyn Backend>>` 决定 child **在哪**跑。`None`（默认）就是上面这套进程内路径；`Some(ProcessBackend)` 则把 child 交给另一个进程——目前唯一实现是 `spawn` **我们自己的二进制**的 `forge subagent worker`，一行请求 JSON 进、一行响应 JSON 出（`BackendRequest`/`BackendOutcome`，`v=1`，op=`run`/`resume`）。它不调用任何 DSH/DS 服务，是一个真正独立的第二进程（自己的内存、崩溃域、环境）。选后端时**不写本地 child log**——那份 transcript 归 worker 所有；父侧仍照旧预留 sibling 名并回填 catalog 行，因为那是父日志的事。并发池与深度上限仍在这一侧生效，后端只决定合法的 child 跑在哪。嵌套 child 强制 `backend: None`（不自我递归）。

## 公开面

```rust
pub struct Options { /* provider, data_dir, workspace, model, system, tools,
   max_steps, depth, max_depth, max_active, parent_session_id, parent_log,
   clock, now, logger, policy, limits, canceller, structured_schema, backend */ }
pub struct Runner;                       // Arc<Runner> 才能取 tool()
impl Runner {
    pub fn new(Options) -> forge_core::Result<Self>;
    pub fn tool(self: &Arc<Self>) -> Arc<dyn forge_tool::Tool>;
    pub fn run(&self, &CallContext, prompt, system_override, model_override)
        -> forge_core::Result<ChildRun>;
    pub fn run_with_schema(&self, &CallContext, prompt, system, model, Option<Value>)
        -> forge_core::Result<ChildRun>;
    pub fn run_child(&self, &CallContext, prompt, system, model, Option<Value>,
                     Mode, Option<&str>) -> forge_core::Result<ChildRun>;
    pub fn resume(&self, child_id: &str, prompt, system_override, model_override)
        -> forge_core::Result<ChildRun>;
    pub fn max_depth(&self) -> usize;
}
pub struct CallContext { cancel, parent_session_id, workspace }
pub struct ChildRun { id, path, model, steps, turns, text, structured,
                       error, cancelled, name: Option<String>,
                       catalog: Option<serde_json::Value>, mode: Mode }
pub struct Canceller { /* ... */ }        // cancel_all / cancel_call
pub trait Logger { /* debug/info/warn/error，默认空实现 */ }
pub fn spec() -> forge_llm::ToolSpec;
pub fn tool_schema() -> serde_json::Value;
pub fn structured_spec(&serde_json::Value) -> forge_llm::ToolSpec;
pub const STRUCTURED_OUTPUT_TOOL: &str = "structured_output";
pub const CODE_TERMINATED: &str = "already_terminated";
pub const CODE_BAD_OUTPUT: &str = "bad_output";
```

`Runner::run` 是本 crate 唯一超出工具面的公开入口，给需要直接驱动 child 的宿主使用。`Runner::run_with_schema(ctx, prompt, system, model, Option<Value>)` 允许单次覆盖 schema；`Runner::run_child(..., Mode, Option<&str>)` 额外指定创建模式与 label（`OneShot` 收尾写 `session/end`，`Continuable` 保持打开可 `resume`）。`Runner::resume(child_id, prompt, system, model)` 对一个 continuable child 追加新 turn。空字符串 override 表示“沿用 Options 的值”（对齐 Go 的 `TrimSpace` 语义）。准备阶段失败返回 `Err`；child 已经开跑之后的失败写进 `ChildRun::error`，one-shot 的终态 `session/end` 仍然落盘。

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
- **本校验器不支持完整 JSON Schema**：`structured_output` 的校验只覆盖 `type`/`required`/`properties`/`items` 与 object/array/string/number/integer/boolean（及 null），不处理 `$ref`、`oneOf`、`patternProperties`、数值范围等；不要在文档或调用方暗示它有通用 schema 能力。
- 不要编辑 `architecture.yaml`、根 `Cargo.toml` 或任何其它 crate；本 crate 的登记行由集成方添加：
  `forge-subagent: { layer: L2, ... }`。
