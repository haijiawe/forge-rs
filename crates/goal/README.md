---
dir: crates/goal
layer: L2
role: goal state folded out of the session log, and the continuation decision
public: [Goal, Status, Action, StopReason, RoundIdentity, Reservation, CasOutcome, FenceOutcome, fold, fence, check_in, CheckIn, fold_and_decide, validate, goal_created, goal_continued, goal_ended, continuation_prompt, CONTINUATION_PROMPT, DEFAULT_MAX_ROUNDS, next, Next, Snapshot, reservation_to_open]
may_import: [hearth-core]
must_not:
  - own a timer, a thread or a turn loop
  - call a model
  - decide anything the log cannot justify
gate: cargo test -p hearth-goal --offline
---

# hearth-goal

## 做什么

目标（goal）是操作者说"继续做，直到这件事完成"。本 crate 把目标的**状态**变成会话日志里的三条事件（`goal/created`、`goal/continued`、`goal/ended`），并用 `fold` 从事件重新算出当前目标。因此"重启后目标还在"不是一个额外功能，而是"状态只在日志里"的必然结果——进程里没有任何字段是真相。

它还回答一个别的问题：**还要不要再跑一轮**。`Goal::next_action` 返回 `Continue { round }` 或 `Stop { reason }`；上界（`max_rounds`）由这里而不是调用方检查，这样"续跑有界"是可以脱离 turn loop 单独测试的性质。

`goal/continued` 在那一轮**开跑之前**写。代价昂贵的东西要被记在账上，而不是等它花完了才记。

## 三层

H3 起分成三块，各管一件事：

- `domain` — 目标是什么，日志允许用哪些词（`Goal`/`Status`/`StopReason`/`RoundIdentity`/`Reservation`/`Action`）。纯数据，无 I/O、无时钟。
- `fold` — 日志折成状态（`fold`），以及对那状态做的竞态栅栏（`fence`/`check_in`）。只吃 `&[Event]`。
- `runtime` — **纯决策函数** `next(Snapshot { events, claim, running }) -> Next`。

`Next` 的六个值就是"宿主该做什么"：没有目标（`NoGoal`）、目标已终结（`Ended`）、自己那轮还活着（`Running`）、被别的进程抢先（`Superseded`）、宿主正忙（`Busy`）、可以开下一轮（`Open`）。

**为什么是函数而不是状态机。** DSH 的 round driver 是异步、有状态的监听器：每个 agent 一份 `DriverState`，订阅十来个事件，在回调里做决定——那是插件运行时逼出来的形状。这里同一份信息是"日志 + 宿主是否在跑"的函数值：测试可以把整条生命周期喂进一个 `Vec` 跑完，宿主也不会因为回调触发两次就多花一轮（每次问都是重算）。

**`Superseded` 不等于 `Ended`。** 另一个进程已经推进了这个目标时，本地这一轮必须让路，但**不能**替对方写 `goal/ended`——那会把一个还在进行的目标关掉。这是分层之后才写得清的一条规则。
## 公开面

- `Goal { id, text, max_rounds, rounds, status }`，`Goal::new`，`Goal::is_open`，`Goal::next_action`。
- `Status`（`active`/`complete`/`blocked`/`cancelled`/`exhausted`）、`StopReason`（后四种）、`Action`。
- `fold(&[Event]) -> Option<Goal>`：**最后一条** `goal/created` 胜出；`goalId` 不匹配的尾随事件被忽略；读不懂的 payload 跳过而不报错。
- 事件构造：`goal_created`、`goal_continued`、`goal_ended`（都返回 `Event`，`session` 由调用方给，`seq`/`id`/`ts` 留给 `Store::append` 填）。
- `continuation_prompt(&Goal)` 与模板常量 `CONTINUATION_PROMPT`（`{goal}` 占位，替换后即提示词）。
- `validate(&Goal)`：空文本、空 id、`maxRounds == 0` 在写入前就被拒，并说清是哪一条。

## 依赖与边界

只依赖 `hearth-core`（`Event`/`EventType`/`Error`）与 `serde_json`。不依赖 `hearth-session`：折叠只需要 `&[Event]`，让本 crate 依赖日志存储会把"读一份事件"和"拥有一份存储"绑死，而测试、导入器和未来的迁移都只想要前者。

不读时钟、不起线程、不调模型。所有需要"现在"的东西都在调用方。

## 不要做

- 不要在这里跑那一轮：本 crate 产出决定和事件，turn loop 才是执行者。把循环放进来会让"有界"和"日志可解释"同时失去测试点。
- 不要用进程内变量缓存目标状态。缓存一出现，"重启还能恢复"就从性质退化成巧合。
- 不要把模型的自我评价直接写进 `status`：`blocked` 必须由人（或 `maxRounds`）判定，否则模型说一句"我被卡住了"就能结束一个还能继续的目标。
- 不要为了新类型升 `v`：`AGENTS.md` 法则 #2 是"未知类型是数据、未知版本是错误"，加法不需要版本号。