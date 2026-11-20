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
> **交付记录（2026-11-20）**：P0/P1/P2 项已按波次补齐——C waterfall = `69aa3c9`(W5) + D 波 `01fd95b`（第三相位 `request-error` 决定是否重试）、F revision/CAS = `d6c98b2`(W3)、G 依赖图+journal = `fedd8fe`(W6)、H surfaceOp+pruner = `6150b27`(W4)、E 子代理 = S1–S5(`47118db`/`cec20ec`/`a42df34`/`ea42128`/`cae8d74`) + W1(`5343e28`) + S2/S3/S4 收尾（`maxActive` 配置、`ChildLock`、`Providers` 注册表、`Delivery`/`interrupt`/`deliver`、`continuation` 消息、`delegationDepth` header 权威）。仍欠：F 的 steer（同步 CLI 无异步注入通道，以 pause→cancel 表达）、H 的 image-offload（此设计下无对象可卸，非缺口）；C 的通用 `emit`/`serial` 监听器分发已补（G2 的 `Dispatch`）；F 的 race fence 已补（`forge_goal::fence` + `goalfence::hook`，每步重校验）；H 的三事件事务/孤儿检测/summary-error 已补（C 波）；`interrupt` 已提供**进程内**版本（按 child id 取消已注册的 token）与**跨进程**版本（`forge subagent interrupt` 标记文件，长驻 Runner 轮询后 trip 同一 token）；一次性 worker 进程不受跨进程中断影响；A 常驻 phase 机 = G3/G4（`49ab189`，`phase`/`kick`/`when_idle`/`run_maintenance` + 双持久队列 `agent/inbox`）、D attempt 记账 = G1（`09c255e`，`request/header` + `assistant/attempt`）、F steer = G5（`ded692a`，每步先认领 `next-step` 队列）、E 子代理异步 settled 通知 = G6（`Runner::spawn` → 父队列 `next-turn`，工具 `background:true`）。**八项（四项「刻意不做」+ 四项「部分实现」）已全部补齐。**
>
> **H 波（2026-11-20，参照 DSH 但按「比他好」重做）**：H1 `request/context` 记实际生效的采样参数与系统提示指纹（DSH 只记 provider/model）`5cc02ed`；H2 `turn/end.cancelledBy` 把停止原因**写进日志**（DSH 只在内存）`226921d`；H3 `forge-goal` 拆 `domain`/`fold`/`runtime` 且轮次决策变**纯函数** `next(snapshot)`（DSH 是异步有状态 driver），`Superseded` 与 `Ended` 分开 `45bfa5c`；H4 `forge web` 每轮后抽干队列 `b4be25d`；H5 `session/archived` + `forge session archive` + 归档祖先拒跑（把 `archive-admission` 做出来）`f74ecbf`；H6 用**证据测试**替代 `deepFreeze` `474677b`；H7 每会话一个常驻 `Agent` 跨轮复用（`host_builds` 计数证明只装配一次）。
>
> **2026-11-20 逐符号复核**：补齐的是这八项。对着 DSH 继续逐文件比，当时**仍有 6 项未实现**。其中 6 项已于同日补齐（`listDescendants`、`resolveAgentPath`、`subagentTiming`（库 + CLI + HTTP + 面板）、错误码词表、`interrupt` 祖先权限，以及 `archive-admission`（H5，用 `session/archived` + `forge session archive` 自建了归档概念））——**这一轮台账里已经没有「刻意不做」的归档项**。见文末「复核」节与 E 表末尾六行。**「与 DSH 结构等价」仍只对已点名符号的那批成立，不要用来概括整个 agent 层。**

规模对照（源码实测，排 tests/node_modules）：

| 子系统 | DSH 源文件数 | forge-rs 源文件数 |
|---|---|---|
| agent 核心 | `core/agent` 9 + `core/agent-loop` 7 | `crates/agent/src` 8 |
| 子代理 | `subagent/subagent` **22**（4,784 行） | `crates/subagent/src` **15**（3,726 行；S1–S5 前为 5） |
| 目标 | `goal/goal` 7 + `goal/goal-round-driver` 3 | `crates/goal/src` 4（domain/events/fold/runtime） |
| 团队 | `team/agent-team` 10 | `crates/team/src` 5 |
| 压缩 | `compaction/` 5 个包 | `crates/context/src` 4 |

