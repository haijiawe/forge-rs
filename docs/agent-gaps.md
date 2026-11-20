# DSH 有、forge-rs 缺什么 —— Agent 层差距清单

> **这份文档要回答的问题**：拿原版 DSH（`deepseek-harness-master/`，`@deepseek-ai/dsh-root` 0.2.2-rc.5）
> 当标尺，逐项检查 `forge-rs` 的 agent 执行层**少了哪些能力**、现在到哪一步、缺的代价是什么。
>
> **谱系（先读这段，否则会误读全文）**：
> DSH（TypeScript）→ `forge/`（DSH 的 fork，改名换皮，仍在磁盘上）→ `forge-go`（DSH 那边写的
> Go 实现，2026-11-17 删除，只留契约与语料）→ `forge-rs`（Go 树的 Rust 移植，本仓库）。
> `forge-rs` 的 `AGENTS.md`/`README.md` 首句都写「Rust rewrite of a **Go** implementation」。
> **所以 forge-rs 相对 DSH 缺的东西，是 Go 树当年就没做或另做的，Rust 移植照单继承。**
> 这不是移植 bug，是一份待补齐的账。
>
> **状态四态**：已实现 / 部分实现 / 未实现 / 刻意不做（最后一态列出代价，但不当缺陷算）。
> 每条都指到 DSH 的源文件符号与 forge-rs 的 crate·文件；从 README/注释推断而未逐行验证的，标注「据注释，未逐行验证」。
>
> **交付记录（2026-11-20）**：P0/P1/P2 项已按波次补齐——C waterfall = `69aa3c9`(W5) + D 波 `01fd95b`（第三相位 `request-error` 决定是否重试）、F revision/CAS = `d6c98b2`(W3)、G 依赖图+journal = `fedd8fe`(W6)、H surfaceOp+pruner = `6150b27`(W4)、E 子代理 = S1–S5(`47118db`/`cec20ec`/`a42df34`/`ea42128`/`cae8d74`) + W1(`5343e28`) + S2/S3/S4 收尾（`maxActive` 配置、`ChildLock`、`Providers` 注册表、`Delivery`/`interrupt`/`deliver`、`continuation` 消息、`delegationDepth` header 权威）。仍欠：F 的 steer（同步 CLI 无异步注入通道，以 pause→cancel 表达）、H 的 image-offload（此设计下无对象可卸，非缺口）；C 的通用 `emit`/`serial` 监听器分发（仅 waterfall 三相位已做）；F 的 race fence 已补（`forge_goal::fence` + `goalfence::hook`，每步重校验）；H 的三事件事务/孤儿检测/summary-error 已补（C 波）；`interrupt` 已提供**进程内**版本（按 child id 取消已注册的 token）与**跨进程**版本（`forge subagent interrupt` 标记文件，长驻 Runner 轮询后 trip 同一 token）；一次性 worker 进程不受跨进程中断影响；A 常驻 phase 机与 D attempt 记账刻意不做。

规模对照（源码实测，排 tests/node_modules）：

| 子系统 | DSH 源文件数 | forge-rs 源文件数 |
|---|---|---|
| agent 核心 | `core/agent` 9 + `core/agent-loop` 7 | `crates/agent/src` 8 |
| 子代理 | `subagent/subagent` **22** | `crates/subagent/src` **11**（S1–S5 前为 5） |
| 目标 | `goal/goal` 7 + `goal/goal-round-driver` 3 | `crates/goal/src` 1（280 行） |
| 团队 | `team/agent-team` 10 | `crates/team/src` 5 |
| 压缩 | `compaction/` 5 个包 | `crates/context/src` 4 |

---

