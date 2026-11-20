<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# forge HTTP + WebSocket API contract, protocol version 1

> **Implemented.** Protocol 1 shipped in M3 and is served today by
> `forge web` (`docs/contracts/cli.md` §2.7), which also serves the browser UI from
> the same origin. `internal/server` is the implementation; `web/` is the
> protocol-1 client, and `internal/protocol` + `web/src/api/protocol.ts` (generated
> from it) are the shared wire types.
>
> The document was written before the code, and it remains the compatibility
> promise: once a client ships against protocol 1 the server cannot unilaterally
> change it. Where the shipped server and this text still disagree, the
> disagreement is recorded as a defect in `docs/audit-m7.md` and flagged inline in
> the affected section — that is a list of work, not permission to bend the wire
> format.
>
> The event format underneath is frozen: `docs/contracts/events-v1.md`. This API is
> a transport for those events, and it must never expose state that cannot be
> derived from the log.

Version: **1**. Negotiated with the `X-Forge-Protocol` header (§3).

---

## 1. Shape of the surface

- Transport: HTTP/1.1 (HTTP/2 acceptable) over **loopback only**, plus one
  WebSocket upgrade for streaming.
- Base URL: `http://127.0.0.1:{port}`. The server binds `127.0.0.1` and never
  `0.0.0.0`; a non-loopback peer is rejected with `403` before routing (§2).
- Content type: `application/json; charset=utf-8` for every request and response
  body. A request with any other body content type is `415 unsupported_media_type`.
- Bodies are UTF-8 JSON objects. Unknown request members MUST be ignored
  (forward compatibility mirrors the event format).
- Every response carries `X-Forge-Protocol: 1`.
- The server is single-user and local: no multi-tenant concepts, no user ids.
- All state is derived from the event log. The API may cache projections, but a
  restart must reproduce identical responses from the log alone.

### Casing and encoding rules

- JSON member names are `camelCase`.
- Timestamps are **integer Unix epoch milliseconds** (`ts`, `createdAt`,
  `updatedAt`), never ISO strings, never floats.
- `seq` is a JSON integer; clients using IEEE-754 doubles (JavaScript) must
  handle it as `BigInt` above `2^53`.
- Ids are opaque strings. Clients MUST NOT parse them.
- Absent optional members are omitted, not `null`, unless a schema below says a
  member is nullable.

---

## 2. Authentication and local-only access

- On first start the server generates a **32-byte random token** (256 bits,
  base64url without padding) and stores it in `<dataDir>/token` with owner-only
  permissions (`0600`); a later start reuses that file. The server itself prints
  nothing: `forge web` prints the UI URL with the token as its `token` query
  parameter on stderr, once per start, and that URL is a credential (§2.7 of
  `docs/contracts/cli.md`).
- Every request must present the token, either as
  `Authorization: Bearer {token}` or, for browser clients, as the `HttpOnly`
  cookie `forge_token`. Missing or wrong token → `401 unauthorized`. The token is
  compared in constant time.
  - A browser cannot set `Authorization` on a `GET /` navigation, so the server
    itself sets the cookie: when it serves the shell for a request whose `token`
    query parameter matches, the response carries
    `Set-Cookie: forge_token={token}; HttpOnly; SameSite=Strict; Path=/`. A wrong
    or absent `token` parameter still serves the shell, just without the cookie
    (the operator may also set it out of band; there is no auth endpoint in
    protocol 1). This is what makes the URL `forge web` prints a working
    credential end to end, including the WebSocket, which a page cannot attach a
    header to.
- Requests whose peer address is not a loopback address are rejected with
  `403 forbidden` regardless of token. There is no "bind to the network" mode in
  protocol 1.
- State-changing requests (`POST`) must carry an `Origin` header of
  `http://127.0.0.1:{port}` or `http://localhost:{port}`; a missing or mismatched
  `Origin` → `403 forbidden`. This blocks cross-site request forgery from a
  browser tab.
- WebSocket upgrades apply the same token and `Origin` checks; a rejected
  upgrade returns the HTTP status before the upgrade, not a WS close frame.

