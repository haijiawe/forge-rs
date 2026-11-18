---
dir: crates/team
layer: L2
role: parallel workers over a shared task board with a per-worker mailbox
public: [Board, NewTask, Task, TaskPatch, Status, Problem, ProblemKind, ScopeWarning, Message, Drain, Mailbox, Plan, Report, TaskCounts, WorkerReport, WorkerSpec, WorkerResult, WorkerRunner, Call, RecordingRunner, CommandRunner, run, valid_name, MAX_NAME_LEN, BOARD_FILE, BOARD_VERSION, INBOX_DIR, WORKERS_DIR, DEFAULT_MAX_PARALLEL]
may_import: [forge-core, forge-exec]
must_not:
  - write session events or open a session log for writing
  - depend on an LLM, a provider, a network client or an async runtime
  - hold a lock, a permit or a thread across the end of `run`
  - treat a write-scope warning as a lock
gate: cargo test -p forge-team --offline
---

# forge-team

## 做什么

一个 `subagent` 调用只能串行地委托**一件**自足的事。专家团队是另一种形状的工作：几个 worker 同时跑，它们不能抢同一件任务，要能互相留话，最后调用方要拿到**一份**确定的报告而不是 N 份交错的日志。本 crate 就是这三件事：

- **任务板** `<root>/board.json`。每次状态迁移（`claim`/`complete`/`reopen`/`edit`/`set_dependencies`/`reassign`）都要求调用方带上 `expected_revision`，不匹配就是 `conflict`。比较并交换（CAS）才是重点：两个 worker 认领同一件任务，是本 crate 存在的理由，而单靠一把锁只会让后到的认领覆盖先到的。
- **邮箱** `<root>/inbox/<worker>.jsonl`，只追加，每行一条 JSON 消息，按 worker 各有一个游标。worker 没在跑也有邮箱，所以"先发消息、稍后启动"是正常的，不是特例。
- **调度** `run(&Plan, &Board, &dyn WorkerRunner) -> Report`：`std::thread` 作用域线程（没有 async 运行时，没有线程池 crate），在飞数量有上界（`max_parallel`，默认 4），每个 worker 有自己的转录文件 `<root>/workers/<name>.jsonl`，最后按名字排序的确定性报告。

三件东西分开的原因：任务板不关心谁在跑，邮箱不关心任务板，调度不关心模型。`WorkerRunner` 是唯一的接缝，因此并发上界、CAS、邮箱投递都能在没有 provider、没有网络、没有 token 预算的情况下被断言。

**`<root>/workers/<name>.jsonl` 不是会话日志。** 法则 1 是"一个日志只有一个写者"：会话事件只能由 `forge-session` 的 `Store` 追加，本 crate 一个字节都不写。worker 转录是**团队文件**——路径里没有会话 id，`forge-session` 从不打开它，也没有任何事件投影到它上面；它每个 worker 一个文件、由那个 worker 的线程独占写，所以"一个日志一个写者"对它同样成立，只是"日志"指的不是会话日志。把团队转录塞进 `sessions/` 会同时破坏两件事：会话日志的读者会看到不认识的记录，而本 crate 会以为自己被允许写事件。

## 公开面

- 任务板：`Board`（`open`/`add_task`/`task`/`tasks`/`revision`/`claim`/`try_claim`/`complete`/`reopen`/`edit`/`set_dependencies`/`reassign`/`ready`/`in_progress`/`warned_scopes`/`board_problems`），`NewTask`、`TaskPatch`、`Task`、`Status`（`pending`/`in_progress`/`completed`）、`Problem`/`ProblemKind`（`dangling_dependency`/`self_dependency`/`dependency_cycle`）、`ScopeWarning`，常量 `BOARD_FILE`、`BOARD_VERSION`。
- 邮箱：`Mailbox`（`open`/`send`/`drain`/`drain_from`/`path`），`Message { from, to, text, ts }`，`Drain { messages, cursor, skipped, torn_tail }`，常量 `INBOX_DIR`。
- 调度：`run`、`Plan`（`worker`/`member`/`max_parallel`/`mailbox`/`parallelism`）、`WorkerSpec`（`new`/`task`/`validate`）、`Report { tasks, workers, scopes, problems }`、`TaskCounts { claimed, completed, failed }`、`WorkerReport { name, ok, exit, note }`，常量 `WORKERS_DIR`、`DEFAULT_MAX_PARALLEL`。
- runner 接缝：`trait WorkerRunner`、`Call`（`spec`/`task`/`root`/`transcript`/`mail`）、`WorkerResult`（`ok`/`failed`/`exit`/`text`）。随包两个实现：`RecordingRunner`（测试替身，用计数器测量真实并发重叠）与 `CommandRunner`（经 `forge-exec` 起一个进程，提示词走 stdin）。
- 名字校验：`valid_name`、`MAX_NAME_LEN`。名字会变成文件名，所以这是安全边界而不是格式偏好：`..`、分隔符、盘符、Windows 设备名一律拒绝，不静默清洗（清洗会让两个不同的 plan 撞进同一个邮箱）。

