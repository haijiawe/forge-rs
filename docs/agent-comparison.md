# DSH 与 forge-rs 的 Agent 层专项对比

> **谱系先读**：DSH（TypeScript）→ `forge/`（DSH 的 fork，改名换皮）→ `forge-go`（Go 实现，
> 2026-11-17 删除）→ `forge-rs`（Go 树的 Rust 移植）。`forge-rs` 的 `AGENTS.md`/`README.md`
> 首句都写「Rust rewrite of a **Go** implementation」。**所以 forge-rs 不是 DSH 的重写**，
> 它继承的是 Go 相对 DSH 的取舍。两者 agent 逻辑不同，源于 Go 树当年的独立演化，**不是移植
> 出错**。要按「DSH 有、forge-rs 缺」逐项对账，看 [agent-gaps.md](agent-gaps.md)。
>
> 范围：只对比 **Agent 执行层**——循环形态、输入队列、扩展点、请求与流记账、取消语义、
> 子代理、团队与目标、上下文压缩。模型适配、工具清单、沙箱、UI、规模等见
> [dsh-vs-forge.md](dsh-vs-forge.md)。
>
> 口径：原版侧是 `deepseek-harness-master/`（`@deepseek-ai/dsh-root` **0.2.2-rc.5** 源码，
> 目录无 `.git`，给不出 commit）；forge-rs 侧是 `forge-rs/`（Rust，HEAD 以仓库当前状态为准）。
> 结论全部来自源码，不来自文档或性能数字；rc.5 与早先性能实测所用的 rc.1 只差一个
> rc，架构层的判断不受影响。

---

## 1. 一句话结论

**DSH 的 agent 是一个"始终活着、可被外部改写"的状态机；forge-rs 的 agent 是一个
"每次调用跑完即弃、从日志重新派生"的纯函数。**

这是两条各自演化出来的实现（DSH→forge-go→forge-rs）在 agent 层的设计差，不是谁移植
错了谁。差别集中在：

- DSH：agent 是一个**有生命周期的服务**。它常驻、有 phase、有可插拔的输入队列、
  有能被插件拦截和改写的请求管线。Loop 本身就是一个可替换的插件（`ctx.agentLoop`）。
- forge-rs：agent 是一次**确定性的计算**。`run(text)` 读日志、发请求、写日志、返回；
  没有常驻对象，没有能改写它的 listener，同样的日志必然产生同样的请求。

**往下每一节是"差异描述"；"DSH 有而 forge-rs 缺什么、怎么补"另见
[agent-gaps.md](agent-gaps.md)。**

---

## 2. 循环形态：常驻状态机 vs 单轮纯函数

| | DSH `ReactLoopAgent` | forge-rs `Agent::run` |
|---|---|---|
| 存在形式 | 常驻对象，实现 `Agent`，有身份 `id: SessionId` | 每次调用新建的 `Agent{opt, cur_turn, cur_step}`，调用完即弃 |
| 状态机 | `idle \| maintenance \| running`，`setPhase` 时 emit `agent/status` | 无 phase；只有两个原子计数器 |
| 驱动方式 | `kick()` = `while (await this.turn()) {}`；`whenIdle()` 等活动稳定 | 一次 `run()` 驱动一个 user turn，跑完返回 |
| 维护任务 | `runMaintenance(job)` 在 idle 跑，**不进模型** | 无独立维护通道 |
| 源码 | `deepseek-harness-master/packages/core/agent-loop/src/agent.ts`（751 行） | `forge-rs/crates/agent/src/agent.rs`（790 行） |

**后果**：DSH 可以在回合之间做后台维护、接受 wakeup、被外部驱动；代价是必须处理
phase 竞争、activity 生命周期、dispose。forge-rs 完全没有这类状态，代价是"回合之间"
这个概念在它的模型里不存在——要做定时/续跑只能在回合外另起一次 `run()`（这正是
`forge-goal` 与 `forge-schedule` 的做法，见第 8 节）。

---

## 3. 输入：持久 inbox 双队列 vs 无队列

这是两版 agent 最实质的结构差别。

