# tests/ — 共享语料，也就是「什么叫做对了」

这里没有产品代码。这里的文件**是判据**：`cargo test` 拿它们比对，而不是拿某个实现比对。

这条规矩比它听起来更强：**不许改这里的用例让某个 reader 通过**。用例是契约，如果用例错了，
是所有实现都错了，不是用例错了。`AGENTS.md` 六条反模式的第一条就是它。

## 里面有什么

| 目录 | 是什么 | 谁在跑 |
| --- | --- | --- |
| [`conformance/cases/events/`](conformance/README.md) | 事件信封的接受与拒绝用例，逐字节比较 canonical 编码 | `crates/core/tests/conformance.rs` |
| [`conformance/cases/http/`](conformance/README.md) | 34 个语言中立的 HTTP 用例：状态码、错误码、头 | `crates/server/tests/`（真 loopback 服务） |
| [`conformance/cases/ws/`](conformance/README.md) | WebSocket 黄金转录 | `tests/ws/` |
| [`fixtures/`](fixtures/README.md) | 会被读的日志、被改写的工作区，故意做成「真实但脏」 | 各 crate 的集成测试 |
| [`golden/`](golden/README.md) | 逐字节冷冻的输出：投影结果、两轮的转录 | 投影与 agent 的回归测试 |
| [`e2e/`](e2e/README.md) | 端到端验收：起真二进制、打真端口 | 单独一个 workspace 成员 |
| [`ws/`](ws/README.md) | WebSocket 客户端的黄金转录与帧级用例 | 单独一个 workspace 成员 |

计数会变，所以这里不写死；想知道现在是几个，看各目录 README 或直接数文件。

## 什么时候进这个目录

- 你改了一个**对外行为**（事件形状、状态码、错误码、usage 文本），要加一条用例。
- 你的测试红了，第一件事是判断**是用例错了还是代码错了**。绝大多数时候是代码错了。
- 你要给「以后不许再这样」立规矩，写成用例比写进文档有用。

## 三种语料怎么选

| 你想固定的是 | 用哪种 | 为什么 |
| --- | --- | --- |
| 某个输入必须被接受/拒绝，且字节不能变 | `conformance/cases/` | 语言中立：任何实现都能跑同一批用例 |
| 一段真实的日志被投影成什么 | `golden/` | 冻结整份输出，回归时能逐字 diff |
| 整个过程能不能跑通 | `e2e/` | 只有真二进制、真端口才能暴露「单测都过但装不起来」 |

## 新东西放哪

- 新事件类型 → `conformance/cases/events/` 一个 `ok_*.json` 或 `reject_*.json`，
  并在 `crates/core/tests/conformance.rs` 的计数断言里加一。
- 新 HTTP 行为 → `conformance/cases/http/` 一个 JSON，形状照抄邻居。
- 新的**工作区**形态（符号链接、大文件、非 UTF-8）→ `fixtures/` 加一份，用例自己建临时目录也行，
  但能被两个 crate 共用的放这里。

## 相关

- [docs/contracts/](../docs/contracts/README.md) — 用例冻结的是这些契约的行为
- [AGENTS.md](../AGENTS.md) — 反模式第一条：不要改用例