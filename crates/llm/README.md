---
dir: crates/llm
layer: L2
role: provider contract and the OpenAI-compatible SSE client
public: [Provider, Stream, Request, Message, ToolSpec, Delta, Usage, Aggregator, ProviderError, Client, Options, OpenAiStream, Canceller]
may_import: [hearth-core]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

模型适配层：一个产出 delta 的 `Provider` trait，以及一个讲 `/chat/completions` 的 OpenAI 兼容 SSE 实现（移植自已退役的 `forge-go/internal/llm/openai`）。provider 在调用线程上用阻塞 socket——没有 async 运行时。明文 `http://` 走 `std::net::TcpStream`；`https://` 走工作区的 rustls 栈，配固定的 `webpki-roots` CA 包。

### 私有 CA

`Options { ca_file: Some(path), .. }` 把 PEM 文件里的证书加进那份固定包，于是由私有 CA 前置的端点无需关闭公共信任就能通过校验。私有根是**叠加在** `webpki-roots` 之上的；公共包从不被替换。CA 文件缺失、不可读或格式损坏，都会作为一个带类型的 `ProviderError` 报出，其消息点名该文件，但绝不打印文件内容（一个证书包可能和私钥放在一起）。配置文件用可选的 `caFile` 键暴露同一个开关。

## 公开面

冻结的 `contract.rs` 面（`Provider`、`Stream`、`Request`、`Message`、`ToolSpec`、`Delta`、`Usage`、`Aggregator`、`ProviderError`），加上 OpenAI 适配器：`openai::Client`、`Options`、`openai::OpenAiStream`、`openai::Canceller`、`openai::provider_error`，以及共享的错误码常量。`Client::stream_open` 返回具体流类型，好让调用方拿到 `Canceller`，从另一个线程关掉一个阻塞的读者。

## 依赖与边界

不要导入 `hearth-session`：provider 不该知道日志存在，否则「重放一条流」就没法做单元测试。不要加 async 运行时；流在调用线程上读。

## 不要做

- 不要在 `Cargo.toml` 的工作区预算之外加依赖。
- 不要悄悄换成 async，也不要加 SSE/HTTP 客户端 crate：线格式代码故意只用 std + 工作区 TLS。
- 不要让 API key 出现在错误消息或截断的响应体里。