**DSH**：输入是一个**从日志 fold 出来的持久状态**。
`packages/core/agent-loop/src/inbox.ts`（232 行）定义 `inboxProjectionDefinition`，
key=`inbox`、stateVersion=1，从 `agent/inbox/spliced` 事件重建；它校验 splice 的
start/removedCount 范围与 message id 全局唯一，坏 splice 直接抛错。`ReactLoopInbox`
实现 `InboxContract`，有两条队列：

- `next-turn`：下一轮才消费；
- `next-step`：插入当前回合的下一步。

三种投递语义都由 `send(message, target, wakeup)` 表达：
`followup` = next-turn + wake，`steer` = next-step + wake，`inject` = next-step 不 wake。
`claim(target, turn)` 先取走全部 next-step，target 为 next-turn 时再取一个 next-turn。
`cancel(cause, {keepInbox})` 还能决定取消时是否保留队列。**队列是 durable 的**——进程
重启后从日志重建，不丢。

**forge-rs**：**没有 inbox**。用户的 prompt 由 HTTP 层直接写成 `turn/start` +
`user/message` 落进日志；`Agent::run(text)` 收到的 `text` 就是这一次要处理的输入。
`Options.prompt_recorded` 为 true 时用 `open_turn(&snap)` 采纳 HTTP 层已经写下的 turn，
避免重复写。`steering` 是一个 `SteeringHook` 闭包，`steps()` 在每步之后轮询它，
拿到的字符串**只注入内存里的 history，不写任何事件**（`agent.rs` 的 `steps()`）。

**后果**：

| 能力 | DSH | forge-rs |
|---|---|---|
| 运行中追加下一条输入 | 持久队列，重启不丢 | steer 仅内存，进程一停就没了 |
| 注入在下一轮 vs 下一步 | `next-turn` / `next-step` 两队列 | 无此区分 |
| 谁写 user/message | agent 的 inbox | HTTP 层（CLI/Web） |
| 取消时保留待办 | `keepInbox` 选项 | 无队列可保留 |

一句话：DSH 把"待办输入"当作一等持久数据，forge-rs 把"输入"当作一次调用的参数。

---

## 4. 扩展点：waterfall 拦截 vs 只读投影

**DSH** 有一个专门的三态分发器（`packages/core/agent/src/dispatch.ts`，176 行）：

```ts
export interface AgentEventDispatch { emit; serial; waterfall }
```

- `emit`：非否决。fused dispatcher `agentEvents()` 把 agent subject 注入每个 payload；
  它**自己遍历 callbacks**，把同步 throw 与 promise reject 都 contain 掉（log warn），
  不饿死后续 listener——因为 Cordis 原生 emit 用 `Array.map`，一个抛错会截断后面所有
  监听器，所以这里刻意自建。
- `serial`：有序 awaited，用于 `agent/turn-stopping` 这类必须在收尾时串行的钩子。
- `waterfall`：环绕式，listener 必须调用 `next()`。三个关键扩展点都是 waterfall：
  - `agent/pre-step`：`preStep()` 里派发，默认返回 `enter{decision}`。插件可以改写 messages，
    也可以返回 `reject` —— agent 随即 dispose toolSet 并拒绝这一步。**goal 续跑就是挂在这里
    实现的**（见第 8 节）。
  - `agent/request`：`step()` 里派发，可改本次请求 config；但**改 provider/model/reasoningEffort
    会抛错**，要求先走 `agent.model.select()`——防止插件绕过模型选择的状态记录。
  - `agent/request-error`：失败时派发，携带 `{failure, retryPolicy}`，插件返回 `retry` 才重来。

**forge-rs** 没有 listener 层。它延伸事件模型（`events-v1.md`，只追加）与只读投影
（`forge-session` 的 `Projection::project`）——事件是**诊断记录**，不是可被监听改写的
钩子。没有任何插件能在请求发出前改掉它。扩展靠三条静态通道：

1. **回调 hooks**：`TextHook`/`DeltaHook`/`SteeringHook`/`AttachmentHook`/`HiddenHook`/`Clock`/
   `Cancel`，全部在 `Options` 里，编译期定型；
2. **工具注册表**：`Arc<dyn ToolRunner>` + `forge-tool` 的 `Registry`；
3. **接线**：`crates/cli/src/wiring.rs` 在启动时一次性把插件/MCP/技能/团队工具接进来。

