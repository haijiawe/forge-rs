# e2e/ — 真的把二进制跑起来，用脚本化的模型走完一轮

这是唯一一个**不把代码链进来**的测试 crate：它 `Command::new` 起真正的 `hearth` 可执行文件，
在 loopback 上起一个假的 OpenAI 兼容 SSE provider，然后检查 stdout、stderr、事件日志和 HTTP 面。

代价是慢（每个用例要起进程），收益是它能抓到单元测试结构上抓不到的东西：
参数解析、`--version` 快路径、`HOME` 目录布局、日志路径、关进程时有没有留下监听端口。

## 里面有什么

| 文件 | 是什么 |
| --- | --- |
| [`tests/acceptance.rs`](tests/acceptance.rs) | 五个验收用例（下面逐个说） |

## 五个用例各自守什么

| 用例 | 守的是 |
| --- | --- |
| `cli_plain_turn_streams_text_and_writes_dense_event_log` | 一轮纯文本：文本流式到达、日志 seq 稠密、`request/context` 记了模型名 |
| `cli_tool_call_with_always_approval_runs_shell_and_final_answer_uses_output` | `--approve always`：工具真的跑了、结果进了下一轮请求、最终答案引用了它 |
| `cli_tool_call_with_never_approval_has_no_approval_events_and_fails_tool` | `--approve never`：**不产生审批事件**、工具调用以失败结束而不是卡住 |
| `cli_two_turn_resume_replays_history_and_uses_the_remembered_fact` | 跨进程续跑：第二轮能复述第一轮埋下的事实（历史从日志重建，不是从内存） |
| `http_surface_matches_oracle_protocol_and_event_contract` | `hearth web` 的 HTTP 面：建会话、发消息、读事件、`lastSeq` 与事件页一致 |

## 为什么它不依赖网络和密钥

假 provider 就在这个测试进程里（`serve_provider_stream`），只讲 `POST /v1/chat/completions`。
所以 `cargo test` 在离线机器、CI、以及别人的笔记本上给出的结论是同一个。
**任何需要真模型才能跑的检查都不许进这里**——那种检查的正确位置是 `docs/` 里的一段实测记录。

## 常见的假失败

- 起进程需要几秒：超时统一是 `TIMEOUT = 20s`，机器极慢时才需要调。
- `CARGO_BIN_EXE_hearth` 只在本 crate 拥有该二进制时才被 cargo 设置，其余情况回落到
  共享 `target/` 目录。所以**先 `cargo build` 再跑 e2e**，否则会报找不到可执行文件。
- `http_surface_…` 里 `lastSeq` 与事件页是同一次请求内两次读文件的产物，已改成有界重试；
  改这段代码时不要把重试删掉，那不是容忍缺陷，是承认两个值不可能原子取到。

## 相关

- [tests/README.md](../README.md) — 三种语料怎么分工
- [conformance/README.md](../conformance/README.md) — 语言中立的判定用例
- [docs/contracts/api.md](../../docs/contracts/api.md) — HTTP 面的契约