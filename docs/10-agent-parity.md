# 用 DSH 反推补齐 forge-rs —— 执行路线图

> **这份文档是什么**：[agent-gaps.md](agent-gaps.md) 是**账本**（DSH 有、forge-rs 缺什么，现状与代价）。
> 这份是**路线图**（怎么补、什么顺序、用什么语言、每波的验收线在哪）。两者互补：
> 前者回答"差多少"，后者回答"下一步干什么"。
>
> **状态（2026-11-20 更新）**：路线图已基本执行完——W1/W3/W4/W5/W6 已交付并各有 commit（见
> 下）；W2 经核对判定「冻结契约已满足，无需新代码」。仍刻意不做：A 常驻 phase 机、D attempt
> 记账。子代理的深移植（S1–S5）另见 [12-subagent-port.md](12-subagent-port.md)。
>
> **口径**：缺口清单 `agent-gaps.md` 的事实全部来自源码（DSH `deepseek-harness-master/`
> 0.2.2-rc.5，forge-rs 以仓库当前状态为准）；路线图里的顺序与验收是**建议**，由用户拍板。

---

## 1. 语言决策：整条补齐链用 Rust，唯一的例外是插件体

用户的问法是「用重构那边的、最符合最好的编程语言选择」。答案不是「Rust 最快」——**Rust
在纯吞吐上不是最快的**——而是本项目的缺口形态决定的：

| 层 | 语言 | 判据 |
|---|---|---|
| agent 循环、输入队列、子代理、目标续跑、团队、压缩、waterfall 扩展点（W1–W6） | **全部 Rust**（在现有 crate 内加文件，或按层拆新 crate） | 这些缺口是**状态机与队列的解析、管道、校验**，不涉 CPU 密集计算。第二条编译链会把 CI、交叉编译、错误边界三样成本同时翻倍，而换来的吞吐在这个瓶颈上量不出来 |
| 插件体（W5 若做） | **插件作者自选**（Node / Python / 任何能讲 stdio JSON-RPC 的进程） | 宿主只讲协议、不嵌运行时。P3 已实测：Node 父↔子一次 JSON-RPC 往返 空 result **80.6 µs** / 4 KB **118.3 µs** / 64 KB **343.7 µs**，对照一次模型往返 **1,203 ms**——插件调用占 **0.007%**，语言在此不构成决策依据 |
| 前端（能力面板等） | **TypeScript**（`web/src`，Vite/React 既有栈） | 已有栈，无新增 |

**否决，并写清理由**（防止同一个决定被反复重议）：

- **用 Go 写调度/goal 循环**：多一条 GC 运行时 + 第二套交叉编译 + 第二套错误边界；而
  `forge-go` 已经证明"Go 写过一版"这条路走了又删，重走等于交两次学费。
- **把 Python 嵌进二进制**：打包、ABI、信号处理、GIL 在长驻进程里全是坑；插件已经能用
  外部 Python 进程（P3 的 `tests/reference-node/smoke.py` 就是）。
- **用 WASM/wasmtime 承载插件**：把"进程隔离"降级成"同进程沙箱"，与 `docs/contracts/`
  里的沙箱契约（真进程、真文件系统遏制）正面冲突；多一个引擎依赖也超出 9 条预算。

结论一句话：**内核单一 Rust，插件语言自由，前端 TypeScript**。这是"最符合"（与现有 24 crate
同构、archcheck 可查、依赖预算不涨）与"最好"（该快的快、该隔离的隔离）的交点，不是单一维度
的极值。

---

## 2. 波次总览

严格沿用 `09-platform-tier.md` §5 的纪律：**每波独立可交付、独立可验证；上一波的门禁全绿
之前不开下一波。** 顺序按 `agent-gaps.md` 汇总表的 P0→P1→P2，但每波拆到「一个 crate 或一组
互斥文件 + 一条验收线」的粒度，避免一波动到两个 crate 的边界。

