---
dir: crates/core
layer: L0
role: the frozen vocabulary — event envelope, ids, canonical JSON, typed errors
public:
  - Event
  - EventType
  - new_id
  - SessionId
  - canonical
  - canonical_line
  - parse_log
  - Error
  - ErrorKind
  - Result
may_import: []
must_not:
  - import any other hearth crate
  - validate payload members (only the envelope is enforced)
  - depend on the filesystem, the clock, or the network
gate: cargo test -p hearth-core --offline
---

## 做什么

所有「必须由不止一个 crate 达成一致」的东西：事件信封（33 种事件类型）、id 格式、规范 JSON 编码，以及错误词汇表。它没有任何 I/O。`parse_log` 接收一个 `&str` 并返回 `Vec<Event>`——它从不打开文件，因为一个会打开文件的读者，没法给「字节早已在手里」的 HTTP 层使用。

## 公开面

`Event`（v/seq/id/ts/session/turn/step/type/payload），带 `parse_line`、`from_value`、`validate`、`canonical`；`EventType` 及其 `ALL` 列表；`new_id` 与 `SessionId`；`canonical`/`canonical_line`；`Error`+`ErrorKind`+`Result`；`parse_log`，它执行单行记录看不出来的两条跨记录规则（序号连续、id 唯一）。

## 依赖与边界

工作区内不依赖任何 crate。工作区外：JSON 用 `serde`/`serde_json`，id 用 `getrandom`——两者都在工作区预算里，理由写在 `Cargo.toml`。

边界是「词汇，不是策略」。一条需要知道「会话是什么」或「工具是什么」的规则，属于上一层。

## 不要做

- 不要在这里加 payload 校验。契约负责描述 payload；读者必须能挺过它从没见过的成员，否则每次升级都会让全部历史失效。
- 不要把 `v` 改成可选或带默认值。没有版本号的记录，其含义无法校验，而猜测比拒绝更糟。
- 不要加文件访问或网络访问。文件归 `hearth-session` 所有，本 crate 只拥有字节。