**后果**：DSH 能在运行时用配置替换模型适配器、工具注册、甚至 agent loop 本身——这是它的
"everything is a plugin"（`docs/architecture.md`）。forge-rs 要改变 agent 行为必须改代码、
重新编译；换来的是**没有隐藏的改写者**：拿到一段日志，你能确定每个请求就是投影出来的那个
样子，不会被某个运行时插件中途改过。

---

## 5. 请求与流记账：attempt 级事务 vs 聚合后单条落盘

**DSH** 把"一次模型调用"当成一个有完整生命周期的事务：

- `buildRequest()` 先落三条日志：`request/header`（reason ∈ `initial|resume|change|series`）、
  `request/context`（provider/model/contextWindow/systemPromptUpdate）、工具增删时再落
  `developer/message`。header 用 `deepFreeze` 冻结、messages 记进 `frozenMessages: WeakSet`、
  boundary messages `Object.freeze` —— 防止后续插件篡改已发出的请求。
- `AssistantStreamAttempt`（`assistant-stream.ts`，141 行）给每次 attempt 一个
  `attemptId = ${sessionId}:${attempt}`，有 `start/push/settle/abandon`。`settle()` 先 append
  事件再 emit `end{outcome:'committed', eventType, seq}`，失败则 `abandon{outcome:'abandoned'}`。
- 成功落 `assistant/message`；失败/取消/流错误落 `assistant/attempt`（**不进模型历史**）。
  有可见前缀的中断落 `assistant/message{interrupted:true}`。因此 token、usage、原始输出、
  失败原因都能从日志重建。

**forge-rs**：`step()` 落 `llm_call(model, messages.len(), names)`，然后 `stream()` 用一个
聚合器收集 delta，`consume()` 逐片触发 `on_text`/`on_delta`，最后写**一条** `assistant/message`
（`{text, stopReason, durationMs, usage?, partial?}`）。重试在 `stream()` 内部：
`for attempt in 0..=max_retries`，但**只在已收到的 text 为空时才重试**——一旦写出过文本片段
就不再重来，否则会重复已经发给用户的字。`wait_retry` 按 `Retry-After` 分片 sleep 10ms 并
持续检查 cancel。

**后果**：DSH 能回答"这次调用试了几回、每回 token 多少、为什么失败"；forge-rs 的日志
只记最终结果和一次重试计数，更省、更确定，但拿不到 attempt 级的重放细节。

---

## 6. 取消与停止语义

| | DSH | forge-rs |
|---|---|---|
| 取消原因 | 联合类型 `user \| parent \| disposed \| hook`，另带 `keepInbox` | `Cancel`（原子标志），无原因分类 |
| stop reason | `completed \| max-tokens \| aborted{cause} \| error{failure} \| blocked`；max-tokens 是 **sticky**（后续正常 step 不降级） | `STOP_*`：`end_turn/tool_use/cancelled/max_tokens/error`；turn reason `TURN_COMPLETED/CANCELLED/ERROR`（`payload.rs`，131 行） |
| 词表归一 | 由 provider stop 词映射到 harness 词（`normalize_stop`） | 同（`normalize_stop`），另有 `stop_with_calls`：有 calls 且 end_turn → tool_use |
| 工具调用恢复 | `ToolCallRecovery` 观察 session/event，在失败时补 `tool/result` | 无独立恢复器；`run_tools()` 串行写结果，缺失靠 provider 侧约束 |

DSH 的 `blocked` 和一整套取消原因，配合 waterfall，使插件能"以外科方式"停住某个 agent；
forge-rs 的取消是二值的，够用但不表达"谁、为什么停"。

---

## 7. 子代理：ActivationPool vs 有界池（2026-11-20 已补齐，见本节末的「补齐后」）

**DSH**（`packages/subagent/subagent/src/activation.ts`，184 行及同目录 25 文件）：
`ActivationPool.reserve(capacity)` 用 `Set<symbol>` 占位，超限抛
`SubagentError('subagent limit reached…', 'ACTIVATION_LIMIT_REACHED')`。activation 分
**local / external** 两类：local 有 handle/outputStart/failureAt/poke；external 有
run/controller 且**不支持 follow-up**（`requireLocalActivation` 报 `NOT_CONTINUABLE`）。
配套 `observeLocalFailure`（agent/error 记 failureAt，turn/end 越过则清除）、
`captureLocalResult`（从 outputStart 起做 snapshot）、`ChildLock.run(childId, op)`（每个
子 agent 一条 promise 链串行）。有 **4 种后端**，从本地子 agent 到外部产品委派，接口统一
`startActivation`。

