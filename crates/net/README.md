---
dir: crates/net
layer: L1
role: blocking HTTP and HTTPS client, plus HTML to text
public: [Client, ClientOptions, Response, Url, Roots, Connection, Head, Body, BodyMode, connect, read_head, read_bounded, parse_pem_certificates, parse_status, status_slug, to_text, host_without_port, port_from_authority, DEFAULT_TIMEOUT, DEFAULT_MAX_BYTES, MAX_REDIRECTS, ERR_ABORTED]
may_import: [hearth-core]
must_not:
  - import any crate above L1
  - depend on an async runtime
  - decide that a non-2xx status is an error
gate: cargo test -p hearth-net --offline
---

## 做什么

一个建立在 `std::net` 与 `rustls` 之上的阻塞式 HTTP/1.1 客户端，没有 async 运行时，也没有 HTTP crate。它负责那些容易出微妙错误的部分：TLS 信任根（公共 webpki 根，外加可选的 PEM 文件）、有次数上限的重定向跟随、整个请求一个 deadline 而不是每次读一个超时、响应体的字节上限，以及分块传输解码。

`to_text` 把 HTML 页面变成散文：丢掉从不承载文本的元素（`script`、`style`、`noscript`、`template`、`svg`、`iframe`），把块级标签变成换行，解码散文里真的会出现的实体，并折叠连续空行。

## 公开面

`Client` 的 `get`/`post`/`post_json`/`post_json_with_headers`；`ClientOptions`（`timeout`、`max_bytes`、`user_agent`、`max_redirects`、`ca_file`）；`Response`（`status`、`headers`、`url`、`body`、`truncated`、`text()`、`header()`、`is_success()`、`error_summary()`）；`Url`（`parse`、`join`、`origin`、`request_target`）；信任根 `Roots`；`to_text`；`status_slug`；以及给需要直接讲线格式的测试用的底层 `connect`/`read_head`/`Body`。

## 依赖与边界

依赖预算是有意为之的：`rustls` + `rustls-pki-types` + `webpki-roots`，别的一概不要。这正是 `hearth-llm` 用的那套栈，所以二进制里只有一份 TLS 实现，而不是两份。

非 2xx 状态码是**数据**，不是错误：响应带着状态码返回，由调用方决定。只有传输层失败——DNS、TLS、连接、deadline——才是 `Err`。已经到达一些字节后在传输中途断掉的响应体，以 `truncated` 返回而不是被丢弃，因为一个「大体成功了」的 200 比什么都没有更有用。

凭据永远不进 URL。`post_json_with_headers` 存在的目的就是 `Authorization` 头，而当重定向离开原始 origin 时这些头会被丢掉——否则一个搜索 key 会跟着 `Location` 跑到写它的人那里去。

## 不要做

- 不要加 async 运行时或 HTTP crate；整件事的意义就是一条小到可以审计的请求路径。
- 不要把状态码变成 `Err`；404 意味着什么由工具层决定。
- 不要在仍然发送调用方请求头的情况下，跟随重定向到另一个 origin。
- 不要让 `to_text` 长成一个 HTML 解析器。它是散文提取器；真正的解析器是一项依赖，也是大得多的攻击面。