---

## 3. Protocol negotiation

| Header | Direction | Required | Value |
| --- | --- | --- | --- |
| `X-Forge-Protocol` | request | yes | `1` |
| `X-Forge-Protocol` | response | always | `1` |

- Missing header on an `/api/*` request → `400` with code `protocol_required`.
- A version the server does not speak (e.g. `2`) → `400` with code
  `protocol_unsupported`. The server does not fall back to another version.
- The order of the checks is observable and is part of the contract: a
  non-loopback peer gets `403` first, then the token is checked (`401`), and only
  then the protocol header (`400`). A request carrying neither a token nor the
  header therefore gets `401 unauthorized`, not `400 protocol_required` — the
  server will not tell an unauthenticated caller which protocol it speaks.
- `GET /healthz` is exempt from version negotiation and auth (§4.1).
- WebSocket clients send the header on the upgrade request; each server frame
  also carries `"protocol": 1` so a proxy that strips headers cannot silently
  downgrade a stream.
  - A browser cannot send a request header on a WebSocket handshake at all. Such
    a client therefore offers the subprotocol `forge.v1` in
    `Sec-WebSocket-Protocol` instead of sending `X-Forge-Protocol`; the server
    accepts a handshake that offers it and echoes
    `Sec-WebSocket-Protocol: forge.v1` in the `101` (RFC 6455 §4.2.2 requires the
    selection to be named, or the browser fails the connection). Either signal
    satisfies negotiation; a handshake carrying neither is still
    `400 protocol_required`.

---

## 4. Endpoints

All paths are under `{base}`. `{id}` is a session id; an unknown id is
`404 not_found`, never an empty `200`.

| # | Method | Path | Purpose | Success |
| --- | --- | --- | --- | --- |
| 1 | `GET` | `/healthz` | Liveness, no auth, no version header | `200` |
| 2 | `GET` | `/api/sessions` | List sessions | `200` |
| 3 | `POST` | `/api/sessions` | Create a session | `201` |
| 4 | `GET` | `/api/sessions/{id}` | One session's metadata/projection | `200` |
| 5 | `GET` | `/api/sessions/{id}/events` | Page of events | `200` |
| 6 | `POST` | `/api/sessions/{id}/messages` | Submit a user message | `202` |
| 7 | `POST` | `/api/sessions/{id}/approvals/{approvalId}` | Answer an approval | `200` |
| 8 | `GET` | `/api/sessions/{id}/stream` | WebSocket: snapshot, then live events | `101` |
| 9 | `POST` | `/api/sessions/{id}/cancel` | Stop the turn that is running | `202` |
| 10 | `POST` | `/api/sessions/{id}/steer` | Inject text into the running turn | `202` |
| 11 | `GET` | `/api/sessions/{id}/attachments/{attachmentId}` | One attachment's bytes | `200` |
| 12 | `GET` | `/api/capabilities` | What this process can offer, session or not | `200` |
| 13 | `GET` | `/api/teams`, `/api/teams/{board}` | Team boards this machine has run | `200` |

### 4.1 `GET /healthz`

No auth, no `X-Forge-Protocol`, no body. `200` with:

```json
{"status":"ok","protocol":1,"version":"0.0.0-dev","uptimeMs":123456}
```

Used by a supervisor process. It must not touch the filesystem or the log, and
must answer within 100 ms. It says nothing about model providers: a healthy
process with no network still reports `ok`.

### 4.2 `GET /api/sessions`

Query: `limit` (default 50, max 200), `cursor` (opaque, from a previous page),
`status` (optional filter: `idle|running|awaiting_approval|ended|aborted`).

```json
{
  "sessions": [
    {
      "id": "s_01jya7c9q3r5t7v9w1x3y5z7ab",
      "createdAt": 1794787200000,
      "updatedAt": 1794787222110,
      "status": "ended",
      "lastSeq": 15,
      "turns": 2,
      "workspace": "D:/work/demo",
      "model": "deepseek-chat"
    }
  ],
  "nextCursor": null
}
```