## A. Agent 循环形态

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 常驻 agent 对象，有身份 `id: SessionId` | `core/agent-loop/src/agent.ts` `class ReactLoopAgent` | 每次 `Agent::run` 新建 `Agent{opt,cur_turn,cur_step}`，跑完即弃（`crates/agent/src/agent.rs`） | 刻意不做 |
| phase 机 `idle \| maintenance \| running` + `agent/status` 事件 | 同上 `setPhase` | 无 phase、无 `agent/status` 事件 | 刻意不做 |
| 回合间后台维护 `runMaintenance(job)`（不进模型） | 同上 | 无 | 刻意不做 |
| `kick()`/`whenIdle()` 持续驱动 | 同上 | 一次 `run()` 驱动一个 turn | 刻意不做 |

**代价说明（此节不算缺陷）**：forge-rs 的 agent 是「读日志→发请求→写日志→返回」的纯函数，
同样日志必然产生同样请求，没有隐藏的改写者。代价是「回合之间」这个概念在它的模型里不存在——
续跑/定时只能另起一次 `run()`（`forge-goal`、`forge-schedule` 就是这么做的）。这是设计选择，
不是漏做。

---

## B. 输入队列

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 持久 inbox，从日志 fold（`agent/inbox/spliced` 事件投影） | `core/agent-loop/src/inbox.ts` `inboxProjectionDefinition` | 无专用 inbox 事件类型；但运行中的输入**已经写进日志**：运行中 POST → 自有新 turn（`202`，`messages_second_while_running_202`），`/steer` → `user/message{steering:true}`（持久） | 记录已持久；只缺 DSH 的双队列结构 |
| next-turn / next-step 双队列 | 同上 `ReactLoopInbox` | 无区分 | 未实现 |
| `followup`=next-turn+wake | `agent.ts` `send()` | grep `followup` = 0 命中 | 未实现 |
| `steer`=next-step+wake（持久） | 同上 | `/steer` 已把文本**持久写进日志**为 `user/message{steering:true}`（`crates/server/src/server.rs` `handle_steer`），投递由 `ServerSteeringSource` 从进程内 deque 喂给 `SteeringHook`；记录不丢，只是投递在进程内 | 已实现（记录持久；投递进程内） |
| `inject`=next-step 不 wake | 同上 | grep `inject` 2 命中（非队列语义） | 未实现 |
| `claim(target,turn)` 批量取 | `inbox.ts` | 无队列可 claim | 未实现 |
| `cancel(cause,{keepInbox})` 保留队列 | `agent.ts` `cancel()` | 取消是二值原子标志，无队列可保留 | 未实现 |

**代价（已核对源码后下调，比初稿轻）**：运行中追加的输入在 forge-rs 里**已经落日志**——见上行
`/steer` 与运行中 POST 的证据，进程重启后文本不丢，会作为上下文回到模型。真正的差异是结构性的：
DSH 有一个从日志 fold 出来的 next-turn / next-step **双队列**加 `claim` / `keepInbox` 语义，
forge 没有这一层，把"注入当前步"交给 `SteeringHook` 读进程内 deque。双队列能为"断点续接**正在跑的
那一轮**"提供更强保证，但那一轮本身随进程结束，收益有限，故不列为必须补的缺口。

---

## C. 扩展点（waterfall）

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| `emit/serial/waterfall` 三态分发器 | `core/agent/src/dispatch.ts` `AgentEventDispatch` | `crates/agent/src/hooks.rs` 的 `Hooks`：三相位——`pre-step`/`request` 改写（每次改写写 `hook/rewrite`）+ `request-error` 决定是否重试（`HookOutcome::Retry`，不改写不记事件）；无通用 `emit`/`serial` 监听器分发 | 已实现（三相位；W5 `69aa3c9` + D 波） |
| `agent/pre-step` 可改写 messages 或 reject | `agent.ts` `preStep()` 派发 | `Hooks::run_pre_step` + `agent.rs step()`（改写 transient，不改 durable history） | 已实现（W5） |
| `agent/request` 可改本次请求 config（改 model 抛错） | `agent.ts` `step()` 派发 | `Hooks::run_request`（hook 不能改 model id，`run_request` 强制还原） | 已实现（W5） |
| `agent/request-error` 返回 `retry` 才重来 | `agent.ts` | `Hooks::run_request_error`：传输策略本要放弃时，钩子可答 `HookOutcome::Retry`（不改写、不记 `hook/rewrite`，重试体现为下一条 `llm/call`）；无钩子时行为不变 | 已实现（D 波 `01fd95b`） |
| listener 抛错不饿死后续（自建 emit 而非 map） | `dispatch.ts` `emit` | `Hooks` 顺序执行，任一 hook 出错即 `hook_failed` loud 终止该步 | 部分实现（loud 而非跳过） |

