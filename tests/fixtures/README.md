# fixtures/ — 看起来像真的输入，不是断言

和 `conformance/` 的区别：conformance 用例说的是「必须这样判」，fixtures 说的是
「这是一份长得像真实世界的东西，拿它跑一遍，别炸」。

## 里面有什么

| 文件 | 是什么 | 谁在读 |
| --- | --- | --- |
| [`events/v1/happy-path.jsonl`](events/v1/happy-path.jsonl) | 一轮正常对话的完整日志（用户提问 → 工具调用 → 回答） | [`crates/session/tests/session_tests.rs`](../../crates/session/tests/session_tests.rs)、[`crates/import`](../../crates/import/README.md) |
| [`events/v1/approval-flow.jsonl`](events/v1/approval-flow.jsonl) | 需要审批的一轮：请求 → 批准 → 执行 | 同上 |
| [`events/v1/aborted-turn.jsonl`](events/v1/aborted-turn.jsonl) | 用户中途取消的一轮 | 同上 |
| [`events/v1/torn-final-line.jsonl`](events/v1/torn-final-line.jsonl) | 写到一半被断电的日志——最后一行不完整 | 存储层必须能读它、且**不能**把它当成完整事件 |
| [`events/v1/unknown-future-event.jsonl`](events/v1/unknown-future-event.jsonl) | 含一个这个版本不认识的事件类型 | 法则二：未知**类型**是数据，跳过并计数 |
| [`llm/text-stream.json`](llm/text-stream.json) | 一段纯文本的 SSE 流 | [`crates/llm`](../../crates/llm/README.md) 的解析测试 |
| [`llm/tool-call-stream.json`](llm/tool-call-stream.json) | 一段带工具调用、参数分片到达的 SSE 流 | 同上 |

## 加一个 fixture 的门槛

- **它必须是别人真会遇到的形状**，不是为了让某个边界分支跑一遍而手搓的。手搓的边界属于 `conformance/`。
- **它必须能独立读懂**：文件开头就是真实的一行事件，不要加注释行（那会让它不再是合法日志）。
- 撕裂的、未知的、非法的这类「坏」输入，文件名要说出坏在哪（`torn-final-line` 而不是 `bad-1`）。

## 相关

- [tests/README.md](../README.md) — 三种语料怎么分工
- [conformance/README.md](../conformance/README.md) — 断言放那里
- [golden/README.md](../golden/README.md) — 期望输出放那里