---

## A. Agent 循环形态

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 常驻 agent 对象，有身份 `id: SessionId` | `core/agent-loop/src/agent.ts` `class ReactLoopAgent` | `Agent` 带相位、待办队列与 `kick()`，同一进程内可以多轮驱动同一个会话（G3/G4）；`forge web` 现在**每个会话只装配一个 `Agent` 并跨轮复用**（H7 `crates/cli/src/web.rs` 的 `ResidentHost` + `hosts` 缓存，`host_builds` 计数器证明两轮只装配一次），每轮结束再抽干队列（H4 `crates/cli/src/resident.rs`）。CLI 单命令仍是每条命令建一个 | 已实现（web 常驻宿主，H7；CLI 单命令仍是一次性进程） |
| phase 机 `idle \| maintenance \| running` + `agent/status` 事件 | 同上 `setPhase` | `crates/agent/src/agent.rs` 的 `phase: AtomicU8`（`PHASE_IDLE/RUNNING/MAINTENANCE`）+ `phase()` + `set_phase(next, record)`；`agent/status` 由 `run_maintenance` 写（普通 turn 不写，否则每轮多两行诊断、golden 全变） | 已实现（G3/G4） |
| 回合间后台维护 `runMaintenance(job)`（不进模型） | 同上 | `Agent::run_maintenance(job)`：进 `maintenance` 相位、跑闭包、回 `idle`，闭包不进模型 | 已实现（G3/G4） |
| `kick()`/`whenIdle()` 持续驱动 | 同上 | `Agent::kick()` 抽干队列（先 next-step 再 next-turn，每条消息一个 `run`）直到为空；`when_idle()` 报告是否已经静止。`run()` 本身仍是「恰好一个 turn」，向后兼容 | 已实现（G3/G4） |

**仍然存在的代价**：一个 `turn` 依然是「读日志→发请求→写日志→返回」的纯函数，同样日志产生同样请求，没有隐藏的改写者——这一点没有让步。G3/G4 补的是**同一进程内的驻留**（相位、待办、`kick`），H4 把这个驻留接到了真实的长命进程上：`forge web` 在每轮结束后调 `resident::drain`，把 `agent/inbox` 里排着的东西（后台子代理的结算、另一进程写的 steer、浏览器第二次 POST）就地跑掉，**不需要等操作员再发一条消息**。但 CLI 单命令仍没有常驻宿主，也没有后台守护进程：要跨命令存活，仍然靠日志与 `forge-goal`/`forge-schedule` 另起一次 `run()`。做成 DSH 那样的长驻宿主得先决定是否引入后台进程，那是运维决定而不是代码决定。

---