| 波 | 主题 | 优先级 | 动到的 crate | 新增 crate？ | 是否动冻结契约 | 是否需拍板 |
|---|---|---|---|---|---|---|
| W1 | 子代理结构化输出 | P0 | `forge-subagent`、`forge-tool` | 否 | 否（复用 tool/result） | 否 |
| W2 | 持久输入队列 | — | 无需改动（核对后由冻结契约满足） | 否 | 否 | 否 |
| W3 | 目标续跑 revision/CAS + 运行中打断 | P1 | `forge-goal`、`forge-cli`、`forge-agent` | 否 | **是**（`goal/*` payload 新形状） | 否 |
| W4 | 压缩事务与结果裁剪 | P1 | `forge-context`、`forge-agent`(history) | 否 | **是**（`context/compressed` 增 `surfaceOp`） | 否 |
| W5 | waterfall 扩展点 | P2 | `forge-agent`、`forge-plugin` | 否 | 是（新 hook 事件） | **是（改定位）** |
| W6 | 团队依赖图 + journal | P2 | `forge-team` | 否 | 否 | 否 |

**交付状态（2026-11-20）**：W1=`5343e28`、W3=`d6c98b2`、W4=`6150b27`、W5=`69aa3c9`、W6=`fedd8fe` 均已交付并通过门禁；W2 见上（无需改动）。子代理深移植 S1–S5 见 `docs/12-subagent-port.md`（`47118db`/`cec20ec`/`a42df34`/`ea42128`/`cae8d74`）。

**没有一波新增 crate**：所有缺口都落在现有 crate 的既有层里（见 `architecture.yaml`）。这是
有意的——`archcheck` 的边缘越少，漂移面越小；只有当真需要新层时（例如把 waterfall 做成独立
可替换组件）才拆 crate，而那是 W5 的事，且需先改定位。

**依赖预算**：预计 W1–W6 **一条外部依赖都不新增**（9 条预算不动）。任何一波若发现非加不可，
先在文档里回答"为什么标准库不行"，由用户批准。

---

## 3. 逐波展开

每一波都按同一模板写：**范围 / 契约变更 / 测试与验收 / 风险与回退 / 语言**。

### W1 —— 子代理结构化输出（P0，最小可用切片）

**问题（来自 `agent-gaps.md` E 节）**：DSH 的子代理能把最终结果按 schema 交回父侧
（`subagent/src/structured.ts` 的 `STRUCTURED_OUTPUT_TOOL = 'structured_output'` + 终结工具
守卫）；forge-rs 的子代理只回自由文本，父侧想拿结构化结果只能解析文本。

**范围**：
- `crates/subagent/src/` 新增 `structured.rs`：定义 `structured_output` 终结工具，参数
  schema 由 `Options.structured_schema` 携带；子代理注册该工具，调用即视为"完成"，其后
  再调任何工具由"已终结"守卫拒绝。
- `crates/subagent/src/options.rs`：`Options` 加 `structured_schema: Option<serde_json::Value>`。
- `crates/subagent/src/tool.rs`：`subagent` 工具的 `spec()` 参数表加可选 `schema`。
- `crates/subagent/src/runner.rs`：`ChildRun` 增 `structured: Option<serde_json::Value>`，从子
  日志里那次 `tool/result` 的 args 取。

**契约变更**：**无新事件类型**。结构化结果是子代理自己日志里的一条 `tool/result`，父侧从
子日志派生——与现有"结果从独立子日志读"的模型一致。这是本波刻意选最小切片的原因：它不需要
动 `events-v1`、不需要新 conformance 用例的事件种类。

**校验器**：不引 JSON-schema crate（预算）。写一个只覆盖 `{"type","required","properties",
"items"}` 的极小校验器，放在 `structured.rs`，reason 写进代码注释："我们要的不是 JSON Schema
的全部，是子代理交回结果时能当场判'对不对'；一个两百行的子集比拖一个 schema 引擎和一个
validator 依赖更小、更好审。"

**测试与验收**：
- 单元：校验器接受/拒绝（缺 required、类型不符、`items` 递归）。
- 集成：脚本 provider 让子代理调 `structured_output` → 父侧 `ChildRun.structured` 拿到对象；
  子代理不调、直接回文本 → 父侧报 `bad_output`（不是成功）。
- 门禁：`cargo test -p forge-subagent --offline`、clippy `-D warnings`、`archcheck: ok`。