**forge-rs**（`crates/subagent/src/runner.rs`，360 行）：模块头一句
`one serial slot, one tool, fresh child logs`。`Semaphore` 用 `Mutex<bool>` + `Condvar`，
acquire 时每次唤醒都重查 `Cancel`，无轮询 sleep。`run()`：
open_child_log → child_tools（父工具按名包成 `AdaptedTool`，并在 depth+1 处挂嵌套 subagent
工具；policy/limits 继承，**approver 从不继承**）→ `forge_agent::Agent::run(prompt)` →
从子日志派生 turns/text → write_end(reason=completed|aborted)。工具在 depth 上限时返回
`DepthExceeded` 错误码，且**永不注销**。子代理是独立 session log，结果从日志派生。

**后果（写作时的判断）**：DSH 的子代理是一个可容纳多种后端、能观察失败、能串行同 child 操作的
子系统；当时 forge-rs 是一个刻意最小的递归装置——单并发槽、depth 链、结果落日志。

### 补齐后（2026-11-20，S1–S5 + G6/G7 之后）

上面的对比是本文件写作时的快照，已被后续八项补齐改变。现在 `crates/subagent/src` 是
**15 个文件 3,726 行**（`backend/cancel_file/catalog/child/continuation/descriptor/lib/list/lock/
names/options/providers/runner/structured/tool`），DSH 是 22 个文件 4,784 行——**源行数 0.78×**，
不是当年那个数量级差。逐项对应：

| DSH | forge-rs |
|---|---|
| `ActivationPool.reserve(capacity)` 超限抛 `ACTIVATION_LIMIT_REACHED` | `Semaphore{capacity,free}` + `Options.max_active`，超限**排队**（同步模型下排队比拒绝更诚实） |
| `manager.registerProvider` + 4 种后端 | `backend.rs` `Backend` trait + `ProcessBackend`、`providers.rs` `Providers` 注册表、配置 `subagent.providers[]` + `subagent.backend` |
| `catalog.ts` 父目录投影 / `descriptor.ts` 子身份 | `catalog.rs` + `descriptor.rs`（版本 3，descriptor first-wins） |
| `ChildLock.run` 每 child 串行 | `lock.rs` `ChildLock::run` |
| 冷恢复 `create`/`resume` | `child.rs reopen_child_log` + `runner.rs resume`（跨进程读日志） |
| `subagent.prompt`(queue/steer) / `subagent.interrupt` | `resume` 参数 + `forge subagent prompt`；进程内 `runner.interrupt` + 跨进程 `cancel_file` 标记 |
| `lifecycle.ts` 生命周期通知 | `Runner::spawn` + `leave_settlement` 把结算写进父的 `next-turn` 队列 |

**仍然没有（2026-11-20 复核）**：`listDescendants` 树遍历与诊断行、`resolveAgentPath`
（`lead/agent-1/agent-2` 反查 session id）、`archive-admission`、`subagentTiming` 时间投影、
约 35 个具名 `SubagentErrorCode`（我们只有 4 个 `CODE_*`）、`interrupt` 的祖先权限校验。
详见 [agent-gaps.md](agent-gaps.md) 的 E 表末尾六行与「复核」节。

---

## 8. 团队与目标：事件驱动 race-fence vs 纯函数决策 + CLI 循环

**团队**：DSH 的 `packages/team/agent-team` 有 task-board / task-graph / task-view /
journal / projection 等一整套；forge-rs 的 `crates/team/src/run.rs`（670 行）把并发收敛成
四条硬规则：① 所有线程在 `std::thread::scope` 内 join（panic 只影响本 worker）；② panic 用
`catch_unwind` 计为 failed worker 并重开 task；③ report 有序（worker 按名排序、problems
排序去重）；④ **claims 在任何 worker 启动前按名字顺序发生**，靠 board 的 CAS 排除其他
进程，使 owner 是 plan 的属性而非线程调度的结果。并发上限由 permit 控制（worker 各一线程，
进 runner 前等 permit），是 forge-subagent 单槽信号量的 N 倍推广。