`status` is derived from the last relevant events (`session/start`,
`turn/start`, `approval/request` without a decision, `session/end`, `turn/end`).
`nextCursor` is `null` on the last page; ordering is `updatedAt` descending, and
ties break on `id` so paging is stable.

### 4.3 `POST /api/sessions`

```json
{"workspace":"D:/work/demo","model":"deepseek-chat","profile":"default"}
```

`workspace` is required and must be an absolute path; a missing directory is
created (`0755`), and a path that exists but is not a directory is refused.
`model` and `profile` are optional and fall back to config. The server appends a
`session/start` event (which therefore has `seq: 1`) and responds `201`:

```json
{"session":{"id":"s_01jya7c9q3r5t7v9w1x3y5z7ab","createdAt":1794787200000,"updatedAt":1794787200000,"status":"idle","lastSeq":1,"turns":0,"workspace":"D:/work/demo","model":"deepseek-chat"},"lastSeq":1}
```

Errors: `400 bad_request` (missing `workspace`, a path that is not absolute, a
path that exists and is not a directory, or an unknown `profile`), `409 conflict`
(a session for the same workspace is already `running` or `awaiting_approval`).

> **Implementation status.** There is no profile registry in this build and
> exactly one known value, `"default"` (the empty profile is also accepted and
> means the same thing). Anything else is rejected with `400 bad_request` rather
> than recorded: the log must not claim a preset no code can honour. When a real
> registry lands, the whitelist in `internal/server/server.go` (`defaultProfile`)
> is the one place that grows.

### 4.4 `GET /api/sessions/{id}`

Same object as one element of §4.2, plus derived detail:

```json
{
  "id": "s_01jya7c9q3r5t7v9w1x3y5z7ab",
  "createdAt": 1794787200000,
  "updatedAt": 1794787222110,
  "status": "awaiting_approval",
  "lastSeq": 15,
  "turns": 2,
  "workspace": "D:/work/demo",
  "model": "deepseek-chat",
  "pendingApproval": {
    "approvalId": "apr_01",
    "callId": "call_01",
    "tool": "shell",
    "args": {"command": "rm -rf build"},
    "reason": "destructive command",
    "requestedAt": 1794787301300,
    "timeoutMs": 120000
  }
}
```

`pendingApproval` is omitted unless an `approval/request` has no matching
`approval/decision`. For a `running` session the same request may also be
answered from a cache while the log catches up; the `seq` values in the response
must never run ahead of the log.

### 4.5 `GET /api/sessions/{id}/events`

Query: `after` (exclusive `seq`, default `0`), `limit` (default 100, max 1000).

```json
{
  "sessionId": "s_01jya7c9q3r5t7v9w1x3y5z7ab",
  "from": 4,
  "to": 8,
  "lastSeq": 15,
  "events": [ { "v": 1, "seq": 4, "id": "…", "ts": 1794787202098, "session": "…", "turn": 1, "step": 1, "type": "assistant/message", "payload": {"text":"…"} } ],
  "nextAfter": 8
}
```

- `events` are **verbatim event objects** from the log, in `seq` order, never
  re-serialised into a different shape (`docs/contracts/events-v1.md` §2).
- `events` may be empty when `after >= lastSeq`; that is `200`, not `404`.
- `from` is the first returned `seq`, `to` the last; both `0` when the page is
  empty. `nextAfter` is what the client passes as `after` to continue; it equals
  `to` (or `after`, unchanged, for an empty page).
- `limit` above the maximum is clamped, not rejected. A `limit` that is not a
  positive integer, or an `after` that is not a non-negative integer, is
  `400 bad_request` (`"limit must be a positive integer"` /
  `"after must be a non-negative integer"`).
- Events of unknown types are returned unchanged (forward compatibility).

### 4.6 `POST /api/sessions/{id}/messages`

Submits one user message and starts (or continues) a turn.

Accepting a message appends `turn/start` and then `user/message`, both carrying
the turn number, before the turn runs. The `seq` in the response is the
`user/message` seq, not the `turn/start` one.