**风险与回退**：校验器与真 JSON Schema 的偏差（只认子集）——在 `subagent/README.md` 的
「不要做」里写明"不要以为这支持完整 JSON Schema"；回退就是删 `structured.rs` 与 `Options`
字段，不碰其他 crate。

**语言**：Rust，`serde_json::Value`（零新依赖）。

---

### W2 —— 持久输入队列（已核对：冻结契约已经满足，无需新代码）

**结论**：本波原计划（新增 `agent/inbox/*` 事件、改运行中 `POST /messages` 从 409 到 202 入队）
**基于一个错误前提**——"forge 运行中追加输入会丢"。核对源码后，前提不成立：

- 运行中 `POST /api/sessions/{id}/messages` **本来就是 202 + 为它开一个新的自有 turn**：
  `crates/server/src/server.rs` 的 `accept_message` 只在 `ended`/`aborted`/`awaiting_approval`
  时返回 409，其余（含 `running`）落到 `_ => {}` 继续 `let turn = state.max_turn.max(state.turns)+1`
  并起跑；`handle_post_message` 对非去重结果回 202。冻结契约与语料都已钉住这一点：
  `tests/conformance/cases/http/messages_second_while_running_202.json`（描述原文"accepted with
  202 and queued"）与 `messages_conflict_awaiting_approval_409.json`。
- `/api/sessions/{id}/steer` **本来就把文本持久写进日志**：`handle_steer` 追加一条
  `user/message{steering:true}`（含 idempotencyKey），投递再从进程内 deque（`ServerSteeringSource`）
  喂给 agent 的 `SteeringHook`。也就是说**记录已经在日志里**，进程重启后文本作为上下文回到模型；
  只有在"注入当前正在跑的这一步"时依赖进程内 deque。

因此"再引入一份 `inbox/queued` 事件"会把日志已经记录的事实**再记一遍**，正是本仓库最忌讳的
"两份真相"，且改 `POST /messages` 会回归已冻结的 `api.md`（第 281–284 行已承诺"each accepted
message gets its own turn … a second POST is accepted while the previous turn is still running"）。

**范围**：无代码改动。真实残差只是 DSH 那种"从日志 fold 的 next-turn / next-step 双队列 + `claim` /
`keepInbox`"这一**结构**；它能为"断点续接正在跑的那一轮"提供更强保证，但那一轮本身随进程结束，
收益有限，故不列为待补缺口（见 `docs/agent-gaps.md` B 节已按此改写）。

**测试证据**：`cargo test -p forge-server --offline` 覆盖上述两个 http 语料；`handle_steer` 的
`user/message{steering:true}` 由 `crates/server` 现有测试与投影测试覆盖。

**语言**：Rust。

---

### W3 —— 目标续跑 revision/CAS + 运行中打断（P1）

**问题（F 节）**：DSH 的续跑是**事件驱动 + `RoundIdentity{goalId,revision,round}` + 每
checkpoint 重校验的 race fence**（`goal-round-driver/src/index.ts` `validReservation()`），
能在任意时刻 pause/steer 打断；forge-rs 是纯函数 `next_action()` + CLI 循环，单机够用、多入口
不安全。

**范围**：
- `crates/goal/src/lib.rs`：`Goal` 加 `revision: u64`；`next_action()` 带上当前 revision；
  fold 时从 `goal/created|continued|ended` 的 payload 恢复 revision；新增 `RoundIdentity`。
- `crates/cli/src/goalcmd.rs`/`chat.rs`：续跑前写 `goal/continued{round, revision: n}` 时用
  CAS——只有日志里最新 revision 与本地一致才继续，否则放弃本轮（另一个入口已经推进了）。
- `crates/agent`：暴露一个取消钩子（已有 `Cancel`），供运行时在收到 pause 时触发。

**契约变更**：`goal/*` 事件 payload 增 `revision` 字段。**这是新形状——按 AGENTS.md 法则 2
"payload 形状变了就是新类型"**，所以要么加 `goal/created` 的 v2 类型名，要么在文档里说明
"revision 是可选追加字段、旧 reader 跳过"。两案二选一，倾向后者但必须在 `events-v1.md` 写死
"缺失 revision 视为 0"。新增 conformance 用例。