## B. 输入队列

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 持久 inbox，从日志 fold（`agent/inbox/spliced` 事件投影） | `core/agent-loop/src/inbox.ts` `inboxProjectionDefinition` | `agent/inbox` 事件（splice 形状 `{target,start,removed,inserted}`，`crates/agent/src/inbox.rs` 的 `fold`），`Agent::pending`/`enqueue`/`kick` 全部从日志 fold，第二个进程能看到同一份待办（G3/G4） | 已实现（`agent/inbox`，G3/G4） |
| next-turn / next-step 双队列 | 同上 `ReactLoopInbox` | `crates/agent/src/inbox.rs` 的 `Inbox{next_turn,next_step}`，`Target::{NextTurn,NextStep}`；`kick()` 先取 next-step 再取 next-turn（先入队的 turn 消息不会插队） | 已实现（G3/G4） |
| `followup`=next-turn+wake | `agent.ts` `send()` | `Agent::enqueue(text, Target::NextTurn)` 再 `kick()`；`run()` 仍是「恰好一个 turn」，`kick()` 才是循环 | 已实现（G3/G4） |
| `steer`=next-step+wake（持久） | 同上 | `/steer` 把文本持久写进日志为 `user/message{steering:true}`（`handle_steer`），并从进程内 deque 喂 `SteeringHook`；**G5 把投递做成持久的**：`steps()` 在每一步之前 `claim(next-step)`，取到的行写成 `user/message{steering:true}` 并进入本轮请求，因此跨进程、跨重启都成立 | 已实现（记录与投递都持久） |
| `inject`=next-step 不 wake | 同上 | `Agent::enqueue(text, Target::NextStep)` 不调用 `kick()`：消息停在队列里，下一次 `kick()`/`run()` 才被取走 | 已实现（G3/G4） |
| `claim(target,turn)` 批量取 | `inbox.ts` | `crates/agent/src/inbox.rs` 的 `claim(session, target, &Inbox)` 每次取队头一行并写一条 splice 事件；批量由 `Agent::kick()` 循环 claim 完成（DSH 一次取全部 next-step + 一条 next-turn，效果相同，事件数不同） | 已实现（G3/G4，逐条 claim） |
| `cancel(cause,{keepInbox})` 保留队列 | `agent.ts` `cancel()` | `crates/agent/src/cancel.rs` 的 `Cancel` 带 `Cause{Unknown,User,Parent,Timeout,Hook,Disposed}`（H2 `5cc02ed` 之后的提交）：第一个原因胜出、写进 `turn/end.cancelledBy`，**落日志**而不只在内存——DSH 的 cause 只活在进程里，重启就没了。`keepInbox` 的语义天然成立：队列本来就在日志里，取消从不删除待办 | 已实现（H2，比 DSH 多一条持久记录） |

**代价（已核对源码后下调，比初稿轻）**：运行中追加的输入在 forge-rs 里**已经落日志**——见上行
`/steer` 与运行中 POST 的证据，进程重启后文本不丢，会作为上下文回到模型。真正的差异是结构性的：
DSH 有一个从日志 fold 出来的 next-turn / next-step **双队列**加 `claim` / `keepInbox` 语义，
forge 没有这一层，把"注入当前步"交给 `SteeringHook` 读进程内 deque。双队列能为"断点续接**正在跑的
那一轮**"提供更强保证，但那一轮本身随进程结束，收益有限，故不列为必须补的缺口。

---

## C. 扩展点（waterfall）

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| `emit/serial/waterfall` 三态分发器 | `core/agent/src/dispatch.ts` `AgentEventDispatch` | 两个面：改写面 `crates/agent/src/hooks.rs` 的 `Hooks`（三相位——`pre-step`/`request` 改写并写 `hook/rewrite`，`request-error` 决定是否重试）；观察面 `crates/agent/src/dispatch.rs` 的 `Dispatch`（`emit` 对每个已落盘事件调用全部监听器、不可否决；`serial` 在 turn 边界按注册序串行） | 已实现（G2 `09c255e` 后续提交） |
| `agent/pre-step` 可改写 messages 或 reject | `agent.ts` `preStep()` 派发 | `Hooks::run_pre_step` + `agent.rs step()`（改写 transient，不改 durable history） | 已实现（W5） |
| `agent/request` 可改本次请求 config（改 model 抛错） | `agent.ts` `step()` 派发 | `Hooks::run_request`（hook 不能改 model id，`run_request` 强制还原） | 已实现（W5） |
| `agent/request-error` 返回 `retry` 才重来 | `agent.ts` | `Hooks::run_request_error`：传输策略本要放弃时，钩子可答 `HookOutcome::Retry`（不改写、不记 `hook/rewrite`，重试体现为下一条 `llm/call`）；无钩子时行为不变 | 已实现（D 波 `01fd95b`） |
| listener 抛错不饿死后续（自建 emit 而非 map） | `dispatch.ts` `emit` | `Dispatch::emit/serial` 用 `catch_unwind` 逐个包住监听器，panic 记进 `Dispatch::problems()` 后继续下一个（2 个单测 + 1 个集成测试钉住）；`Hooks` 则相反——hook 出错即 `hook_failed` loud 终止该步（改写者必须响，观察者必须静） | 已实现 |

**代价**：DSH 能在运行时用插件替换模型适配器、工具、甚至 loop；forge-rs 只开放了三个
有界扩展点（`pre-step`/`request` 改写 + `request-error` 决定重试，见 `agent-gaps.md` §C 与
ADR-0004），其余 agent 行为仍须改代码重编译。换来的是「没有**未记录**的改写者」——每次改写都
落一条 `hook/rewrite`。**这是两者最根本的哲学差：可塑性 vs 可解释性。**