## 依赖与边界

只依赖 `forge-core`（`Error`/`ErrorKind`/`Result`）与 `forge-exec`（`Command`/`Pipe`）。**故意不依赖 `forge-session`，也不依赖 `forge-subagent`**：

- 不依赖 `forge-session`：任务板和邮箱是普通文件（临时文件 + rename，或一行一次追加），不是事件流。依赖会话存储会把"要有日志权限"当成"能开一个团队"的前提，而一个没有会话的进程（一个测试、一个批处理工具）也应该能开团队。
- 不依赖 `forge-subagent`：`forge-subagent` 是**串行**的——容量一，一次一个子进程——那是它"委托一件自足任务"的职责决定的。团队要的是"在飞 N 个"，把它接进来只会得到"串行的 N 倍"。真正该复用的是它的思想（有界并发、失败即失败的记录、不写会话日志），不是它的代码。

不写事件、不调模型、不读时钟以外的环境、不起自己的线程池。`run` 里起的每个线程都在 `std::thread::scope` 内被 join；任何一条失败路径（认领被拒、runner 返回 `Err`、runner panic、转录写失败）都变成报告里的一个字段，而不是让团队挂住或用 `Err` 丢掉已经跑完的结果。`CommandRunner` 的子进程通过 `forge-exec` 启动：没有 shell，提示词走 stdin 而不是命令行（提示词任意大、任意含引号，argv 上限与 shell 转义是两个不值得有的问题）。

## 不要做

- **不要在这里跑模型。** `WorkerRunner` 是接缝，模型支持的 runner 属于调用方（lead 的接线工作）。本 crate 只提供 trait、一个测试替身和一个起进程的实现；`CommandRunner` 是"今天就能用"的诚实退路，不是最终形态。
- **不要把写作用域警告变成锁。** 两个 worker 动相邻文件经常没问题、有时是故意的（一个写测试、一个写实现）。因为一条警告就拒绝认领，会教会所有调用方忽略警告，那么真正要紧的那一次就没人读了。所以它只出现在报告里。
- **不要用 `warned_scopes` 当"并发安全"的证明。** 它是前缀比较的**提示**：`crates/team/src` 与 `crates/team/src/board.rs` 会命中，`src/a` 与 `src/ab` 不会（按路径分量比较，不是字符串前缀）；非 Windows 上大小写敏感，Windows 上不敏感。它读不懂真正的文件、也不做任何解析。
- **不要指望任务板是跨机器的。** 它靠"临时文件 + rename"原子替换，加上进程内的 `Mutex`；对同一台机器上的多个进程成立（CAS 在每次迁移里重新读文件），对网络共享不承诺。
- **不要以为认领发生在 worker 线程里。** `run` 在起线程**之前**按名字顺序替整个 roster 认领，然后才并发跑；否则同一份 plan 每次会给不同的 worker 拿到那件任务，团队结果就依赖线程调度了。CAS 的竞态在它该被测的地方测——`Board::claim` 上两个线程抢同一 revision，恰好一个 `Ok` 一个 `conflict`。跨进程同时 `run` 仍然会撞在 CAS 上，`conflict` 会进报告，但没有任何退避或重试。
- **不要以为空 roster 是成功。** `Report::ok()` 对"一个 worker 都没跑"返回 `false`：没发生的事不能被读成做完了。
- **不要指望 `Report::problems` 做结构化判断。** 它是给人读的、已排序去重的字符串；要分支就读 `Board::board_problems()` 的 `Problem`。
- **没有做的**：跨进程的认领顺序仲裁（本 crate 只在进程内调度；多个进程同时 `run` 会在 CAS 上互相拒绝，`conflict` 会出现在报告里，但没有任何退避或重试）；worker 之间的实时投递（邮箱在 worker **启动前**被 drain，运行中到达的消息由下一次 dispatch 投递，所以一次运行内的对话要靠转录文件——这是有意的，它让"worker 的起始状态"可以从转录复现）；任务板的租约/超时回收（一个 crash 掉的进程留下的 `in_progress` 任务要靠人 `reopen`）；以及任何形式的预算、计费或速率限制。`Report::scopes` 是**认领完成后、worker 开跑前**的快照——等 `run` 返回时被认领的任务都已 `completed`，那时再去读活动任务只会得到空列表，而空列表会被误读成"没有冲突"。
