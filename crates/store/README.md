---
dir: crates/store
layer: L2
role: the rebuildable session index — SQLite rows, WAL readers, FTS5 over boundary messages
public:
  - Store
  - Options
  - Session
  - Hit
  - Stats
  - Query
  - Result
  - is_closed
  - is_bad_cursor
may_import:
  - hearth-core
  - hearth-session
  - hearth-protocol
must_not:
  - write to a session log (the log is the only truth)
  - migrate an unknown schema version (drop and rebuild instead)
  - index tool output or any non-boundary event
  - import anything from L3
gate: cargo test -p hearth-store --offline
---

## 做什么

维护 `index.db`，放在 `<dataDir>/sessions/` **旁边**（绝不放在里面）。它是事件日志的缓存：`Rebuild` 在一个事务里清空它，并重新折叠每一个 `*.jsonl`，所以删掉索引再重建会还原出同样的行、同样的搜索命中。一个 WAL 模式的写连接、四个只读连接，加上 FTS5 触发器，让搜索与普通写入保持同步。

## 公开面

`Store::open(Options)`、`close`、`path`、`put`、`delete`、`list`、`search`、`stats`、`rebuild`；类型 `Options`（路径 + 可选时钟）、`Session`、`Hit`、`Stats`、`Query`；`Result<T> = std::result::Result<T, hearth_core::Error>`。`is_closed` 与 `is_bad_cursor` 用来识别这两个哨兵错误，因为 `hearth_core::Error` 没有 downcast。游标是 base64url 的不透明令牌，形状 `{"u":i64,"i":string}`，与冻结契约一致。

## 依赖与边界

依赖 `hearth-core`（信封、`parse_log`、错误）、`hearth-session`（投影）、`hearth-protocol`（状态常量），以及带 bundled SQLite 的 `rusqlite`；`serde`/`serde_json` 用来编码游标和读取 payload 成员。这些都已在工作区预算内。`Options` 没有 logger 字段，因为本树里没有 slog 的对应物——被跳过的日志报到 stderr。

## 不要做

- 不要加载未知的 `PRAGMA user_version`，也不要试图迁移它。丢掉每一个对象、以版本 1 重建，让调用方从日志 Rebuild。
- 不要把索引当成持久真相。Put 失败绝不能导致会话写入失败；Rebuild 中途崩溃必须让上一份索引完好无损。
- 不要索引工具输出。只有 `user/message` 与 `assistant/message` 的文本可搜索；工具 payload 可能是日志的第二份副本。
- 不要用 OFFSET 分页。并发更新不能让游标重复或跳过行；键集是 `(updated_at, id)`。
- 不要让用户文本变成 FTS 语法。每个 token 都加引号并加前缀星号。