**测试与验收**：
- 两个 CLI 进程同时对一个 goal 续跑 → 只有一个写进 `goal/continued`，另一个因 CAS 失败退出，
  **不双跑**。
- 运行中发 pause → 下一轮开始前停下，日志有 `goal/ended{reason: cancelled}`。
- 门禁全绿。

**风险与回退**：revision 语义与旧日志的兼容（缺失=0）——写测试固化。回退：删 revision 字段
与 CAS，退回到纯函数（仍能用，只是不安全）。

**语言**：Rust。

---

### W4 —— 压缩事务与结果裁剪（P1）

**问题（H 节）**：DSH 用三事件锁（`compaction/start`→`summary`→`end`）+ `surfaceOp:
{op:'replace',startSeq,endSeq}` 做**唯一表层改写**，崩溃留可检测孤儿锁，摘要失败有恢复协议，
另有 image-offload、tool-result-pruner；forge-rs 是一条 `context/compressed` 事件按 seq 区间
**隐藏**（不是 replace），无事务、无失败恢复、无专用裁剪器。

**范围**（这是最"改法"而非"加法"的一波，风险最高）：
- `crates/context/src/wire.rs`：`context/compressed` payload 增可选 `surfaceOp`（`op:replace` +
  `startSeq`/`endSeq`），保留现有"隐藏区间"为默认（向下兼容）。
- `crates/agent/src/history.rs`：`to_llm_messages_hiding` 支持两种模式——"hide"（现状）与
  "replace"（用摘要 user 消息替换选中范围，保持 tool 配对闭合）。
- `crates/context`：新增 `pruner` 能力——按阈值裁剪旧 `tool/result` 的 output，保留 call/result
  配对（与 tool-pairing 同源保证）。
- 事务性（三事件锁）**作为可选加强**：先做 replace 语义，三事件锁列为 W4 的"若崩溃可检测性
  被要求再补"。

**契约变更**：`context/compressed` 增 `surfaceOp`（新形状=新类型名或可选字段，同 W3 的二选一
纪律）；`events-v1.md` 补表；conformance 用例。

**测试与验收**：
- replace 后：模型看到的请求里被选范围消失、摘要 user 消息在位、tool 配对闭合（不会出现
  "有 result 无 call"的 400）。
- pruner：超阈值后旧工具结果被裁剪、call/result 仍成对。
- 失败方向：摘要为空/失败时**不替换**（保守，多给）。
- 门禁全绿。

**风险与回退**：replace 动 projection/fork 的读语义——这是 `09-platform-tier.md` §5 把
"可寻址压缩"排在 P4 的原因。**W4 必须先确认没有其他波次还依赖"隐藏"语义**；回退：保留
`hide` 为默认、`replace` 为可选，删掉 replace 分支即可。

**语言**：Rust。

---

### W5 —— waterfall 扩展点（P2，**需用户拍板是否做**）

**问题（C 节）**：DSH 的 `dispatch.ts` 有 `emit/serial/waterfall` 三态，`agent/pre-step`、
`agent/request`、`agent/request-error` 可被插件拦截/改写/reject；forge-rs grep `waterfall` =
**0 命中**——没有可改写层。

**为什么单列并要拍板**：这一波与 forge-rs 的核心设计原则**正面冲突**。ADR/README 反复强调
"没有隐藏改写者，同样日志必然产生同样请求"。加 waterfall 就是**故意引入可改写层**，代价是
可解释性下降。这不是"补个缺"，是**要不要把 forge-rs 从'确定性内核'改成'可配置产品'**：

- 若答案是"要"，W5 是一项**定位变更**，必须先改 ADR、再动代码。
- 若答案是"不要"，W5 记为**刻意不做**（和常驻 phase 机、attempt 级记账同列），并在
  `agent-gaps.md` 的 C 节标状态。

**范围（仅当拍板做）**：`crates/agent` 从纯函数扩出 `pre-step`/`request` 钩子；
`crates/plugin` 暴露 hook 注册；新事件类型 + conformance。默认**不排入执行队列**。

**验收（若做）**：一个插件在 `agent/pre-step` 拦截并改写 messages，被记录、可复现；不装插件时
行为与现在逐字节一致。