---

## D. 请求与流记账

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| attempt 级事务 `AssistantStreamAttempt`（attemptId） | `core/agent-loop/src/assistant-stream.ts` | 每次**未成为可见答案**的尝试落一条 `assistant/attempt`（`attempt`/`outcome`/`retryable`/`message`/`code`），成功那次仍只落一条 `assistant/message`（G1 `09c255e`）；没有 DSH 的 attemptId 对象，日志里用 `(turn,step,attempt)` 定位 | 已实现（G1） |
| `request/header`（reason: initial/resume/change/series） | `agent.ts` `buildRequest()` | `crates/agent/src/payload.rs` 的 `request_header(reason, model, messages, tools)`，`Agent::request_reason()` 给出四值之一（工具集合变化→`change`，`prompt_recorded`→`resume`）（G1） | 已实现（G1） |
| `request/context`（provider/model/contextWindow） | `packages/core/agent-loop/src/agent.ts` 的 `buildRequest` | `crates/agent/src/payload.rs` 的 `request_context`：`provider`/`model`/`messages`/`tools`/`temperature`/`maxTokens`/`stop`/`systemBytes`/`systemFingerprint`；H1 `request/context` 事件一行一请求 | 已实现（H1，比 DSH 多记采样参数与系统提示指纹） |
| 失败/取消/流错误落 `assistant/attempt`（**不进模型历史**） | `assistant-stream.ts` `settle()` | `Agent::record_attempt()` 在 `stream()` 的七个失败/取消分支写 `assistant/attempt`（`assistant/attempt` 不在 `to_llm_messages` 的可见类型里，因此永不喂回 provider）（G1） | 已实现（G1） |
| header 冻结防插件篡改（`deepFreeze`+`WeakSet`） | `agent.ts` `buildRequest()` | Rust 的所有权已经排除了「共享可变请求」这个前提：`Request` 按值传给 provider，hook 只拿到 `&[Message]` 且必须自己 clone 才能改。证据测试代替运行时冻结：`crates/agent/tests/agent.rs` 的 `a_transient_rewrite_does_not_reach_the_next_step`（改写只影响发出的请求，日志仍是操作者的原话）与 `an_earlier_request_is_a_snapshot_not_a_window_into_later_history`（前一个请求不会随后续步骤增长） | 已实现（H6，用测试而非 `Object.freeze`） |

**仍未拿到的**：token 级明细不在 attempt 行里（只有 `assistant/message` 带 usage），所以「第 2 次尝试花了多少 token」还要自己按 attempt 序号配时间戳推。事件顺序、失败原因、是否可重试都已在日志里。

---

## E. 子代理（缺口最大的一节）

