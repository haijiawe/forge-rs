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
  - import any other forge crate
  - validate payload members (only the envelope is enforced)
  - depend on the filesystem, the clock, or the network
gate: cargo test -p forge-core --offline
---

## 做什么

Everything that more than one crate must agree on: the event envelope (15 event
types), the id format, the canonical JSON encoding, and the error vocabulary.
It has no I/O. `parse_log` takes a `&str` and returns `Vec<Event>` — it never
opens a file, because a reader that opens files cannot be used by the HTTP layer
that already has the bytes in hand.

## 公开面

`Event` (v/seq/id/ts/session/turn/step/type/payload) with `parse_line`,
`from_value`, `validate`, `canonical`; `EventType` with its `ALL` list; `new_id`
and `SessionId`; `canonical`/`canonical_line`; `Error`+`ErrorKind`+`Result`;
`parse_log`, which enforces the two cross-record rules a single line cannot show
(dense sequence numbers, unique ids).

## 依赖与边界

Depends on nothing inside this workspace. Outside it: `serde`/`serde_json` for
JSON and `getrandom` for ids — both in the workspace budget for reasons stated
in `Cargo.toml`.

The boundary is "vocabulary, not policy". A rule that needs to know what a
session is, or what a tool is, belongs one layer up.

## 不要做

- Do not add payload validation here. The contract describes payloads; readers
  must survive members they have never seen, or every upgrade invalidates every
  history.
- Do not make `v` optional or defaulted. A record without a version is a record
  whose meaning cannot be checked, and guessing is worse than refusing.
- Do not add file or network access. `forge-session` owns the file; this crate
  owns the bytes.