**语言**：Rust（钩子层）+ 插件侧作者自选。

---

### W6 —— 团队依赖图 + journal（P2）

**问题（G 节）**：DSH 的 `team/agent-team` 有 `task-graph.ts`（`TeamTaskGraphError`：
missing/duplicate/cycle 依赖校验）+ `journal.ts`（per-Lead 事务序）；forge-rs 有 board（CAS）+
mailbox + permits，但缺依赖图校验与 journal。

**范围**：
- `crates/team/src/task-graph.rs`（新文件）：对 `Plan` 里的任务依赖做 missing/duplicate/cycle
  校验，plan 提交前跑。
- `crates/team/src/journal.rs`（新文件）：per-Lead 的事务序记录（落 board 目录），保证同一
  Lead 的多次操作有序。

**契约变更**：无新事件类型（团队状态在 board 目录，非事件日志）；若 journal 要进日志另议。

**测试与验收**：循环依赖被拒并**点名是哪两个任务**；重复依赖被拒；两个 worker 争同一 scope
由现有 CAS 挡住（已有测试）。

**风险与回退**：journal 与现有 CAS 职责重叠——文档要写清"CAS 防并发写、journal 定顺序，两者
不互替"。回退：删两个新文件即可。

**语言**：Rust。

---

## 4. 横切纪律（每一波都适用）

- **日志只有一个写入者**：工具只回 `result.meta`，loop 写事件。新事件类型必须进
  `EventType::ALL`（否则 `validate` 拒为"does not match the v1 envelope"）并配 conformance 用例。
- **未知类型是数据、未知版本是错误**：加事件不升版本；改 payload 形状=新类型名，不复用旧名。
- **依赖预算**：9 条不动；要加先回答"为什么标准库不行"，用户批。
- **每个 crate `#![forbid(unsafe_code)]`**；新 crate 需 README 七键 + 四个中文二级标题。
- **不动 conformance 语料让 reader 过**（六反模式之一）；契约变了就同提交改 `docs/contracts/`。
- **门禁**：每波结束必须 `cargo fmt --all --check`、`RUSTFLAGS='-D warnings' cargo clippy
  --workspace --all-targets --offline`、`cargo run -q -p archcheck --offline`、`cargo test
  --workspace --offline`、`pwsh -NoProfile -File .\check.ps1` 全绿；动 web 再加 `npx tsc
  --noEmit` + `npx vitest run`。

---

## 5. 明确"没做的事"与"需拍板"

| 项 | 归类 | 说明 |
|---|---|---|
| W5 waterfall | **需用户拍板** | 与"无隐藏改写者"原则冲突；做=定位变更，需先改 ADR |
| 常驻 phase 机（A 节） | 刻意不做 | 代价：回合间无维护通道；续跑/定时另起 `run()` |
| attempt 级流记账（D 节） | 刻意不做 | 代价：拿不到 attempt 级重放；换来日志更小更确定 |
| 三事件锁孤儿检测（H 节） | 先做 replace，锁列可选 | 单事件无事务需求，缺可检测性是代价 |
| Windows 完整性标签 | 用户已说"留着" | 不在本路线图内 |
| 工期 | 不给 | 只给顺序与验收；不承诺"DSP 完全等价" |

**本路线图不承诺的事**：不承诺每一波都做（P2 两波可由用户裁掉）；不承诺工期；不承诺与 DSH
逐字节等价——只承诺**能力项可逐条核对**，判据是 `agent-gaps.md` 的表格。

---

## 6. 与现有文档的关系

| 文档 | 关系 |
|---|---|
| [agent-gaps.md](agent-gaps.md) | 本路线图的**事实底座**（缺什么）；本文件是它的**执行面**（怎么补） |
| [agent-comparison.md](agent-comparison.md) | 两条实现沿革与设计差异（为什么会有这些缺口） |
| [09-platform-tier.md](09-platform-tier.md) | ADR-0003，P1–P4 平台层波次；本文件是它的**续篇**（P4 之后补 agent 层） |
| [08-rust-port.md](08-rust-port.md) | ADR-0002，语言与 crate 分层；本文件 §1 的语言决策沿用其纪律 |