DSH 的 `subagent/subagent` 有 22 个源文件；forge-rs 的 `crates/subagent/src` 现有 **15** 个
（`backend/cancel_file/catalog/child/continuation/descriptor/lib/list/lock/names/options/providers/runner/structured/tool`）——S1–S5 + W1 之后从 5 个长到 15 个。

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 容量槽 `ActivationPool.reserve(capacity)` | `subagent/src/activation.ts` | `runner.rs` counting `Semaphore{capacity,free}`，`Options.max_active`（默认 1=Go 串行；>1 可并发）；超限**排队**非拒绝 | 已实现（S2 `cec20ec`） |
| 进程外后端（another process 的子 agent） | `subagent/src/out-of-process.ts`、`manager.ts` 的 `registerProvider` | `crates/subagent/src/backend.rs`：`Backend` trait + `ProcessBackend`（任意程序，stdio 一行 JSON）；`crates/subagent/src/providers.rs`：`Providers` 具名注册表（重名拒 `Conflict`）；配置 `subagent.providers[{name,program,args,timeoutSecs}]` + `subagent.backend` 由 `crates/cli/src/wiring.rs` 注册，`forge subagent providers` 列出并探测程序是否在 PATH 上 | 已实现（S5 `cae8d74` + G7 可配置多后端） |
| `structured_output` 终结工具（子代理按 schema 返回） | `subagent/src/structured.ts` `STRUCTURED_OUTPUT_TOOL` | `crates/subagent/src/structured.rs`：`structured_output` 工具 + `GuardedTool` + 极小校验器；`ChildRun.structured` | 已实现（W1 `5343e28`） |
| 深度记账 `subagentDepth`（header 权威、只增不减） | `subagent/src/depth.ts` | `runner.rs` max_depth/depth 链，上限返回 `DepthExceeded`；`session/start` 写 `delegationDepth`，冷恢复按 header 取 `max(header, options)` 作权威下限（child.rs `reopen_child_log` → `ChildRun.depth` → `child_tools(child_depth)`） | 已实现 |
| 生命周期通知（scoped dispatch + 异常隔离） | `subagent/src/lifecycle.ts`、`continuation-messages.ts` | `crates/subagent/src/runner.rs` 的 `Runner::spawn`：子代理跑在后台线程，`leave_settlement` 把结算写成父日志的一条 `next-turn` inbox 消息（`crate::continuation::settlement_notice` 组装文本，含名字、最后一句、结构化结果）；槽位在**线程内** acquire，所以后台子代理与前台一样排队；`subagent` 工具加 `background: true` 暴露给模型，且与 `schema` 互斥（异步结果没有 tool/result 可落） | 已实现（G6，异步形态；无 scoped dispatch 那种订阅者隔离——那是 G2 `Dispatch` 的部分） |
| 子代理目录投影（chunked catalog，version 3） | `subagent/src/catalog.ts` `SUBAGENT_CATALOG_VERSION` | `crates/subagent/src/catalog.rs`（`subagent/catalog` 事件，父 loop 写）、`descriptor.rs`（子 runner 写）、`names.rs`、`list.rs` | 已实现（S1 `47118db`） |
| `subagent.prompt`（delivery: queue/steer）/ `subagent.interrupt` 控制接口 | `subagent/src/control.ts` | `prompt` 已实现：工具 `resume` 参数（S4）+ CLI `forge subagent prompt`（S5）；`interrupt` 已实现**进程内**（`runner.interrupt(child_id)` 取消该 child 注册的 cancel token）与**跨进程**（`forge subagent interrupt <child>` 写 `<dataDir>/sessions/<child>.cancel`，运行中的 `Runner` 轮询该标记并 trip 同一 token；worker 一次性进程无监听） | 已实现（进程内 + 跨进程标记） |
| 冷恢复 cold resume | `subagent/src/manager.ts` | `child.rs reopen_child_log` + `runner.resume`（跨进程靠日志，descriptor 必须 continuable） | 已实现（S3 `a42df34`） |
| 每 child 串行锁 `ChildLock.run` | `subagent/src/activation.ts` | `lock.rs` 的 `ChildLock.run(child_id, op)`（同一 child 的 resume 串行，避免 turn/start 与 turn/end 交错） | 已实现（S3 补齐） |
| 4 种后端（本地/外部产品委派等） | `subagent/src/`（provider 抽象） | `Providers` 注册表（注册/按名解析，重名 `Conflict`、空名 `Invalid`）+ `Backend` trait（唯一 `ProcessBackend`）；`backend_name` 未注册报 `NotFound` | 已实现（S2/S5） |
| 子代理树遍历 `listDescendants`（任意层，带 corrupt/unsupported/unavailable 诊断） | `subagent/src/list-children.ts` `listDescendants` | `crates/subagent/src/list.rs` 的 `descendants(data_dir, root)` 前序遍历、`Node{entry,path,depth,children,diagnostic}`、`Diagnostic{Corrupt,Unsupported,Unavailable}`、`render_tree`；CLI 是 `forge subagent tree <root>`。缺日志的子级保留为 `unavailable` 行而不是消失 | 已实现（2026-11-20） |
| 按名字解析到会话 `resolveAgentPath`（`lead/agent-1/agent-2`） | `subagent/src/list-children.ts` `resolveAgentPath`、`names.ts` `agentAddress` | `crates/subagent/src/list.rs` 的 `resolve_name(data_dir, root, name)` 逐段走 catalog，`lead/` 前缀等价；`names.rs` 补 `is_agent_path`/`agent_path_segments`；CLI 是 `forge subagent resolve <root> <name>` | 已实现（2026-11-20） |
| 归档祖先拒收 `archive-admission` | `subagent/src/archive-admission.ts` | `crates/subagent/src/archive.rs` 的 `is_archived`/`admit`：`session/archived` 事件 + `forge session archive`；runner 的两扇门（起子代理、续跑子代理）逐跳查祖先，命中即 `Refused` 并点名被归档的那个会话；已在跑的一轮不回头查 | 已实现（H5；比 DSH 多一条可回放的日志记录，少一个否决面） |
| `subagentTiming` 投影（`settledMs`/`active`/`lastTurnCompleted`） | `subagent/src/projection.ts` `subagentTimingProjectionDefinition` | `crates/subagent/src/timing.rs` 的 `Timing{child_id,settled_ms,active,last_turn_completed,turns}` + `of_log`/`of_child`/`children`（时钟倒退计 0、`session/end` 后 `active` 恒 false）；CLI 是 `forge subagent timing`，HTTP 是 `GET /api/sessions/{id}/subagents`（`crates/server/src/subagent_route.rs` 把它与 `list::descendants` 的树拼在一起），面板是 `web/src/components/SubagentPanel.tsx` | 已实现（2026-11-20，库 + CLI + HTTP + 面板） |
| `SubagentErrorCode` 词表（约 35 个具名错误码） | `subagent/src/error.ts` | `crates/subagent/src/error_code.rs` 的 `Code`（**28** 个变体）+ `ALL` + `from_kind`（9 臂穷尽、无 `_ =>`）+ `from_error`（先按文案、后按 kind）。DSH 35 个里 7 个在本 crate 无对应概念（`preset_*`、`model_selection_*`、`subagent_control_*`、`draining`），未硬造；`DUPLICATE_PROVIDER`→`provider_changed`、`LLM_PROVIDER_NOT_FOUND`→`no_provider` | 已实现（2026-11-20） |
| `interrupt` 的祖先权限校验（只允许 user 或同树 live ancestor） | `subagent/src/manager.ts` `interrupt(target, authority)` | `crates/subagent/src/authority.rs` 的 `Caller{User,Session}`/`Verdict{Allow,Deny}`/`check`/`ancestry`/`describe_ancestry`：禁止停自己、禁止停祖先、禁止跨树；链路读不出即拒绝（fail closed）；`MAX_ANCESTRY=64` 且遇环 `Refused`。CLI `forge subagent interrupt --as <id|user>`，默认 user。检查在写 marker **之前**。**仍缺** DSH 的 live 约束（已 `session/end` 的祖先仍算祖先） | 已实现（2026-11-20） |