**代价**：DSH 能在运行时用插件替换模型适配器、工具、甚至 loop；forge-rs 只开放了三个
有界扩展点（`pre-step`/`request` 改写 + `request-error` 决定重试，见 `agent-gaps.md` §C 与
ADR-0004），其余 agent 行为仍须改代码重编译。换来的是「没有**未记录**的改写者」——每次改写都
落一条 `hook/rewrite`。**这是两者最根本的哲学差：可塑性 vs 可解释性。**

---

## D. 请求与流记账

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| attempt 级事务 `AssistantStreamAttempt`（attemptId） | `core/agent-loop/src/assistant-stream.ts` | 单聚合器收 delta，只落一条 `assistant/message`（`agent.rs` `stream()`/`consume()`） | 部分实现 |
| `request/header`（reason: initial/resume/change/series） | `agent.ts` `buildRequest()` | 无 | 未实现 |
| `request/context`（provider/model/contextWindow） | 同上 | 部分信息在 `llm_call` 事件里 | 部分实现 |
| 失败/取消/流错误落 `assistant/attempt`（**不进模型历史**） | `assistant-stream.ts` `settle()` | 无独立 attempt 事件；有 `partial` 标记 | 部分实现 |
| header 冻结防插件篡改（`deepFreeze`+`WeakSet`） | `agent.ts` `buildRequest()` | 无（无插件改写层，不需要） | 刻意不做 |

**代价**：DSH 能回答「这次试了几回、每回 token、为何失败」；forge-rs 日志只有最终结果，
更省更确定，但拿不到 attempt 级重放。

---

## E. 子代理（缺口最大的一节）

