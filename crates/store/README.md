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

Maintains `index.db` beside (never inside) `<dataDir>/sessions/`. It is a cache
of the event logs: `Rebuild` clears it in one transaction and refolds every
`*.jsonl`, so deleting the index and rebuilding restores the same rows and
search hits. One writer connection in WAL mode, four read-only connections, and
FTS5 triggers keep search in step with ordinary writes.

## 公开面

`Store::open(Options)`, `close`, `path`, `put`, `delete`, `list`, `search`,
`stats`, `rebuild`; types `Options` (path + optional clock), `Session`, `Hit`,
`Stats`, `Query`; `Result<T> = std::result::Result<T, hearth_core::Error>`.
`is_closed` and `is_bad_cursor` identify the two sentinel errors because
`hearth_core::Error` has no downcast. Cursors are opaque base64url tokens with
the Go wire shape `{"u":i64,"i":string}`.

## 依赖与边界

Depends on `hearth-core` (envelope, `parse_log`, errors), `hearth-session`
(projection), `hearth-protocol` (status constants), and `rusqlite` with the
bundled SQLite; `serde`/`serde_json` encode cursors and read payload members.
All are already in the workspace budget. `Options` has no logger field because
there is no slog equivalent in this tree — skipped logs are reported on stderr.

## 不要做

- Do not load an unknown `PRAGMA user_version` or try to migrate it. Drop every
  object, recreate with version 1, and let the caller Rebuild from the logs.
- Do not treat the index as durable truth. A Put failure must never fail the
  session write; a crash mid-Rebuild must leave the previous index intact.
- Do not index tool output. Only `user/message` and `assistant/message` text is
  searchable; a tool payload can be a second copy of the log.
- Do not use OFFSET paging. Concurrent updates must not make a cursor repeat or
  skip rows; the keyset is `(updated_at, id)`.
- Do not let user text become FTS syntax. Every token is quoted and prefix-starred.