**代价**：DSH 的子代理是一个能容纳多种后端、能观察失败、能按 schema 交付、能被外部 steer/
interrupt、能冷恢复的子系统；forge-rs 是一个刻意最小的递归装置——单并发、depth 链、结果从
子日志派生。这是「一个子系统」与「一个工具」的差距，也是这次清单里最该优先补的地方。

---

## F. 目标续跑

| 能力 | DSH 在哪 | forge-rs 现状 | 状态 |
|---|---|---|---|
| 事件驱动 round driver（inject agents/goals/sessions） | `goal/goal-round-driver/src/index.ts` | `crates/goal/src/runtime.rs` 的 `next(Snapshot{events,claim,running}) -> Next`：六值 `NoGoal`/`Ended`/`Running`/`Superseded`/`Busy`/`Open`，每次问都从日志重算，无 `DriverState`、无订阅、无 async | 已实现（H3，形状不同：函数代替监听器） |
| revision + CAS 并发防重 | 同上 `RoundIdentity{goalId,revision,round}` | `Goal.revision`、`RoundIdentity`、`CasOutcome{Written,Stale}`、`next_action_identity()`；CLI 续跑前 `append_continued_if_current` 只有 revision 一致才写 `goal/continued` | 已实现（W3 `d6c98b2`） |
| `agent/pre-step` waterfall 上校验续跑（race fence，每 checkpoint 重校验） | 同上 `validReservation()` | `forge_goal::fence(events,&Reservation)->FenceOutcome{Valid,Ended,Stale}`（纯比较，4 单测）；CLI `goalfence::hook` 给每个 goal round 挂 `pre-step` 钩子，每步重读日志比对 revision/round/status，失配则 `hook_failed` 中止本轮（2 端到端单测） | 已实现（per-step fence） |
| 运行中 pause/steer 打断（`goal/changed` → `agent.cancel`） | 同上 | `execute_goal` 接 Ctrl-C → `cancel_open_goal` 写 `goal/ended{reason:cancelled}`；per-step fence 也会在目标被外部结束时停轮。pause 已实现（Ctrl-C → `goal/ended{reason:cancelled}`）；**steer 已实现**（G5）：`steps()` 每步前从 `agent/inbox` 的 `next-step` 队列取消息，写成 `user/message{steering:true}` 并进入当轮请求，任何进程都可通过写队列行注入 | 已实现 |
| goal domain / fold / runtime 分层 | `goal/goal/src/{domain,fold,runtime,client}.ts` | `crates/goal/src/{domain,events,fold,runtime}.rs` 四模块：`domain` 纯数据、`events` 三个构造器、`fold` 折叠与栅栏、`runtime` 纯决策函数 `next(Snapshot) -> Next`。决策是函数不是状态机（DSH 是 async 有状态 driver），`crates/goal/tests/runtime.rs` 9 个测试用手搓 `Vec<Event>` 驱动整条生命周期 | 已实现（H3，纯函数而非有状态驱动） |
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
| projection 状态 | `team/agent-team/src/projection.ts` | `crates/team/src/view.rs` 的 `project(root)`：把 board + journal 折成一份 `View`（任务按 id 排序、每任务带 `waiting_on`/`missing` 与 `is_ready()`、`Counts` 三态计数、`leads` 每个 Lead 的条目数、board 自己报的 problems），`View::render()` 是稳定文本 | 已实现（G8） |
| task-view 呈现 | `team/agent-team/src/task-view.ts` | `forge team view <board>`（`crates/cli/src/teamcmd.rs`）打 `View::render()`，`--json` 给结构化形状；web `TeamBoards.tsx` 面板读 `/api/teams` | 已实现（G8，CLI 面） |
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
waterfall 插件式恢复（forge 是内建策略，不是可插拔监听器，但观察面 `Dispatch` 已提供）；image-offload 在此设计下无对象可卸。