**目标续跑**是两者哲学差异最集中的地方：

- **DSH** `packages/goal/goal-round-driver/src/index.ts`（484 行）是一个**事件驱动的插件**，
  inject `['agents','goals','sessions']`。它给每个 agent lifecycle 维护 `DriverState`，监听
  `agent/status(idle)`、`goal/changed`、`agent/inbox/*`、`session/event` 等，用
  `RoundIdentity{goalId, revision, round}` 做身份。它在 `agent/pre-step` waterfall 上校验续跑
  消息（`validReservation`：fiber active、!stopping、attempt claimed、!stale、同 round、
  goal active+armed、source.round===roundsStarted+1）；无效则 reject 并恢复其他 claimed
  消息，有效则 `next()` 并在**每个 checkpoint 后重新校验**（race fence）。到
  `maxGoalRounds` 时 block('round-limit')。
- **forge-rs** `crates/goal/src/lib.rs`（304 行）把决策收进一个**纯函数**
  `next_action()`：`if self.rounds + 1 >= self.max_rounds { Stop } else { Continue }`。
  `max_rounds` 是**轮**预算（`goal/created` 就是第 1 轮），`rounds` 只数续跑次数——注释里
  专门写了"比 rounds 会多花一轮"。三个事件构造器 `goal_created/continued/ended`，
  `fold(&[Event])` 取最近 created、忽略异 goalId、跳过坏 payload。续跑循环不在 agent 层，
  而在 CLI 层（`forge chat --goal`）。

**后果**：DSH 的续跑能在任意时刻被事件打断、重校验、恢复；forge-rs 的续跑是一个可读的
确定性决策，配合"日志是唯一真相"，重启后从日志 fold 就能知道 goal 状态（`/api/sessions/
{id}/goal` 路由正是这么做的）。同样地，DSH 用 revision + CAS 防并发，forge-rs 用"决策集中
在纯函数、副作用集中在 CLI"防并发。

---

## 9. 上下文压缩：三事件锁 + surfaceOp vs 单事件区间隐藏

**DSH**（`packages/compaction/`，README.zh.md:95-97/142）把压缩设计成一个**可选插件实现的事务**：

- `SurfaceEventType` 是封闭联合——`user/message`、`assistant/message`、`tool/result` 必须带
  `surfaceOp`，其他事件禁止带，因此 `compaction/*` 永远上不了模型可见的"表层"。
- 成功运行的日志序列：`compaction/start`（仅日志，取锁）→ 摘要该范围 → `compaction/summary`
  （仅日志，记 rawOutput/usage）→ 一条带 `surfaceOp: {op:'replace', startSeq, endSeq}` 的
  `user/message` 替换选中范围（**唯一的表层变更**）→ `compaction/end`（仅日志，释放锁）。
- `start` 与 `end` 之间崩溃会留下**可检测的孤儿锁**，而不是一个假装成功的 end；active lock
  不能跨 `turn/start`/`turn/end`。`deriveMessages()` 把摘要渲染成 user 消息 + 保留节点，
  被遮蔽的事件仍留在原始日志，**回放是确定的**。
- `compaction/summary-error` 是 waterfall，让 image-offload 等插件请求持久的输入恢复——
  监听器只有先记录进展才返回 true；后端重试前重新派生并重计价。

**forge-rs**（`crates/context/src/wire.rs`，110 行）：压缩是**一条事件**
`context/compressed{blockId, from, to, summary, messages?}`，按 `seq` 闭区间寻址，payload
**不复制原文**（原文已在日志前几行）。恢复侧 `history.rs` 的 `closed_hidden_set()` 做两遍
配对闭合——隐藏一次工具调用必须带走它的结果，隐藏结果也要带走调用，否则会发出"有 tool
消息却没有 call"的请求被 provider 拒。`hidden_hook` 在区间构造时读一次；**读不到日志就不
隐藏**——因为隐藏是丢信息，失败方向必须是"多给"。

**后果**：DSH 的三事件锁 + surfaceOp 能精确表达"哪里被替换、替换前后是什么、事务是否
完整"，支持摘要失败后按需恢复、支持 image offload 插件；代价是复杂的锁语义与孤儿锁治理。
forge-rs 用一条事件 + 隐藏区间做到同样的"少发旧消息"，失败方向安全，但表达力有限——
没有三事件事务、没有 surfaceOp 改写、没有摘要失败恢复协议。