Required header: `Idempotency-Key: {uuid}`. The same key replayed returns the
same result instead of appending a second `user/message`.

Concurrent submissions to one session are serialised: each accepted message gets
its own turn number and exactly one `turn/start`/`user/message` pair, so two
requests never share a turn. The response is returned before the turn runs, so a
second POST is accepted while the previous turn is still running.

If the log takes `turn/start` but the `user/message` append then fails, the
server appends `turn/end` for that turn with `"reason":"error"` — the log is
append-only, so the turn cannot be un-opened — and answers `500`. The session
therefore stays `idle` instead of reporting a running turn with no prompt, and a
retry with the same `Idempotency-Key` is *not* deduplicated: no prompt from the
failed attempt landed, so the retry opens the next turn and runs.

```json
{"text":"Run the package tests.","attachments":[{"kind":"file","path":"notes/todo.md"}]}
```

`text` is required and non-empty (max 256 KiB); `attachments` is optional, max 10
entries, each `{kind:"file"|"image"|"diff", path?, data?, mediaType?, name?}`.

An attachment's body is **not** part of the turn's event. `data` (standard base64,
max 1 MiB each, 8 MiB for all of them together) is decoded and written to a file
under `<dataDir>/blobs/`, and the `user/message` event records only
`{kind, id, mediaType, name, bytes, path}`. Three reasons, all of them measured
rather than assumed: every reader of the log would pay for the base64 (projection,
fork, the index, and every `GET /events` page, which returns whole event lines);
the log is append-only, so an uploaded body could never be deleted; and a fork
copies events verbatim, so one 6 MiB screenshot becomes 12 MiB.

* `kind:"file"` names a workspace path and carries no body. `path` is required.
* `kind:"image"` and `kind:"diff"` require `data`.

Body bytes are fetched with `GET /api/sessions/{id}/attachments/{id}` (§4.11).

Response `202`:

```json
{"seq":16,"accepted":true,"deduplicated":false}
```

A replay of the same key returns `200` (not `202`) with `"deduplicated":true` and
the original `seq`. Errors: `409 conflict` if the session has ended or is
`awaiting_approval`; `422 unprocessable_entity` if the input is well-formed but
cannot be used (e.g. attachment too large); `400 bad_request` for malformed
bodies; `428 precondition_required` when `Idempotency-Key` is missing.

### 4.7 `POST /api/sessions/{id}/approvals/{approvalId}`

```json
{"decision":"allow","note":"generated output, safe to remove"}
```

`decision` is `allow` or `deny`; `note` is optional (max 4 KiB). Appends an
`approval/decision` event and resumes the blocked tool call on `allow`.

Response `200`:

```json
{"approvalId":"apr_01","decision":"allow","seq":5}
```

Errors: `404 not_found` (no such approval, or not pending), `409 conflict` (it was
already answered — a double-click must not append two decisions), `400` for an
unknown `decision` value. This endpoint is the only way to release a blocked
tool call; a timeout is itself a `deny` decision appended by the server, with
`decidedBy: "timeout"`.

### 4.8 `GET /api/sessions/{id}/stream` (WebSocket)

Upgrade on the same base URL. Query `after` (default `0`) resumes: the server
sends a snapshot, then every event with `seq > after`, then live events, without
gaps and without duplicates.