DSH 的 `subagent/subagent` 有 22 个源文件；forge-rs 的 `crates/subagent/src` 现有 **11** 个
（`backend/catalog/child/descriptor/lib/list/names/options/runner/structured/tool`）——S1–S5 + W1 之后从 5 个长到 11 个。

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 容量槽 `ActivationPool.reserve(capacity)` | `subagent/src/activation.ts` | `runner.rs` counting `Semaphore{capacity,free}`，`Options.max_active`（默认 1=Go 串行；>1 可并发）；超限**排队**非拒绝 | 已实现（S2 `cec20ec`） |
| 进程外后端（another process 的子 agent） | `subagent/src/out-of-process.ts` | `crates/subagent/src/backend.rs`：`Backend` trait + `ProcessBackend`（spawn `forge subagent worker`，stdio 一行 JSON）；`Options.backend` 路由 | 已实现（S5 `cae8d74`，仅 worker 后端） |
| `structured_output` 终结工具（子代理按 schema 返回） | `subagent/src/structured.ts` `STRUCTURED_OUTPUT_TOOL` | `crates/subagent/src/structured.rs`：`structured_output` 工具 + `GuardedTool` + 极小校验器；`ChildRun.structured` | 已实现（W1 `5343e28`） |
| 深度记账 `subagentDepth`（header 权威、只增不减） | `subagent/src/depth.ts` | `runner.rs` max_depth/depth 链，上限返回 `DepthExceeded`；`session/start` 写 `delegationDepth`，冷恢复按 header 取 `max(header, options)` 作权威下限（child.rs `reopen_child_log` → `ChildRun.depth` → `child_tools(child_depth)`） | 已实现 |
| 生命周期通知（scoped dispatch + 异常隔离） | `subagent/src/lifecycle.ts` | 无；forge 子代理是同步工具调用，结果即工具结果，无异步 settled 通知 | 刻意不做（同步模型无需） |
| 子代理目录投影（chunked catalog，version 3） | `subagent/src/catalog.ts` `SUBAGENT_CATALOG_VERSION` | `crates/subagent/src/catalog.rs`（`subagent/catalog` 事件，父 loop 写）、`descriptor.rs`（子 runner 写）、`names.rs`、`list.rs` | 已实现（S1 `47118db`） |
| `subagent.prompt`（delivery: queue/steer）/ `subagent.interrupt` 控制接口 | `subagent/src/control.ts` | `prompt` 已实现：工具 `resume` 参数（S4）+ CLI `forge subagent prompt`（S5）；`interrupt` 已实现**进程内**（`runner.interrupt(child_id)` 取消该 child 注册的 cancel token）与**跨进程**（`forge subagent interrupt <child>` 写 `<dataDir>/sessions/<child>.cancel`，运行中的 `Runner` 轮询该标记并 trip 同一 token；worker 一次性进程无监听） | 已实现（进程内 + 跨进程标记） |
| 冷恢复 cold resume | `subagent/src/manager.ts` | `child.rs reopen_child_log` + `runner.resume`（跨进程靠日志，descriptor 必须 continuable） | 已实现（S3 `a42df34`） |
| 每 child 串行锁 `ChildLock.run` | `subagent/src/activation.ts` | `lock.rs` 的 `ChildLock.run(child_id, op)`（同一 child 的 resume 串行，避免 turn/start 与 turn/end 交错） | 已实现（S3 补齐） |
| 4 种后端（本地/外部产品委派等） | `subagent/src/`（provider 抽象） | `Providers` 注册表（注册/按名解析，重名 `Conflict`、空名 `Invalid`）+ `Backend` trait（唯一 `ProcessBackend`）；`backend_name` 未注册报 `NotFound` | 已实现（S2/S5） |

**代价**：DSH 的子代理是一个能容纳多种后端、能观察失败、能按 schema 交付、能被外部 steer/
interrupt、能冷恢复的子系统；forge-rs 是一个刻意最小的递归装置——单并发、depth 链、结果从
子日志派生。这是「一个子系统」与「一个工具」的差距，也是这次清单里最该优先补的地方。

---

## F. 目标续跑

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 事件驱动 round driver（inject agents/goals/sessions） | `goal/goal-round-driver/src/index.ts` | `crates/goal/src/lib.rs` 纯函数 `next_action()` + CLI 层循环 | 部分实现（设计选择） |
| revision + CAS 并发防重 | 同上 `RoundIdentity{goalId,revision,round}` | `Goal.revision`、`RoundIdentity`、`CasOutcome{Written,Stale}`、`next_action_identity()`；CLI 续跑前 `append_continued_if_current` 只有 revision 一致才写 `goal/continued` | 已实现（W3 `d6c98b2`） |
| `agent/pre-step` waterfall 上校验续跑（race fence，每 checkpoint 重校验） | 同上 `validReservation()` | `forge_goal::fence(events,&Reservation)->FenceOutcome{Valid,Ended,Stale}`（纯比较，4 单测）；CLI `goalfence::hook` 给每个 goal round 挂 `pre-step` 钩子，每步重读日志比对 revision/round/status，失配则 `hook_failed` 中止本轮（2 端到端单测） | 已实现（per-step fence） |
| 运行中 pause/steer 打断（`goal/changed` → `agent.cancel`） | 同上 | `execute_goal` 接 Ctrl-C → `cancel_open_goal` 写 `goal/ended{reason:cancelled}`；per-step fence 也会在目标被外部结束时停轮。**steer（向运行中的轮注入消息）未做**：同步 CLI 无异步注入通道，暂以 pause→cancel 表达 | 部分实现（pause 已实现；steer 刻意不做） |
| goal domain / fold / runtime 分层 | `goal/goal/src/{domain,fold,runtime,client}.ts` | `goal/src/lib.rs` 单文件含 Status/fold/构造器 | 部分实现 |
| round 预算计数 | `goal-round-driver` maxGoalRounds | `next_action()` 已实现（max_rounds 是轮预算，rounds 数续跑次数，注释详述） | 已实现 |

