---
dir: crates/mcp
layer: L1
role: external MCP client — stdio and Streamable HTTP transports, protocol 2025-06-18
public:
  - PROTOCOL_VERSION
  - Transport
  - Server
  - ToolDef
  - CallResult
  - Content
  - ServerInfo
  - Logger
  - NoopLogger
  - Error
  - Dial
  - dial
  - Client
  - ErrServerGone
  - ErrProtocolVersion
may_import: []
must_not:
  - import any hearth crate, including hearth-core
  - put stderr bytes into the JSON-RPC stream
  - handshake inside Dial
  - use the real network in tests
gate: cargo test -p hearth-mcp --offline
---

## 做什么

一个阻塞的、基于线程的 MCP 客户端，覆盖两种传输：走子进程的 JSON-RPC 2.0（一行一个 JSON 对象）和 Streamable HTTP（POST 一份 JSON 文档，读回一份 JSON 文档或一条 SSE 流）。`Dial` 只负责连上，握手指 `initialize`。进程/会话不存在与协议版本不支持，是刻意分开的两种失败。

## 公开面

`Transport::{Stdio,Http}` 与 `Server` 描述对端；`dial`/`Dial` 接收一个 `&Server` 和一个 `Arc<dyn Logger>`，返回 `Box<dyn Client>`。`Client` 有 `initialize`、`tools`、`call`、`close` 和 `clone_handle`（同一条连接上的第二个持有者，好让 `close` 能打断一次在飞的调用）。结果用 `ServerInfo`、`ToolDef`、`CallResult` 和 `Content`。`Error` 是带类型的；`ErrServerGone` 与 `ErrProtocolVersion` 是两个哨兵字符串。`Logger` 接收 stderr 与诊断信息——它永远收不到协议字节。

## 依赖与边界

没有任何 hearth 内部依赖，连 `hearth-core` 都没有：MCP 是一套外部协议，不该被绑到内部的事件类型或错误类型上。外部依赖是线上 JSON 用的 `serde`/`serde_json`，以及 HTTPS 用的 `rustls`/`rustls-pki-types`/`webpki-roots`，全部来自工作区预算。HTTP/1.1 分帧、分块解码、SSE 和那个小小的本地测试服务器都在这里实现，因为预算里没有 HTTP 客户端 crate。

边界是「线协议客户端，不是策略」：本 crate 不决定哪些工具被允许，不持久化任何东西，也不知道会话或审批的存在。

## 不要做

- 不要加 tokio 运行时或 async API。两种传输都是阻塞的，每连接一个线程。
- 不要把 stderr 混进协议流。它只进 `Logger`；而 stdout 上一行格式错误会直接杀掉这条连接。
- 不要让单次调用的超时变成粘性的。一次超时的调用之后连接仍然可用；一个死掉的服务器则不可用。
- 不要加会去连真实主机的测试。HTTP 测试用 127.0.0.1 上的裸 TCP，TLS 只通过构建信任根库来验证。