Close codes: `1000` normal, `1002` protocol error (malformed, binary or unsupported
client frames; an unsupported opcode), `1011` server failure (including "client too
slow"). Server pings every 30 s; a client that misses two pings is dropped with
`1000`.

Policy failures are **not** close codes: a bad or missing token, a foreign or
missing `Origin`, and a non-loopback peer are all rejected *before* the upgrade,
so the client sees an ordinary HTTP status — `401 unauthorized`, `403 forbidden`
or, for a missing/mismatched `X-Forge-Protocol`, `400 protocol_required` /
`protocol_unsupported` (§2, §3). Protocol 1 has no `1008` frame: by the time the
socket is open, every policy check has already passed.

#### Frames

Every frame is a single UTF-8 JSON object with a `kind` discriminator. All server
frames carry `"protocol": 1`.

**`snapshot`** — always the first frame on a new connection:

```json
{
  "kind": "snapshot",
  "protocol": 1,
  "lastSeq": 15,
  "session": { "id": "s_01jya7c9q3r5t7v9w1x3y5z7ab", "status": "running", "turns": 2, "workspace": "D:/work/demo", "model": "deepseek-chat", "updatedAt": 1794787222110 }
}
```

**`event`** — one appended event, verbatim, in `seq` order:

```json
{"kind":"event","protocol":1,"event":{"v":1,"seq":16,"id":"…","ts":1794787300000,"session":"…","turn":2,"type":"tool/call","payload":{"callId":"call_03","tool":"shell","args":{"command":"go test ./..."}}}}
```

**`delta`** — streaming assistant text, batched. Tokens are aggregated per
`(turn, step)` and flushed at most every **50 ms** (and on step end), so a client
never receives one frame per token:

```json
{"kind":"delta","protocol":1,"turn":2,"step":2,"seq":15,"text":"One test fails: the loop "}
```

`seq` is the `seq` of the assistant message the delta belongs to; deltas for a
step arrive before that `assistant/message` event. Deltas are advisory: the
authoritative text is the `assistant/message` payload. A client that reconnects
must be able to rebuild the transcript from `event` frames alone.

**`error`** — a recoverable stream-level problem; the connection stays open
unless `fatal` is true:

```json
{"kind":"error","protocol":1,"fatal":false,"error":{"code":"log_read_failed","message":"could not read events.jsonl","details":{"path":"…"}}}
```

Client → server frames are limited to liveness; all mutations go through HTTP:

```json
{"kind":"ping"}
{"kind":"pong","protocol":1}
```

Any other client frame is a protocol error (`1002`). Ordering guarantee: for a
single session, `event` frames arrive in strictly increasing `seq` with no gaps.
A gap is a bug in the server; the client should reconnect with `after` and
re-read (`GET …/events`) to resynchronise.

### 4.9 `POST /api/sessions/{id}/cancel`

Cancels the turn that is currently running. No body; a bodyless POST needs no
`Content-Type`.

```json
{"cancelled":true,"seq":16}
```

- `202` — a running turn was asked to stop. `seq` is the last `seq` of the log at
  the time of the call.
- `409 conflict` — no turn is running.
- `404 not_found` for an unknown session; `401` without the token.

The turn's ending is **not** written by this endpoint. Cancelling the context the
runner was started with makes the loop stop, and the loop appends
`turn/end{reason:"cancelled"}` itself: it owns the record of how the turn ended,
and two writers would produce two records for one turn. Clients see the ending
arrive as an `event` frame (§4.8), never in this response.

Cancel is idempotent while the turn is still open in the log: a second call is
another `202`, because the log — the only truth — still shows the turn running
and context cancellation is idempotent. Once `turn/end` has been appended, the
answer is `409`: there is nothing left to cancel.

`/interrupt` is deliberately not implemented: with one-shot steps, cancelling a
step *is* cancelling the turn, and a separate verb would promise a granularity
the loop does not have.

### 4.10 `POST /api/sessions/{id}/steer`

Injects text into the turn that is already running, instead of opening the next
one.

Header: `Idempotency-Key` required, same rule as §4.6.

```json
{"text":"also check the retry path"}
```

`202` answers with the seq of the appended `user/message`:

```json
{"seq":17,"accepted":true,"deduplicated":false}
```

- The event is appended **before** the response, as
  `{"text":…,"steering":true,"idempotencyKey":…}`, so the text survives a crash
  immediately after the answer. A steer never starts a turn: it belongs to the
  turn that is already open.
- A replay of the same `Idempotency-Key` is `200` with `deduplicated:true` and the
  original `seq`; the event is appended once.
- `409 conflict` — no turn is running. Steering into nothing is a mistake, not a
  message for the next turn.
- `422 unprocessable_entity` — `text` is empty or whitespace only.
- `428 precondition_required` — missing `Idempotency-Key`; `400` if `text`
  exceeds 256 KiB; `404` for an unknown session.
- Delivery is at most once per turn: the text is queued and the loop takes it
  between steps. Text still queued when the turn ends is dropped from the queue
  (its event stays in the log); it is not replayed into the next turn.

### 4.11 `GET /api/sessions/{id}/attachments/{attachmentId}`

Returns the body of one attachment, byte for byte as it was uploaded.

`Content-Type` is the `mediaType` the sender declared, or
`application/octet-stream`. The response is always
`Content-Disposition: attachment` with `X-Content-Type-Options: nosniff`: these
bytes came from a client, and a browser must not render them as a document in
the origin that holds the token. `Cache-Control` is `private, max-age=31536000,
immutable` — a name refers to one body forever.

- `attachmentId` must be 32 hexadecimal characters; anything else is `404` before
  the filesystem is touched, so a name cannot be a path.
- The id must appear in **this** session's log. A blob reached through another
  session's URL is `404`, not a hit: the difference between "not in this session"
  and "never existed" describes somebody else's data.
- `404 not_found` — no such session, no such reference, or no such file.

### 4.12 `GET /api/capabilities`

Describes **this process**, not one session, so it answers before any session
exists. It is a snapshot taken when the process started, not a query: plugins are
loaded once before the listener binds, so what the model can reach does not
change while the process runs.

```json
{
  "mode": "workspace-write",
  "workspace": "C:/work",
  "model": "deepseek-v4.1-flash",
  "plugins": [{ "name": "counter", "detail": "1.0.0 (node runtime)", "active": false }],
  "experts": [],
  "teams": [],
  "bundles": [],
  "skills": [],
  "mcp": []
}
```

Every group is always present, and an empty array means "none configured" — never
"not supported". `active` says whether this build offers the entry to the model
right now: an installed but disabled plugin, or a roster entry marked
`enabled: false`, appears with `active: false`. That row is the reason the route
exists; describing it is the point, so it is never omitted.

Nothing here starts a process or walks the disk on the request path: the server
carries data the CLI discovered at startup, because the crates that can discover
it are not ones this layer may depend on.

- `GET` only; anything else is `405 method_not_allowed`.
- The `detail` strings are for humans and are not a stable machine interface.
  Machine-readable fields are `name` and `active`; everything else may change.

### 4.13 `GET /api/teams` and `GET /api/teams/{board}`

Describes the team boards on **this machine**, not one session. `forge team run`
writes a run into `<dataDir>/teams/<slug>-<YYYYMMDD-HHMMSS>/`, and this route is
the browser's window on it. It reads files and starts nothing: no worker process,
no board repair, and no directory created on the request path.

```json
{
  "boards": [
    {
      "board": "release-squad-20261118-002223",
      "team": "release-squad",
      "parallel": 2,
      "createdAt": "2026-11-18T00:22:23Z",
      "tasks": [
        {
          "id": "t1",
          "subject": "Add the teams route",
          "status": "completed",
          "owner": "api-reviewer"
        }
      ],
      "workers": [
        {
          "name": "api-reviewer",
          "ok": true,
          "note": "1 turn(s), session s_abc",
          "events": 2
        }
      ]
    }
  ]
}
```

`GET /api/teams/{board}` returns one of those objects, unwrapped: the same fields,
no `boards` array.

- `board` is the directory name and the key a client asks again with. `team` and
  `createdAt` are read from that name, because `board.json` carries neither: the
  directory is `<slug>-<YYYYMMDD-HHMMSS>`, and `createdAt` is that stamp rendered
  as RFC 3339 in UTC. A directory named otherwise (the `TeamTool` bridge names one
  after its call id) reports its own modification time and says so in
  `diagnostics`.
- `parallel` is how many workers `forge team run` allowed at once. Nothing
  persists it — `--max-parallel`, `team.max_parallel` and the default are resolved
  in the CLI and then dropped — so a board written by this build reports `null`
  rather than a number the server would have had to invent. A client must render
  `null` as unknown.
- `tasks` and `workers` are what `board.json` and `workers/<name>.jsonl` hold,
  reduced to what a board view shows. `tasks[].status` is passed through exactly as
  written (`pending`, `in_progress`, `completed`); `tasks[].owner` is `null` for an
  unassigned task.
- `workers[].ok` and `workers[].note` come from the transcript's `worker/end` line,
  and `events` counts the JSON lines in it: two for a worker that finished, one for
  a worker that was still running. A transcript with no end line reports
  `ok: null` — **not** `false` — and says why in `diagnostics`: a worker that has
  not finished has not failed, and a board read mid-run must not make every worker
  look broken. `ok: null` therefore means "this build cannot say", the same
  discipline `parallel` follows above.
- `diagnostics` appears only when something is wrong: a half-written board, an
  unparseable `board.json`, a board version this build does not read, a missing
  `workers/` directory, a line that is not JSON. A healthy board has no such key.
  The strings are for humans and are not a stable machine interface, exactly as
  §4.12 treats `detail`.
- `boards` is newest first, ordered by `createdAt` and then by name, so two runs
  started in the same second still have a stable order.
- A machine that has never run a team has no `<dataDir>/teams/` directory, and the
  answer is `{"boards":[]}`. The route does not create it: a read that made a
  directory would make "has a team run here" unanswerable.
- One junk board never fails the route. A half-written or unreadable directory is
  reported with what is known and a diagnostic, so a single corrupt board cannot
  blind an operator to the other nine.
- `{board}` must be one plain path segment: no `.`, no `..`, no `/` or `\`, no
  leading dot, no `:`. Anything else is `404 not_found` **before** a path is built,
  the same discipline §4.11 applies to an attachment id, so a name cannot become a
  path to the token file next door.
- `404 not_found` — no such board, or a name that is not a plain directory name.
- `GET` only; anything else is `405 method_not_allowed`.
- `500 log_read_failed` — `<dataDir>/teams/` exists but cannot be read at all. One
  bad board is never this.

---

## 5. Error envelope

Every non-2xx response with a body uses exactly this shape:

```json
{
  "error": {
    "code": "not_found",
    "message": "session s_01jya7c9q3r5t7v9w1x3y5z7ab does not exist",
    "details": {"sessionId": "s_01jya7c9q3r5t7v9w1x3y5z7ab"}
  }
}
```

| Member | Type | Presence | Meaning |
| --- | --- | --- | --- |
| `error.code` | string enum | always | Stable machine-readable code, `snake_case`. |
| `error.message` | string | always | Human-readable, single line, English, no stack trace. |
| `error.details` | object | omitted when empty | Structured context; shape is code-specific and may grow. |

Codes and their HTTP statuses:

| `code` | Status | When |
| --- | --- | --- |
| `bad_request` | 400 | Malformed JSON, missing/invalid member, bad query value. |
| `protocol_required` | 400 | `X-Forge-Protocol` missing on an `/api/*` request. |
| `protocol_unsupported` | 400 | Header present but not a version the server speaks. |
| `unauthorized` | 401 | Missing/wrong token. |
| `forbidden` | 403 | Non-loopback peer, or bad `Origin` on POST. |
| `not_found` | 404 | Unknown session, approval, or path. |
| `method_not_allowed` | 405 | Known path, wrong method; response carries `Allow`. |
| `conflict` | 409 | State forbids the action (ended session, answered approval). |
| `unsupported_media_type` | 415 | Request body is not UTF-8 JSON. |
| `unprocessable_entity` | 422 | Valid JSON, semantically unusable. |
| `precondition_required` | 428 | Missing `Idempotency-Key`. |
| `rate_limited` | 429 | Too many requests; `Retry-After` in seconds. |
| `internal` | 500 | Unexpected failure. Never leaks internals. |
| `log_read_failed` | 500 | The log could not be read (a corrupted or unreadable session file); `details` names what failed. |
| `unavailable` | 503 | Starting up or shutting down. |

> **Implementation status — one open deviation.**
>
> 1. `internal`: the shipped value is **`internal_error`**
>    (`protocol::code::INTERNAL`), and the JSON-encode failure fallback uses the
>    literal `"internal_error"` as well. The contract text above says `internal`;
>    the code says `internal_error`, and a rename would break every client
>    written against the shipped behaviour, so the value in the table is what a
>    client will actually receive.
> 2. `rate_limited` / `429`: **implemented in W6.** Only
>    `POST /api/sessions/{id}/messages` is limited. The budget is per *session*
>    (not per peer: the listener is loopback-only, and one browser tab is one
>    session), with a fixed window of **120 turns per 60 s** by default
>    (`forge_server::RateLimit::default()`). The response carries
>    `Retry-After: <seconds>`, rounded up and never below `1`, plus
>    `details: {sessionId, limit, windowSeconds, retryAfterSeconds}`. A refused
>    request appends **nothing** — the charge happens after the state checks and
>    before the first event, so a client can tell "refused" from "accepted" by
>    reading the log. Requests refused for another reason (`409`, `503`) do not
>    spend the budget, so retrying a broken request still shows its error.
>    `RateLimit::disabled()` exists for callers that drive the server themselves
>    (the e2e harness, the benchmark) and know their own rate.
>
> `method_not_allowed` used to be deviation #1: `405` answers shipped with
> `bad_request`. Fixed in M7 — `forge_protocol::code` now has the constant and
> the three sites use it.
>
> §2 and §3 gained the two browser mechanisms late: the shell sets the
> `forge_token` cookie for the printed `?token=` URL, and a WebSocket handshake
> may signal its version with the `forge.v1` subprotocol instead of
> `X-Forge-Protocol`. Neither is a wire deviation — both are additive, and a
> handshake carrying neither signal is still `400`. They exist because the
> earlier text described a browser flow no browser could perform: the UI's
> stream retried `401` forever against a real Edge window before these landed.
>
> `log_read_failed` above was missing from this table until M7; the server emits
> it on `500` and the contract had no name for it.

A client must branch on `error.code`, never on `error.message`. New codes may be
added; unknown codes must be handled as their HTTP status category.

---

## 6. Compatibility and versioning

- Protocol **1** is additive-only. New endpoints, new response members, new
  event types and new `error.code` values are compatible and do not bump the
  version.
- Breaking changes (removing/renaming members, changing types or semantics,
  changing status codes, dropping an endpoint) require
  `X-Forge-Protocol: 2` and a documented overlap window in which the server
  accepts both header values.
- Responses may add members at any time; clients MUST ignore unknown members.
  Clients MUST NOT fail on an unknown event `type` (it is passed through).
- The `version` field in `/healthz` is the build version (`0.0.0-dev` style) and
  is **not** the protocol version; do not gate features on it.
- The spec is authoritative where it is explicit, but the eventual implementation
  plus its conformance tests are the truth (`AGENTS.md`): when they disagree, fix
  this document in the same change that fixes the code.

### How this contract is verified

The event format has the data-driven suite this document asked for
(`tests/conformance/cases/events/*.json`); the HTTP surface does not. What exists
today, and where:

1. `internal/server/*_test.go` — `httptest`-driven request/response tests per
   endpoint (`api_test.go`, `sessions_test.go`, `messages_test.go`,
   `approvals_test.go`, `turncontrol_test.go`, `static_test.go`, `intake_test.go`),
   including the `405` + `Allow` cases and the idempotent-replay cases.
2. `internal/server/wsclient_test.go` — a real WebSocket client over the
   hand-written upgrade, asserting frame order (snapshot → events → deltas).
3. Fixture replay over the API is covered indirectly: the events endpoint returns
   the raw log lines (`json.RawMessage` of the line), and `internal/session`
   tests already pin those bytes.
4. The idempotency test exists: a second `POST …/messages` with the same
   `Idempotency-Key` appends nothing and answers `deduplicated: true`
   (`messages_test.go`).

What is still missing relative to the plan above: `tests/conformance/cases/http/`
does not exist, so there is no language-neutral HTTP case corpus, and no golden
WebSocket transcript fixture file. `internal/server/README.md` records the
implementation decisions this contract left open.