---

## 汇总：按「该优先补什么」

| 优先级 | 缺口 | 状态 |
|---|---|---|
| ~~P0~~ | E 子代理：结构化输出、进程外后端、容量槽 >1、catalog、cold resume | **已交付** S1–S5 + W1（`47118db`/`cec20ec`/`a42df34`/`ea42128`/`cae8d74`/`5343e28`）+ S2/S3/S4 收尾（`maxActive`/`ChildLock`/`Providers`/`Delivery`/`interrupt`/`continuation`/`delegationDepth`） |
| — | B 输入队列 | **已交付**双持久队列 `agent/inbox` + `Agent::kick`/`enqueue`/`pending`（G3/G4）；`/steer` 与运行中 POST 早已持久落日志（`03ab35d`） |
| ~~P1~~ | F 目标续跑 revision/CAS + 打断 | **已交付** revision/CAS（W3 `d6c98b2`）+ per-step race fence（`forge_goal::fence` + `goalfence::hook`，B 波 `30e760c`）；steer 也已补（G5：`next-step` 队列在每步前被 claim 并记录） |
| ~~P1~~ | H 压缩 surfaceOp + pruner + 事务 | **已交付** surfaceOp 与 tool-result-pruner（W4 `6150b27`）+ 三事件事务/孤儿检测/summary-error 失败不隐藏（C 波）；image-offload 在此设计下无对象可卸 |
| ~~P2~~ | C waterfall 扩展点 | **已交付** 三相位 hooks（W5 `69aa3c9` + D 波）+ 通用 `Dispatch` 的 `emit`/`serial` 观察面（G2） |
| ~~P2~~ | G 团队依赖图 + journal | **已交付**（W6 `fedd8fe`） |
| — | A 的跨命令常驻宿主 | 已实现（H7：`forge web` 每会话持一个 `Agent`，工具/插件/MCP 只装配一次，跨轮复用到会话结束；H4 每轮后抽干 `agent/inbox`。**未**引入后台守护进程——跨 CLI 命令仍靠日志与 `forge-goal`/`forge-schedule` 另起一次 `run()`） |