**代价**：DSH 的续跑能在任意时刻被打断、重校验、恢复；forge-rs 的续跑是确定性决策，重启
从日志 fold 即知状态（`GET /api/sessions/{id}/goal` 就这么做），但少了 revision 级并发安全与
运行中干预。

---

## G. 团队

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| task-board（任务板） | `team/agent-team/src/task-board.ts` | `crates/team/src/board.rs`(849 行，CAS 排他) | 已实现 |
| task-graph 依赖校验（missing/duplicate/cycle） | `team/agent-team/src/task-graph.ts` `TeamTaskGraphError` | `crates/team/src/task-graph.rs`：`TeamTaskGraphError{Missing,Duplicate,Cycle}`、`validate_plan`（duplicate→self/missing→DFS 查环，点名两端） | 已实现（W6 `fedd8fe`） |
| journal 事务（per-Lead 顺序 + 提交） | `team/agent-team/src/journal.ts` | `crates/team/src/journal.rs`：`Journal{open,record,transact,entries,last_seq}` + `LeadTxn`（事务 Err 整笔丢弃、重开续号） | 已实现（W6 `fedd8fe`） |
| projection 状态 | `team/agent-team/src/projection.ts` | `board.rs` 持久化 + `/api/teams` 路由 | 部分实现 |
| task-view 呈现 | `team/agent-team/src/task-view.ts` | web `TeamBoards.tsx` 面板 | 部分实现 |
| mailbox（成员通信） | （DSH 经 inbox/control） | `crates/team/src/mailbox.rs`(250 行) | 已实现（forge 自有的） |
| 并行度由 permits 控制（worker 各一线程） | — | `run.rs` `Plan{workers,max_parallel}` + `RecordingRunner::max_inside` 实测 | 已实现 |

**代价**：forge-rs 的团队是一个「共享板 + CAS + permits」的实干工具，缺的是 DSH 那套依赖图
校验与 per-Lead 事务 journal 的精细治理。

---

## H. 上下文压缩

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 三事件锁 `compaction/start`→`summary`→`end` | `compaction/compaction/README.zh.md:95-97`、`src/index.ts` | `crates/session/src/compact.rs` 的 `compact_log` 现在用 `compaction/start`…`compaction/end` 夹住一次压缩（`append_control`），summary 仍是一条 `compaction/summary`；fork 的 `context/compressed` 与 `surfaceOp` 是另一条可寻址压缩路径 | 已实现（C 波） |
| `surfaceOp: {op:'replace',startSeq,endSeq}` 表层改写（唯一表层变更） | `compaction/src/types.ts`、`checkpoint.ts` | `context/compressed` 加可选 `surfaceOp`（`SurfaceOp{Hide,Replace}`）；`agent/history.rs` 的 `to_llm_messages_with_mode` 支持 hide/replace 两模式，仍两遍配对闭合 | 已实现（W4 `6150b27`） |
| 孤儿锁可检测（start 与 end 间崩溃留痕迹，非虚假成功） | `compaction-basic/src/region.ts` | `crates/session/src/compact.rs` 的 `open_compaction(store)->Option<u64>` 报出没有 `compaction/end` 的 `compaction/start`（3 个单测，含手工孤儿 start） | 已实现（C 波） |
| `compaction/summary-error` waterfall 恢复协议 | `compaction/src/index.ts` | 摘要器报错或返回空白时写 `compaction/summary-error{reason,message}` + `compaction/end{ok:false}`，**不写 summary、不隐藏历史**，且 `compact_log` 返回 `Ok(false)` 不让整轮 turn 失败 | 已实现（C 波，方向=多给） |
| image-offload（摘要时省略图片） | `compaction-image-offload/` | **不需要**：附件字节存在 `<dataDir>/blobs/`，事件里只有引用（`{kind,id,mediaType,bytes,…}`），摘要本身是纯文本，事件里没有可卸载的图片负载 | 等价（机制上无对象可卸） |
| tool-result-pruner（裁剪旧工具结果） | `compaction-tool-result-pruner/` | `crates/context/src/prune.rs` 的 `prune_tool_outputs`（按阈值裁旧 tool/result 正文、保配对、UTF-8 边界） | 已实现（W4 `6150b27`） |
| tool-pairing 安全切点（按 tool call/result 内容而非 step 标记） | `compaction/src/tool-pairing.ts` | `history.rs` `closed_hidden_set()` 两遍配对闭合（同类保证，机制不同） | 已实现（等价） |
| 摘要失败方向 | 拒绝不缩小源内容的摘要 | 失败方向是「不隐藏」（多给） | 已实现（等价，保守） |