---

## 10. 共同的底座

抛开所有差别，两版在三条原则上完全一致——这也是 forge-rs 能被称作"重构版"的理由：

1. **每个请求都从日志派生**。DSH 的模块注释原话："Every request is derived from the
   session log"；forge-rs 的 `history.rs`："there is exactly one copy of the truth — the
   append-only log"。
2. **未知类型是数据，未知版本是错误**。DSH 的 session-format v0→v4 迁移链把旧格式当数据
   逐步迁移；forge-rs 单 v1 但 `EventType::ALL` 加类型、`validate` 对未知类型宽容、对未知
   版本 fail closed。
3. **模型可见 ⟺ 落日志**。DSH 用 `surfaceOp` 类型约束把这条规则编进类型系统；forge-rs 用
   "loop 是唯一事件写入者 + 工具只回 `result.meta`"的约定维持它。

---

## 11. 各自付出的代价 / 该怎么选

| 维度 | 选 DSH | 选 forge-rs |
|---|---|---|
| 运行时可塑性 | 插件可拦截/改写请求、替换 loop、挂接续跑 | 行为固定在代码里，改就要重编译 |
| 输入语义 | 持久双队列、wake/followup/steer/inject 齐全 | 无队列，steer 仅内存 |
| 可观测性 | attempt 级记账、三事件压缩事务、完整取消原因 | 最终结果 + 只读投影，日志更小更确定 |
| 并发安全 | revision + CAS + race fence（复杂但通用） | 纯函数决策 + 单点接线 + 单槽/permits（简单但固定） |
| 心智负担 | phase 机、waterfall、锁、孤儿、dispose | 一次调用、无隐藏改写者、可逐行读懂 |
| 规模化成本 | rc.5 源码约 155 万行 / 393 个 package | 6.7 万行 Rust 源 + 1.9 万行测试 / 24 crate |

一句话：**要"能被配置改写一个产品的每一层"，DSH 的设计胜出；要"每个请求都能被逐行解释、
行为不会被运行时偷偷改掉"，forge-rs 的设计胜出。** agent 层的全部差异，都是这一选择的
下游结果。

---

## 12. 证据索引

**DSH**（相对 `deepseek-harness-master/`）
- `packages/core/agent-loop/src/agent.ts`（751 行）—— phase 机、send/cancel、preStep/turn/step、buildRequest
- `packages/core/agent/src/dispatch.ts`（176 行）—— emit/serial/waterfall 三态分发器
- `packages/core/agent-loop/src/inbox.ts`（232 行）—— 持久 inbox projection 与双队列
- `packages/core/agent/src/types.ts`（123 行）—— Agent/InboxTarget/ModelSelection 类型
- `packages/core/agent-loop/src/assistant-stream.ts`（141 行）—— AssistantStreamAttempt
- `packages/subagent/subagent/src/activation.ts`（184 行）—— ActivationPool、local/external、ChildLock
- `packages/goal/goal-round-driver/src/index.ts`（484 行）—— 事件驱动续跑、race fence
- `packages/compaction/compaction/README.zh.md:95-97,142` —— surfaceOp 与三事件事务
- `docs/architecture.md` —— everything-is-a-plugin / ctx.* seam

**forge-rs**（相对 `forge-rs/`）
- `crates/agent/src/agent.rs`（790 行）—— Agent::run、hooks、steps/step/stream/run_tools
- `crates/agent/src/history.rs`（153 行）—— to_llm_messages_hiding、两遍配对闭合
- `crates/agent/src/payload.rs`（131 行）—— STOP_*、normalize_stop、usage_payload
- `crates/subagent/src/runner.rs`（360 行）—— 单槽 Semaphore、depth 链、Approver 不继承
- `crates/goal/src/lib.rs`（304 行）—— next_action()、轮预算语义、fold
- `crates/team/src/run.rs`（670 行）—— scope/catch_unwind/CAS claims/permits
- `crates/context/src/wire.rs`（110 行）—— context/compressed 事件
- `crates/cli/src/wiring.rs` —— 静态接线（插件/MCP/技能/团队工具）
