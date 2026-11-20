# 子代理层移植记录（S1–S5）

对照对象：DSH `packages/subagent/subagent/src/` 的 22 个源文件（逐文件清单见
`docs/agent-gaps.md` §E）。硬约束：**全进程内 Rust，不移植 Cordis / @Remote /
TypertRemoteService / gateway 这些远程服务器面，也不依赖任何 DSH / DS 服务**；
只取进程内的语义。

## 已交付

| 波次 | 提交 | 交付 | 证据 |
| --- | --- | --- | --- |
| S1 持久标识 | `47118db` | `subagent/catalog` + `subagent/descriptor` 两个事件入 `EventType::ALL`（22 类型）；`names.rs`（`^[a-z][a-z0-9-]{0,31}$`、非 `lead`、默认 `agent-N`）、`descriptor.rs`（version 3、one-shot/continuable、first-wins fold）、`catalog.rs`、`list.rs`；runner 起子时写 descriptor 并在父日志占名；`hearth subagent list --session <id> [--json]` | conformance 26 用例；`tests/identity.rs` 2 例；`check.ps1` ok |
| S2 并发池 | `cec20ec` | 单槽信号量 → 计数池，容量由 `Options::max_active` 定（0 取默认 1 = Go oracle 的串行行为；设大即并发）；超限排队而非 DSH 的 `ACTIVATION_LIMIT_REACHED` | runner 3 个池单测 |
| S3 冷恢复 | `a42df34` | `continuable:true` 的 child 不写 `session/end`（停在最后一个 `turn/end`）；`Runner::resume(childId, prompt, …)` 打开同一日志追加新 turn，只靠 durable 日志，换进程也能续；one-shot / 已 end / 非 child id 在写任何事件前被拒 | `tests/resume.rs` 3 例 |
| S4 父→子消息 | `ea42128` | `subagent` 工具加 `resume` 参数：模型可对一个 continuable child 继续发工作；resume 拒绝改 schema、不写新 catalog 行；未知/one-shot/已 end 的 id 是**报错结果**而非 panic | `tests/resume.rs` 共 5 例 |

## S5（进程外后端）— 已交付：worker 模式，我们自己的二进制

DSH 的 S5 是让 `SubagentProvider` 能接**进程外**后端（`out-of-process.ts`、`start`
而非 `prepareContinuable`）。在 hearth 里，一个「后端 trait + 唯一本地实现」本来
是只有一种实现的投机抽象（`AGENTS.md` 六反模式），所以先前只做评估。用户拍板要真实
的第二后端，于是把它实现了——但**不是**接 DSH / DS 服务器，而是 spawn **我们自己的
`hearth` 二进制**，以 `hearth subagent worker` 模式跑：一行请求 JSON 进、一行响应 JSON
出，是一个真正独立的进程（自己的内存、崩溃域、环境）。

| 波次 | 提交 | 交付 | 证据 |
| --- | --- | --- | --- |
| S5 进程外后端 | 本提交 | `crates/subagent/src/backend.rs`：`Backend` trait + `BackendRequest`/`BackendOutcome`（stdio 一行 JSON，`v=1`，op=run/resume）+ `ProcessBackend`（spawn、写一行、带超时读一行、stderr 继承）；`Options::backend` 路由 `run_child`/`resume`（有后端则 child 归 worker、不写本地 log；父侧仍占名/回填 catalog）；CLI `hearth subagent worker`（读 stdin 一行、本进程跑、写一行，失败也写 `ok:false`）与 `hearth subagent prompt <child> --text`（跨进程 resume） | `backend` 6 单测（含跨平台 spawn）；`hearth-subagent` lib 36 passed；`hearth-cli` 15 集成（含 list 正/负例） |

**边界（诚实）**：worker 只跑 one-shot child——一个需要**更晚的进程**去 resume 的
continuable child 走本地路径，因为 worker 的进程在这行写完时就没了。跨进程 `interrupt`
随真正的常驻外部进程一起才有意义，本波不做。协议版本不兼容时拒绝而非强转；worker 永
远写一行响应，失败是 `ok:false` 行而不是裸退出。

## 与 DSH 的语义差（有意为之）

- **同步 vs 异步激活**：DSH 的 child 是常驻 activation，父侧靠 inbox 收 settled 通知；
  hearth 的 child 是同步工具调用，工具结果就是 settled 答案，因此不需要额外的 settled
  事件（同 W2：持久输入队列已由日志 + 现有 202 语义承载）。
- **排队 vs 拒绝**：超并发上限时 DSH 抛 `ACTIVATION_LIMIT_REACHED`；hearth 排队，因为
  工具是同步的、模型已经要求了这份工作。
- **控制面**：DSH 的 `subagent.prompt` / `subagent.interrupt` 是远程 RPC；hearth 用
  `resume`（父→子）与进程内 `Cancel`（中断），不移植 RPC 面。