**代价**：DSH 能精确表达「哪里被替换、事务是否完整、摘要失败怎么恢复」，并支持图片卸载与
结果裁剪；forge-rs 用一条 `compaction/summary` + 事务括号做到「少发旧消息」，失败方向安全，
C 波补齐了三事件事务、孤儿检测与失败不隐藏；仍差的是通用的 `compaction/summary-error`
waterfall 插件式恢复（forge 是内建策略，不是可插拔监听器）；image-offload 在此设计下无对象可卸。

---

## 汇总：按「该优先补什么」

| 优先级 | 缺口 | 状态 |
|---|---|---|
| ~~P0~~ | E 子代理：结构化输出、进程外后端、容量槽 >1、catalog、cold resume | **已交付** S1–S5 + W1（`47118db`/`cec20ec`/`a42df34`/`ea42128`/`cae8d74`/`5343e28`）+ S2/S3/S4 收尾（`maxActive`/`ChildLock`/`Providers`/`Delivery`/`interrupt`/`continuation`/`delegationDepth`） |
| — | B 输入队列 | 已核实非缺口：`/steer` 与运行中 POST 已持久落日志（`03ab35d`） |
| ~~P1~~ | F 目标续跑 revision/CAS + 打断 | **已交付** revision/CAS（W3 `d6c98b2`）+ per-step race fence（`forge_goal::fence` + `goalfence::hook`，B 波 `30e760c`）；仅 steer 不做（同步 CLI 无异步注入通道，以 pause→cancel 表达） |
| ~~P1~~ | H 压缩 surfaceOp + pruner + 事务 | **已交付** surfaceOp 与 tool-result-pruner（W4 `6150b27`）+ 三事件事务/孤儿检测/summary-error 失败不隐藏（C 波）；image-offload 在此设计下无对象可卸 |
| ~~P2~~ | C waterfall 扩展点 | **已交付** 三相位 `pre-step`/`request`/`request-error` hooks（W5 `69aa3c9` + D 波）；无通用 `emit`/`serial` 监听器分发 |
| ~~P2~~ | G 团队依赖图 + journal | **已交付**（W6 `fedd8fe`） |
| — | A 常驻 phase 机、D 请求/流 attempt 记账 | 刻意不做（设计选择；要补需先决定是否做常驻服务） |

---

## 这份文档没做的事

- 没动任何 Rust/web 代码——这是「记录缺失」，不是「补齐缺失」。
- 没把「刻意不做」（A 节、D 节的冻结、F 节的 steer）写成缺陷——那些是设计选择，写了代价供
  决策，不做冤枉账。
- DSH 侧部分行为从 README/注释推断，已逐条标注；未逐行验证的没有当作已确认事实。