---

## 这份文档没做的事

- 没动任何 Rust/web 代码——这是「记录缺失」，不是「补齐缺失」。
- 没把「刻意不做」（A 节、D 节的冻结、F 节的 steer）写成缺陷——那些是设计选择，写了代价供
  决策，不做冤枉账。
- DSH 侧部分行为从 README/注释推断，已逐条标注；未逐行验证的没有当作已确认事实。

---

## 复核（2026-11-20，逐符号验证）

方法：对本文声明的每一项，在 `crates/**/src` 里找出**承载它的符号**（不靠提交信息或文档自述），
再用 `cargo test --workspace` + `check.ps1` 证明它还能跑；找不到符号的一律记为未实现。

**事件类型**：`EventType::ALL` 现有 29 项，其中 `request/header`、`assistant/attempt`（G1）、
`agent/inbox`、`agent/status`（G3/G4）、`subagent/catalog`、`subagent/descriptor`（S1）、
`compaction/start|end|summary-error`（C 波）、`hook/rewrite`（W5）、`context/compressed`（W4）均在册。

| 本文声明 | 承载符号 | 结论 |
|---|---|---|
| 容量池可配 | `runner.rs` `Semaphore{capacity,free}`、`Options.max_active`、配置 `subagent.maxActive` | 存在 |
| 进程外后端可配多份 | `backend.rs` `ProcessBackend`、`providers.rs` `Providers`、配置 `subagent.providers[]` | 存在 |
| 结构化输出 | `structured.rs` `STRUCTURED_OUTPUT_TOOL`、`GuardedTool` | 存在 |
| 深度 header 权威 | `child.rs` 写/读 `delegationDepth`、`runner.rs` `child_tools(child_depth)` | 存在 |
| 异步 settled | `runner.rs` `spawn` + `leave_settlement`、`continuation.rs` `settlement_notice` | 存在 |
| 冷恢复 | `child.rs` `reopen_child_log`、`runner.rs` `resume` | 存在 |
| per-child 串行 | `lock.rs` `ChildLock::run` | 存在 |
| 跨进程中断 | `cancel_file.rs` `request`、`runner.rs` `watch_cancel`、CLI `forge subagent interrupt` | 存在 |
| 常驻 phase 机 | `agent.rs` `PHASE_IDLE/RUNNING/MAINTENANCE`、`kick`、`when_idle`、`run_maintenance` | 存在 |
| 持久双队列 | `inbox.rs` `Target::{NextTurn,NextStep}`、`splice/claim/fold` | 存在 |
| 通用监听面 | `dispatch.rs` `Dispatch::{emit,serial}` + `catch_unwind` | 存在 |
| steer | `agent.rs` `steps()` 每步前 claim `NextStep` | 存在 |
| 目标 revision + 每步 fence + 纯决策 driver | `goal/src/{domain,fold,runtime}.rs` 的 `RoundIdentity`/`CasOutcome`/`next`、`goalfence` | 存在 |
| 压缩事务 | `session/src/compact.rs` 三事件 + `open_compaction`、`context/src/wire.rs` `surfaceOp`、`prune.rs` | 存在 |
| 团队依赖图 / journal / 投影 | `team/src/{task-graph,journal,view}.rs` | 存在 |

**结论**：G1–G8、S1–S5、W1–W6、A–E 各波声明的东西都能指名到符号。
但「与 DSH 结构等价」只对那八项成立；E 表末尾六行是本次复核新记的真缺口，
其中 5 项已于 2026-11-20 补齐（树遍历 / 名字解析 / 时间投影 / 错误码词表 / interrupt 权限），
归档项已补齐（`session/archived` + `forge session archive` + 祖先拒收），不再有「刻意不做」的归档缺口。
