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

One blocking HTTP/1.1 client over `std::net` and `rustls`, with no async runtime and no HTTP crate. It owns the parts that are easy to get subtly wrong: TLS trust roots (public webpki roots plus an optional PEM file), redirect following with a bounded count, a whole-request deadline instead of per-read timeouts, a byte cap on the body, and chunked transfer decoding.

`to_text` turns an HTML page into prose: drops the elements that never hold text (`script`, `style`, `noscript`, `template`, `svg`, `iframe`), turns block tags into line breaks, decodes the entities that actually appear in prose, and collapses blank runs.

## 公开面

`Client` with `get`/`post`/`post_json`/`post_json_with_headers`; `ClientOptions` (`timeout`, `max_bytes`, `user_agent`, `max_redirects`, `ca_file`); `Response` (`status`, `headers`, `url`, `body`, `truncated`, `text()`, `header()`, `is_success()`, `error_summary()`); `Url` (`parse`, `join`, `origin`, `request_target`); `Roots` for trust roots; `to_text`; `status_slug`; and the low-level `connect`/`read_head`/`Body` for tests that need to speak the wire.

## 依赖与边界

The dependency budget is deliberate: `rustls` + `rustls-pki-types` + `webpki-roots` and nothing else. That is the same stack `hearth-llm` uses, so the binary carries one TLS implementation rather than two.

A non-2xx status is **data**, not an error: the response is returned with its status and the caller decides. Only a transport failure — DNS, TLS, connection, deadline — is an `Err`. A body that breaks off mid-transfer after some bytes arrived is returned truncated instead of discarded, because a 200 that mostly worked is more useful than nothing.

Credentials never go in the URL. `post_json_with_headers` exists for an `Authorization` header, and those headers are dropped when a redirect leaves the original origin — otherwise a search key would follow a `Location` to whoever wrote it.

## 不要做

- Do not add an async runtime or an HTTP crate; the whole point is a small, auditable request path.
- Do not turn a status code into an `Err`; the tool layer decides what a 404 means.
- Do not follow a redirect to another origin while still sending the caller's headers.
- Do not grow `to_text` into an HTML parser. It is a prose extractor; a real parser is a dependency and a much larger surface.