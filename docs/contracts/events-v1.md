<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# hearth event format, version 1

Status: **normative for the v1 on-disk format.** This document is written so that
a reimplementation in any language (Rust, TypeScript, Go, ...) can read and write
`hearth` session logs without looking at Go source. Where this document and the Go
code disagree, the code and `tests/conformance/` are the truth and this document
is the bug.

Machine-readable companion: `tests/conformance/cases/events/*.json`. Every rule
in this document that can be stated as input/output has at least one case there.
A conforming implementation is one that passes every case, plus the fixture rules
in [§11](#11-conformance-suite).

The key words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119.

---

## 1. What a log is

A session log is a file at `<dataDir>/sessions/<sessionId>.jsonl` — one flat file
per session, not a directory per session. If a session ever needs sidecar files
(an index, a lock, blobs), they go in `<dataDir>/sessions/<sessionId>/` and the
log itself stays at the flat `.jsonl` path.

- Encoding: UTF-8, no BOM.
- Format: **JSON Lines** — one JSON value per line, each line terminated by a
  single `\n` (LF, `0x0A`). `\r\n` is not written; readers MAY accept and strip
  a trailing `\r` before parsing.
- Each line holds exactly one **event** (the envelope, §2, plus its payload).
- The file is **append-only** (§5). It is the single source of truth for a
  session: no projection, cache or index may be treated as authoritative.
- An empty file is a valid log (a session that has not started yet). A missing
  file is treated as an empty log by a reader that is allowed to create it.
- Blank lines (zero bytes between newlines, or only whitespace) are ignored by
  readers and MUST NOT be written by writers. They neither advance `seq` nor
  produce an event.

---

## 2. The envelope

Every event is a JSON object with exactly these members. Unknown members may
also be present (§6).

| Field | JSON type | Required | Nullable | Type / range | Meaning |
| --- | --- | --- | --- | --- | --- |
| `v` | number (integer) | yes | no | must be `1` | Format version of this record. |
| `seq` | number (integer) | yes | no | `1 .. 2^64-1` | Position in the log. Dense, ascending, starts at 1. |
| `id` | string | yes | no | non-empty | Event identity. Writers use 32 lowercase hex chars (128 random bits). Readers MUST only require non-empty and unique-per-log. |
| `ts` | number (integer) | yes | no | `> 0` | Wall-clock time of the event, **Unix epoch milliseconds, UTC**. |
| `session` | string | yes | no | non-empty | Session id this event belongs to. Readers MUST accept any non-empty string up to the length the filesystem allows; the two shapes writers mint today are `s_` + 26 lowercase Crockford-base32 chars (`internal/server`, 128 random bits) and `<YYYYMMDD-HHMMSS>-<6 lowercase hex>` (`cmd/forge`, e.g. `20260101-120000-a1b2c3`). |
| `turn` | number (integer) | no | no | `1 .. 2^32-1` | Turn number within the session. Omitted (not `0`) when the event is not part of a turn. |
| `step` | number (integer) | no | no | `1 .. 2^32-1` | Step number within the turn. Omitted when not applicable; SHOULD NOT appear without `turn` (readers do not reject it). |
| `type` | string | yes | no | non-empty | Event type, `namespace/name` (§4). |
| `payload` | any JSON value | no | yes | valid JSON | Type-specific body. v1 writers SHOULD use an object. Omitted when there is nothing to carry. |

Notes that implementations get wrong:

- `v`, `seq` and `ts` are **JSON numbers, integers, no fraction and no
  exponent**. Write `1`, never `1.0` or `1e0`. `seq` is an unsigned 64-bit
  integer: a JavaScript implementation must use `BigInt` for `seq` (values above
  `2^53` lose precision as `number`).
- `turn` and `step` are omitted when zero. `"turn": 0` and `"step": 0` are not
  written; a reader that sees them MUST treat them as absent (Go's decoder
  leaves the field at zero, which is exactly this rule). `error_` cases exist for
  every other required field, not for a zero turn.
- No envelope field is nullable. An explicit `null` in `v`, `id`, `ts`,
  `session` or `type` is a validation error, not "absent": a null `v`
  deserialises as 0 and therefore fails the version check; null `id`/`session`/
  `type` are empty strings; null `ts` is 0. A reader that models these fields as
  `Option` must reject `null` rather than coerce it. `payload: null` is the one
  accepted null (§3.6).
- Unknown members in the envelope are **ignored**, and MAY be dropped when the
  event is re-encoded (§6).

### Example of a complete event

```json
{"v":1,"seq":7,"id":"708192a3b4c5d6e7f801122334455667","ts":1794787203302,"session":"s_01jya7c9q3r5t7v9w1x3y5z7ab","turn":1,"step":2,"type":"assistant/message","payload":{"text":"Added a retry wrapper with exponential backoff and jitter.","stopReason":"end_turn"}}
```

---

## 3. Invariants

A log is **valid** when every record satisfies all of the following. A reader
MUST reject a log that violates them and MUST NOT silently repair it
(exception: the torn tail in §8).

1. **`v == 1`.** Any other value is an unsupported version (§9).
2. **`seq` starts at 1** and is **dense and ascending**: record *n* has
   `seq == n`. No gaps, no repeats, no reordering. `seq` is assigned by the
   writer at append time and never reused.
3. **`id` is non-empty** and **unique within the log**. `id` is random, so two
   independent logs may share ids; uniqueness is per log.
4. **`ts > 0`.** Timestamps are epoch milliseconds. They are **not** required to
   be monotonic: a clock step backwards is possible and must not fail a read.
   `seq`, not `ts`, defines order.
5. **`session` is non-empty** and **identical for every record in a log.** One
   file holds exactly one session. The Go store derives the session from the
   file path and does not re-check each record, so a mismatch means the file was
   merged or hand-edited: the fixture test rejects it, and a reader SHOULD treat
   it as corruption rather than as two interleaved sessions.
6. **`type` is non-empty.** Writers MUST use `namespace/name` in lowercase
   (`^[a-z][a-z0-9-]*(/[a-z][a-z0-9-]*)+$`, optionally deeper, e.g.
   `tool/call`, `future/step/heartbeat`). Readers MUST NOT reject an event for a
   `type` that does not match that shape — only an empty one is invalid. Unknown
   types are accepted (§6).
7. **`payload`, if present, is valid JSON.** Its shape depends on `type` and is
   not validated by the v1 reader (§4).
8. **The record is a JSON object.** A top-level array, string, number or boolean
   is not an envelope.

Validation error identities that a conforming implementation must be able to
report, and the substrings the conformance cases match on:

### Rejection diagnostics

| Condition | Stable diagnostic substring | Go message today |
| --- | --- | --- |
| Not parseable as JSON (torn line, trailing garbage) | `invalid JSON` | `invalid JSON: record is not a JSON value` |
| Parses, but is not an object with the v1 members | `does not match the v1 envelope` | `record does not match the v1 envelope: ...` |
| `v` present but not 1 | `unsupported event version` | `session: unsupported event version %d (want %d)` |
| `id` empty/missing/null | `empty id` | `session: event has empty id` |
| `session` empty/missing/null | `empty session` | `session: event has empty session` |
| `type` empty/missing/null | `empty type` | `session: event has empty type` |
| `ts` missing, zero or negative | `empty ts` | `session: event has empty ts` |
| `seq` gap / wrong position | `not dense` | `record %d: event seq %d is not dense (want %d)` |
| same `id` twice in one log | `duplicate event id` | `duplicate event id %q (first seen at record %d)` |

The Go messages are informative, not normative: an implementation MUST surface
the condition, and the substring above is what the conformance suite asserts.

---

## 4. Event types

The v1 type catalogue is **open**. These thirty-two types have producers in `hearth` today:

| `type` | Emitted when | Payload members in current producers |
| --- | --- | --- |
| `session/start` | a session is created | `workspace`, `model`, `profile`, `startedBy`, optional `system` (only when a system prompt is configured), `systemBytes` + `systemTruncated` when that prompt exceeded 8 KiB and was clipped; a forked session's head also carries `forkedFrom`, `forkedAtSeq`, `forkedAt` (§4.1) |
| `session/end` | a **terminal** session finishes (a child session has one; a resumable one does not — see below) | `reason` (`completed` \| `aborted`), `turns` |
| `session/archived` | the operator declared that this session starts no new work | optional `reason` |
| `turn/start` | a user turn begins | `number` (the 1-based turn number; absent in older logs, where a reader falls back to arrival order) |
| `turn/end` | a turn finishes | `reason` (`completed` \| `cancelled` \| `error`), `steps`, optional `cancelledBy` (free-form; the loop writes a cause word, the recovery path writes `cli:recovery`) |
| `step/start` | one model call begins inside a turn | `index` (0-based within the turn), `model` |
| `step/end` | one model call finished | `index`, `reason` (`end_turn` \| `tool_use` \| `cancelled` \| `max_tokens` \| `error`) |
| `llm/call` | a request is about to be sent | `model`, `messages` (count), `tools` (names), optional `temperature`, `maxTokens` |
| `user/message` | user input is accepted (at turn start, or mid-turn by a surface that supports steering — the surface writes this row when it takes the input in, and the loop only puts the text in front of the model at the next step boundary, never a second row) | `text`, `attachments` (array) |
| `assistant/message` | model output for a step | `text`, `stopReason` (`end_turn` \| `tool_use` \| `cancelled` \| `max_tokens` \| `error`), optional `partial`, `usage` (`inputTokens`/`outputTokens`/`cacheReadTokens`/`cacheWriteTokens`), `durationMs` |
| `tool/call` | a tool is about to run | `callId`, `tool`, `args` (object) |
| `tool/result` | a tool finished | `callId`, `tool`, `ok` (bool), `output`, optional `code` (when `ok` is false), `truncated`, `meta` (free-form tool facts: `exitCode`, `bytes`, `durationMs`, …; `read_image` puts `imagePath`/`mediaType`/`bytes` here — §4.0.2) |
| `approval/request` | a tool needs consent | `approvalId`, `callId`, `tool`, `args`, `reason`, optional `timeoutMs` |
| `approval/decision` | the user answered | `approvalId`, `callId`, `decision` (`allow` \| `deny`), `decidedBy`, optional `note` |
| `compaction/summary` | history is compacted | `summary`, `replaces` (`{from,to}` seq range) |
| `error` | the harness failed a step | `message`, optional `code`, `retryable` |
| `goal/created` | a goal begins: keep working until it is done or blocked | `goalId`, `text`, `maxRounds`, optional `revision` (u64) |
| `goal/continued` | a continuation round is opened by the harness, not by a person | `goalId`, `round`, optional `revision` (u64) |
| `goal/ended` | the goal stops | `goalId`, `reason` (`complete` | `blocked` | `cancelled` | `exhausted`), optional `note`, optional `revision` (u64) |
| `context/compressed` | a seq range is replaced, for this window only, by a summary the model can name | `blockId`, `from`, `to` (inclusive seq), `summary`, optional `messages`, optional `surfaceOp` (§4.0.3) |
| `hook/rewrite` | a hook changed what the provider is about to receive (§4.0.4) | `phase` (`pre-step` \| `request`), `hook` (the names that changed something, comma-separated), `changed` (how many hooks did) |
| `subagent/catalog` | the loop records that this session started a child (§4.0.5) | `version` (3), `childId`, `mode` (`one-shot` \| `continuable`), optional `label`, optional `name` |
| `subagent/descriptor` | a child session records how it was created, once, inside its first turn (§4.0.5) | `version` (3), `mode`, `provider`, optional `label`, optional `agentProvider` \| `agentModel` \| `agentReasoningEffort` \| `persona` \| `toolFilter` |
| `compaction/start` | a compaction transaction opens, before the summarizer runs (§4.0.6) | `from`, `to` (the seq range the transaction intends to fold) |
| `compaction/end` | the transaction closes | `ok` (bool), optional `reason` (`empty` \| `error` \| `already-compacted`) |
| `compaction/summary-error` | a summary could not be produced, so the history stays visible (§4.0.6) | `reason` (`empty` \| `error`), `message` |
| `request/header` | the loop names a request it is about to send (§4.0.7) | `reason` (`initial` \| `series` \| `change` \| `resume`), `model`, `messages` (count), `tools` (names) |
| `request/context` | the loop names what a request actually carries (§4.0.9) | optional `provider`, `model`, `messages` (count), `tools` (count), optional `temperature`, optional `maxTokens`, optional `stop`, `systemBytes`, `systemFingerprint` |
| `assistant/attempt` | a provider attempt that did **not** become the visible answer (§4.0.7) | `attempt` (0-based), `outcome` (`retry` \| `error` \| `cancelled`), `retryable` (bool), optional `message`, optional `code` |
| `agent/inbox` | one change to the durable input queues (§4.0.8) | `target` (`next-turn` \| `next-step`), `start`, `removed`, `inserted` (`[{id,text}]`) |
| `agent/status` | a resident loop entered or left a phase (§4.0.8) | `phase` (`running` \| `maintenance` \| `idle`) |
| `file/write` | a person saved a buffer through the editor (§4.0.10) | `path` (workspace-relative), `bytes`, `sha256` |

The optional `revision` on the three `goal/*` payloads is the version a
multi-writer continuation uses to decide, *before it writes*, whether it is still
the one advancing the goal: a writer reads the log's latest `revision` and
appends `goal/continued` (carrying `revision + 1`) only when its local copy still
matches — otherwise it is `Stale` and writes nothing, so two entrants cannot
double-run one goal. **Any `goal/*` event that omits `revision` reads as `0`** —
old logs have none, and a reader must not refuse them for it. `goal/created`
writes `0`, `goal/continued` writes the incremented value, `goal/ended` writes the
version it stops at, and `fold` never lets the value go backwards.

**Exactly one `approval/decision` per `approvalId`, written by whoever decided.** `approval/request` is always written by the agent loop — only it knows the turn and the step the consent belongs to. The decision is written by the surface that produced the answer: the CLI approvers answer inside the loop (`--approve=always`, `--approve=prompt`), so the loop records them; the browser answers over HTTP, so the request handler appends the decision *before* it replies — the consent has to be in the log before a 200 is sent, or a restart between the click and the response would lose the answer the log exists to preserve. An approver that records its own decision declares that with `tool.DecisionRecorder`, and the loop then writes only the request. Two writers would turn one click into two records and leave an audit unable to say how many times a human was asked. A *timeout* is still recorded by the loop as a `deny` with a `note`: "nobody answered" is the loop failing to get an answer, not a decision by anyone.

`decidedBy` names **who** answered, not which binary wrote the record: `user` when a human answered at the HTTP surface (the browser is the user, not the process), the approver's own `Name()` for CLI approvers (`flag:--approve=always`, `prompt`), and one of `timeout` | `cancelled` | `error` when nobody answered and the loop recorded the deny. A reader that needs provenance ("was this a human or the default?") reads this field; a reader that needs the code path reads the event's neighbours.

The three "nobody answered" words are kept apart because they are different
facts, and the loop is the only writer that can tell them apart:

| value | when | what an operator should conclude |
| --- | --- | --- |
| `timeout` | the deadline passed — nobody was at the keyboard | the question was reasonable, the human was absent |
| `cancelled` | the turn was cancelled while the question was open | the human *withdrew* the request; nothing was denied on the merits |
| `error` | the approver itself failed (I/O error, closed pipe) | a bug or a broken surface, not a policy decision |

A writer MUST NOT name the approver in this case: a payload saying
`decidedBy: "web"` next to `note: "no answer: context canceled"` contradicts
itself, and an audit that cannot tell "the human said no" from "the human walked
away" has lost the only thing it was for.

**`turn/start` has more than one producer, on purpose.** Whichever surface owns the turn intake writes it: the agent loop for a CLI turn, the HTTP layer for an API turn (which must open the turn before the runner is asked to run it, so `POST /api/sessions/{id}/messages` can answer with the prompt's `seq` synchronously). Readers must not assume which surface wrote a given `turn/start`, and must not infer CLI-versus-API from it.

**`session/end` belongs to terminal sessions only.** A resumable session file is **resumable**: `hearth chat --session X` appends turn 2 to the same log, and a later process may append turn 3. So "the session ended" is not a fact the harness can observe for those — the process exiting is an operator event, not a domain event. Writing `session/end` when a process detaches would be a lie the next process has to contradict. Readers must therefore treat absence of `session/end` as normal, and must not use it to decide whether a resumable session is complete.

A process that dies **inside** a turn leaves a `turn/start` with no `turn/end`. That is not corruption: it is the honest record of a turn that never finished, and the reader's own projection sees a `running` turn. The next process to resume the log closes it by **appending** a `turn/end` with `reason="cancelled"` and `cancelledBy="cli:recovery"` (§5 forbids the rewrite that would otherwise be tempting), and then continues with the following turn number. Nothing is invented to fill the gap: an unanswered `step/start` stays unanswered, because "this step was abandoned" is already what an open step means.

The one producer is a **child session created by the `subagent` tool**: nobody can resume it (its id is returned in a tool result, not chosen by a user), so it is terminal by construction, and it writes `session/end` once with `reason` `completed` or `aborted` and the turn count it ran. A reader that wants "is this subtree finished" can rely on that; a reader that wants "is this conversation finished" still cannot.

**`session/archived` is a declaration, not a lifecycle event.** It says the
operator decided this session starts no new work; it does not say the session is
finished, unreadable, or deletable. Three consequences follow, and all three are
deliberate:

- **At most once, and idempotent.** The only writer is `hearth session archive`,
  which reads the log first and writes nothing if the record is already there. A
  second run reports `already archived` rather than appending a duplicate, so a
  reader never has to decide which of two records wins.
- **A missing log is not an archived session.** `is_archived` answers `false`
  when the file does not exist, because the archive record lives in the session's
  own log: an archived session always has one. Answering `true` would make a
  typo in a session id look like a policy decision.
- **It refuses new work at the doors, not inside a turn.** The check runs when a
  subagent is started and when a `continuable` child is resumed, walking the
  `session/start` `parent` chain hop by hop. A turn already running keeps its
  approval and its cancel token: stopping it mid-flight is `cancel`, a different
  verb with a different record.

The refusal names the session that is archived — which may be an ancestor rather
than the one asked about — because that is the only entity an operator can act on.

**``turn/end.cancelledBy`` says who stopped a turn.** It is written only when
``reason`` is ``cancelled`` and the writer knows why, and it is **free-form**: the
recovery path writes ``cli:recovery``, imported v0 logs carry the old
implementation's words, and the loop writes one of ``user``, ``parent``,
``timeout``, ``hook``, ``disposed``, ``unknown``. A reader MUST treat an
unrecognised value as data (rule 2) — the member's job is to make "the operator
pressed stop" distinguishable from "the owner was disposed", and that job does not
require a closed vocabulary.

``unknown`` is not silence. A token raised without a stated cause reports
``unknown`` deliberately: the turn really was cancelled, and refusing to say so
would leave the one row that explains the turn short a field.
**`turn/end.reason` and `step/end.reason` are different vocabularies, on purpose.**
A turn and a step answer different questions, so they do not share a word list:

- **`turn/end.reason` — what became of the whole turn:** `completed` \|
  `cancelled` \| `error`. A step that ran out of tokens (`max_tokens`) or ended
  because the model asked for tools (`tool_use`) is part of a turn that
  **completed**: the model did what it could and the turn was not a failure. A
  writer MUST NOT copy a step-level reason into `turn/end`.
- **`step/end.reason` — why one model call stopped:** `end_turn` \| `tool_use` \|
  `cancelled` \| `max_tokens` \| `error`. `assistant/message.stopReason` uses the
  same step-level list plus `error`, because a stream that fails after emitting
  text legitimately leaves partial output behind (see `partial`).

A reader branching on `turn/end.reason == "completed"` therefore counts a turn
whose last step ended `end_turn`, `tool_use` or `max_tokens` as successful; it
must not look for `end_turn` in `turn/end`. Conversely, "did the model stop
because it wants a tool?" is a `step/end` (or `stopReason`) question, never a
`turn/end` one. This asymmetry is load-bearing: collapsing the two lists is what
made successful turns invisible to readers that expected `completed`.

`step/start`, `step/end` and `llm/call` were added in M1. Adding a type is an
**additive** change: a v1 reader that has never heard of `step/start` must skip
it and keep reading (see §6), so their arrival did not bump the format version.
`llm/call` records the *shape* of the request — model, message count, tool names,
sampling knobs — and deliberately **not** the prompt text: the prompt is already
in the log as the `user/message` / `assistant/message` / `tool/result` records it
was built from, so storing it twice would double the log for no gain.

**Payload schemas are descriptive, not yet enforced.** No v1 reader validates a
payload; a writer that adds a member is compatible, and a reader MUST NOT reject
an event because its payload is unexpected. This table records what the writers
emit so a reimplementation can render them; it becomes normative only when a
schema registry lands.

Tool output and subprocess output are capped (64 KB tool result, 1 MiB exec) and
oversized payload text is truncated before it is appended, with `truncated: true`
recorded. A reader must not assume payloads are small.

### 4.0.1 `user/message` attachments

An element of `attachments` is a **reference**, never a body:

| member | type | when |
| --- | --- | --- |
| `kind` | `file` \| `image` \| `diff` | always |
| `id` | 32 hex characters | when the sender uploaded bytes; names a file in `<dataDir>/blobs/` |
| `mediaType` | string | when the sender declared one |
| `name` | string | when the sender named the file |
| `path` | string | for `kind:"file"`: a workspace-relative path |
| `bytes` | number | always; the decoded size, `0` for `kind:"file"` |

A body is fetched with `GET /api/sessions/{id}/attachments/{id}` (api.md §4.11).
It is not in the log, and this is deliberate: every reader of the log pays for
its size — projection, fork, the index, and every `GET /events` page, which
returns whole event lines — the log is append-only so a mistake could never be
deleted, and a fork copies events verbatim so one screenshot costs twice its
bytes. A reader that needs pixels must ask the server, and may assume `id` is
absent (a `kind:"file"` attachment has no body at all).

### 4.0.2 `tool/result` images

A tool that answers with pixels cannot put them in `output`: a tool message is
text on every chat-completions wire. `read_image` therefore writes the facts and
keeps the bytes out of the log, exactly as an attachment does:

| member of `meta` | type | when |
| --- | --- | --- |
| `imagePath` | string | a workspace-relative path the tool read |
| `mediaType` | string | one of `image/png`, `image/jpeg`, `image/gif`, `image/webp`, `image/bmp`, `image/tiff`, `image/x-icon` |
| `bytes` | number | the file size at the moment it was read |

The bytes reach the model once, as a follow-up `user` message the loop builds
from `Result.parts`; they are never in the log. A later turn re-reads the file
the path names, which means an image stays visible for the rest of the session at
the cost of one small metadata row per read — and that if the file is deleted or
replaced by text, the image is simply gone. A reader must tolerate a tool message
with no `meta` at all: most tools have nothing structured to say.

### 4.0.3 `context/compressed` blocks

`compaction/summary` is one-way: the oldest complete history is replaced and the
replaced records are never read again. `context/compressed` is the other shape —
a range the model chose to stop paying for, which it can still name, restore and
search.

The event adds no envelope member and no version bump, so a v1 reader that has
never heard of blocks keeps reading the log and simply shows the uncompressed
history: the safe direction. What it carries is an address, not a copy.

| member | type | meaning |
| --- | --- | --- |
| `blockId` | string | 32 lowercase hex characters; the name the model hands back |
| `from` | number | first `seq` the block hides, inclusive, at least 1 |
| `to` | number | last `seq` the block hides, inclusive, not before `from` |
| `summary` | string | what the model reads instead; 1 byte to 8 KiB |
| `messages` | number, optional | how many projected messages fell in the range when the block was made — a diagnostic, never an address |
| `surfaceOp` | object, optional | how the block is surfaced: absent means `hide`; `{"op":"replace","startSeq":N,"endSeq":M}` stands a summary message in for the range instead of leaving a silent gap |

`surfaceOp` is the one optional member that changes what the model *sees* rather
than how the block is described. It is absent in every block written before the
field existed, and absent means **hide** — the historic behaviour — so a reader
that ignores it reads the log exactly as it always did. When present, `op` must
be `"replace"` (an unknown op is an error, not a silent `hide`: the writer meant
to surface something, and losing it quietly is worse than failing), and
`startSeq`/`endSeq` are inclusive and must fall inside `[from..to]` — a summary
for a range the block does not cover is a claim the ledger cannot answer on
restore. There is still no begin/summary/commit trio: the block is one event, so
it is atomic by construction (a half-written block cannot be observed).

Four invariants, each of which a writer must check before appending:

1. **Ranges are seq ranges.** Seq numbers survive replay, re-forking and a
   different reader; a message index survives none of the three.
2. **No two blocks overlap.** Overlap would hide one record twice and give
   "restore" two answers. Touching ranges (`[1..5]`, `[6..9]`) are fine.
3. **A block id is unique within a log**, and is 32 hex characters, so it is a
   key and never a path.
4. **The range must already be closed by the log.** A pointer to a future
   record is not an address; the writer checks this, because only it knows how
   far the log has been written.

Restoring a block is a **read**, never a rewrite: the originals are still in the
log a few records earlier, so `decompress` returns them from the same file. The
log therefore never shrinks, and a block is never edited — a block that was a
mistake is still there, and the way to stop paying for it is to restore it.

### 4.0.4 `hook/rewrite` records

A hook is an extension point that may change what the provider receives, without
changing the log it was derived from. Two rewrite phases exist: `pre-step` (the
message list about to be sent) and `request` (the fully built request, after tools
are attached). A hook returns either "unchanged" or a rewritten value; the loop
resolves hooks in installation order, each later hook seeing earlier changes.

A third phase, `request-error`, does **not** rewrite and is **not** recorded here:
after a request attempt fails and the transport policy would give up, a hook may
answer only "retry or not" (`HookOutcome::Retry`). It changes nothing durable, so
there is no `hook/rewrite` row for it — the retry it forces shows up as the next
`llm/call`.

`hook/rewrite` is the audit record. Every phase that actually changed something
writes exactly one row, naming the hooks that changed it:

| Member | Type | Meaning |
| --- | --- | --- |
| `phase` | string | `pre-step` or `request`. |
| `hook` | string | The names of the hooks that changed something, comma-separated. |
| `changed` | integer | How many hooks did. |

The inert case is silent: a step where no hook changed anything writes no
`hook/rewrite`. This is what lets a reader say "the log had no rewrite" and mean
it — the loop cannot rewrite what the provider sees without leaving a row. The
record does **not** carry the before/after text. Logging the difference would put
the very bytes a hook may have redacted back into the log, defeating the hook.
The row asserts *that* a rewrite happened and *who* did it; the content is
recoverable only by re-running the same hooks over the same prefix, which is the
same footing `context/compressed` stands on.

A hook may not change the model id (the loop restores it after the call) and may
not append events itself — the loop is the only writer. A hook that fails is not
swallowed: the step ends `error` with `code: hook_failed`, so a broken extension
is loud rather than silently absent.

### 4.0.5 `subagent/catalog` and `subagent/descriptor` records

A subagent is a child session with a durable identity, so a later turn can name
it again after a restart. Two rows record that identity, and each has exactly one
writer:

- `subagent/catalog` is written **on the parent log** by the loop, once per child
  it starts, when the child's tool result comes back. The `subagent` tool never
  writes it — it hands the loop a payload through `Result.meta["subagentCatalog"]`,
  the same channel `compress` uses for `contextBlock`, because the loop is the
  parent log's only writer.
- `subagent/descriptor` is written **on the child's own log** by the child's
  runner, once, inside its first turn, right after `session/start`. It is
  log-only: it never enters the model's history and survives compaction.

| Member | Type | Meaning |
| --- | --- | --- |
| `version` | integer | `3`. A reader that does not know this version folds to "no descriptor", not an error. |
| `childId` | string | (`catalog` only) the child session id. |
| `mode` | string | `one-shot` or `continuable`. |
| `provider` | string | (`descriptor` only) the backend that ran the child. |
| `label` | string | optional, human-facing. |
| `name` | string | (`catalog` only) the child's durable sibling name, e.g. `agent-2`; see below. |
| `agentProvider` / `agentModel` / `agentReasoningEffort` / `persona` / `toolFilter` | optional | (`descriptor`, `continuable` only) the composition the child was created with. |

**Names.** A child's name is unique among its siblings, matches
`^[a-z][a-z0-9-]{0,31}$`, and is never `lead` (the root's name). When the caller
does not ask for one, the runner picks the first free `agent-N`. The name lives in
the catalog row; the tool also echoes it in `Result.meta["name"]` for the model's
eyes, but the log row is the source of truth.

**Folding.** A catalog is the ordered list of its rows (a bad row is skipped, not
fatal — unknown types are data). A descriptor fold reads the **first**
`subagent/descriptor` on the log and ignores later ones; a version it does not
know folds to `None` rather than throwing, so an old reader meeting a future
descriptor degrades instead of refusing the log.

**One-shot vs continuable.** A `one-shot` child is terminal: when its run
finishes it writes `session/end`, and nothing may append to it again. A
`continuable` child never writes `session/end` at rest — its last `turn/end` is
the resting state — so a later `Runner::resume(childId, prompt, …)` may append
another turn to the same log, driven entirely by the durable log rather than by
any in-memory activation. Resuming a one-shot child, a child that already wrote
`session/end`, or an id not shaped like a child id fails before a single event is
written. This is what lets a child survive a restart: the log carries it, not a
live process.

### 4.0.6 `compaction/start`, `compaction/end` and `compaction/summary-error`

A compaction is not one append: the summarizer runs between deciding a range and
writing the summary, and a process can die in that gap. Three additive rows turn
it into an inspectable transaction on the same append-only log:

- `compaction/start{from,to}` opens the transaction **before** the summarizer is
  asked for text. The seq range it names is the plan's range.
- `compaction/summary` (the existing record) lands the summary when one is
  produced.
- `compaction/end{ok,reason?}` closes the transaction. `ok:true` means a summary
  landed; `ok:false` records a round that folded nothing (`reason` is `empty`,
  `error`, or `already-compacted` when a concurrent compactor won the range).
- `compaction/summary-error{reason,message}` records a summary that could not be
  produced (provider error, or empty output). It replaces nothing.

**A failed compaction hides nothing.** When the summarizer errors or returns only
whitespace, the writer records `summary-error` + `end{ok:false}` and appends no
`compaction/summary`; the covered events stay visible to the model. This is the
one place where "give the model more" is the safe direction: an empty summary
would otherwise become the permanent record of what those events said.

**Orphans are detectable, not silent.** A `compaction/start` with no following
`compaction/end` is a compaction that died in flight; `open_compaction()` reports
its seq. A reader that trusts a half-written range would treat a partial summary
as the whole story, so the bracket exists precisely to make that state visible.

### 4.0.7 `request/header` and `assistant/attempt`

A turn's provider traffic has two shapes a reader needs and that the answer alone
does not carry: *why* a request exists, and *which attempts failed on the way*.
Two additive rows record them.

- `request/header{reason,model,messages,tools}` is written once per request the
  loop builds, **before** `llm/call`. `reason` is `initial` for the first request
  of a turn, `resume` when the loop adopts a turn the HTTP layer already recorded,
  `change` when the tool set differs from the previous request, `series` for every
  later step of the same run. The message bodies are deliberately absent: they are
  already on earlier rows, and copying them would make this row as large as the
  request itself.
- `assistant/attempt{attempt,outcome,retryable,message?,code?}` is written for
  every provider attempt that did **not** become the visible answer. `outcome` is
  `retry` (the loop will try again), `error` (it will not), or `cancelled`.
  `attempt` is 0-based within the step.

**The successful attempt writes no `assistant/attempt` row.** It is the single
`assistant/message` the step already records, so a reader that folds only
`assistant/message` sees exactly the answer it saw before these rows existed —
these two types are diagnostics, not history. That is also why neither is ever
fed back to the provider.

Before `assistant/attempt` existed, a turn that failed twice and succeeded on the
third try looked identical to one clean call. That is the difference between "the
provider was fine" and "the provider was fine on the third try", and it was not
recoverable from the log.

### 4.0.9 `request/context`

`request/header` answers *why* a request exists. `request/context` answers *what
it carried*, which is the question you ask when two runs over the same log
disagree about an answer.

- `request/context{provider?,model,messages,tools,temperature?,maxTokens?,stop?,systemBytes,systemFingerprint}`
  is written once per request, immediately after `request/header` and again
  before `llm/call`.

Every member is a scalar or a digest:

| Member | Meaning |
| --- | --- |
| `provider` | The authority the request went to (`api.example.test:8443`). Absent when the caller did not name one — an unnamed endpoint is omitted, never guessed. |
| `model` | The model id the request named. |
| `messages` | How many messages went out. |
| `tools` | How many tool specs went out (a count, not names: `request/header` already carries the names). |
| `temperature`, `maxTokens`, `stop` | Present only when actually configured. A default the loop applies is not reported as a caller choice. |
| `systemBytes` | Byte length of the system prompt. |
| `systemFingerprint` | 16 lowercase hex characters: FNV-1a 64 of the system prompt. |

**The system prompt itself is never repeated here.** It is already on
`session/start`; copying it per step would make this row the largest in the log
for the smallest amount of new information. The fingerprint is a change detector,
not a security boundary: an adversary who can choose the prompt already writes
the log. Its definition is frozen (`hearth_core::digest::fingerprint`) because
these values are written down and compared later — a digest that moved with a
compiler upgrade would make every stored fingerprint a lie.

Two rows together are the difference between "the answer changed" and "the
request changed", which is what a comparison between two runs needs.
### 4.1 Forked logs

A fork (`hearth chat --fork`, `hearth session fork`) creates a **new** session whose
log is a copy of a prefix of another session's log. A forked log is an ordinary
v1 log: it satisfies every invariant in §3, its `seq` runs densely from `1`, and
every record carries the child's session id. A reader needs no fork awareness to
project it — the child's turns are simply the copied ones, and the next turn is
one greater than the last copied turn.

The copy is **verbatim** with exactly two envelope changes per copied record:

1. `session` becomes the child's id;
2. `seq` is renumbered from `1`, in the original order.

`v`, `id`, `ts`, `turn`, `step`, `type` and `payload` are copied unchanged, so a
reader can still correlate a copied record with the one in the parent, and a
reader that checks uniqueness within one log (§9) stays honest.

The **head** record is the parent's own `session/start`, copied the same way, with
three payload members added:

| Member | Type | Meaning |
| --- | --- | --- |
| `forkedFrom` | string | The parent's session id (the immediate parent only: a fork of a fork names its own parent). |
| `forkedAtSeq` | integer | The parent's highest copied `seq`, i.e. the cut. |
| `forkedAt` | integer | When the copy was made, unix milliseconds (all cross-package times are ms). |

Copying the parent's `session/start` rather than synthesising a fresh one is
deliberate: that record already carries the workspace, model, profile, `startedBy`
and system-prompt facts the copied turns were produced under, and a synthesised
head would silently drop them — and every member a future producer adds. Adding
members to a payload is a compatible change (§6), so the head stays readable by a
v1 reader that has never heard of `forkedFrom`.

If the parent has **no** `session/start` (a hand-written or legacy log), the fork
prepends a head of its own: a `session/start` carrying `forkedFrom`, `forkedAtSeq`
and `forkedAt`, plus `workspace`, `model` and `profile` folded from the copied
prefix so the child does not claim to have been created in an unknown directory.
It never invents facts the parent did not have: a member that cannot be folded is
left out.

The **cut** is the last `turn/end` in the parent by default, so a fork never
starts mid-turn and never inherits a turn nothing will close. An explicit cut must
name an existing parent record and must not fall inside an open turn. The parent
log is opened read-only for the whole operation: its bytes do not change, and a
torn tail in the parent is neither read as an event nor truncated.

---

### 4.0.8 `agent/inbox` and `agent/status`

Two rows let a session carry work that has not been answered yet, and let a
reader see a loop that was between turns.

`agent/inbox{target,start,removed,inserted}` is one change to the durable input
queues. The payload is a **splice**: `start` and `removed` address the slice
taken out of the existing queue, and `inserted` is what replaces it. One shape
covers every operation, so a reader never has to know which verb the writer had
in mind:

| operation | row |
| --- | --- |
| append | `{target, start: len, removed: 0, inserted: [{id, text}]}` |
| claim the head | `{target, start: 0, removed: 1, inserted: []}` |
| clear | `{target, start: 0, removed: len, inserted: []}` |

`target` is `next-turn` (deliver as the prompt of a fresh turn) or `next-step`
(deliver before the agent's next model call). Both queues are folded out of
these rows, which is the point: an in-process deque would be a second copy of
the truth, and a message that was accepted would die with the process. The
envelope carries `turn = 0` and `step = 0` because a queued message belongs to
no turn yet.

A splice that does not fit the queue it names (`start` past the end, or
`removed` reaching past the end) is **skipped, not clamped**. Clamping would
silently delete a message the writer did not mean to delete, and a reader that
loses prompts is worse than one that misses an update. An unknown `target` is
skipped for the same reason: putting a message in the other queue changes when
the model sees it, which is not a decision a reader gets to make.

`agent/status{phase}` records a resident loop entering or leaving a phase:
`running`, `maintenance` or `idle`. It is written **only** for a maintenance
window, not for ordinary turns. A turn already says everything about itself; two
extra rows per turn would be a diagnostic in every log for no reader's benefit.
A maintenance window is the opposite case: without these rows an unexplained gap
between two turns is indistinguishable from a slow provider.

A background child reports through the same queues: when it settles, its notice
is appended as a `next-turn` message, so a parent that was restarted between
delegating and settling still finds the answer. The notice is prose, not a tool
result, which is why a background delegation cannot also ask for a schema.

Both rows are log-only. Neither ever enters model history: the queues decide
*when* a message reaches the model, and the message itself is still a
`user/message` row.

### 4.0.10 `file/write`

`file/write{path, bytes, sha256}` records that **a person** saved a buffer through
the editor. `path` is workspace-relative and never absolute; `bytes` is the size
of what was written, including zero; `sha256` is the digest of the bytes written,
lowercase hex, so a later reader can ask whether the file still matches what was
saved.

It is not a `tool/call`, and the difference is the reason the type exists. The
tool channel is a conversation: `tool/call` is what the loop asked a tool to do
and `tool/result` is the answer it gave the model. A human saving a file has no
model in the loop, and recording it as a tool call would make the transcript
claim the agent wrote a file it never touched — a reader cannot tell that apart
from the agent actually writing it.

What the row buys is the ability to explain a disagreement. The log says the
agent read `main.rs` and saw X; the disk says Y; without this row the only
hypothesis available is a bug. With it, a reader can say when the file changed
and by whose hand. That is law 1 applied to the workspace: after a restart the
file is the only surviving evidence, and evidence without a timestamp is a
rumour.

Two consequences worth stating:

- The projection **ignores** it and model history never sees it. It is a fact
  about the workspace, not about the conversation; putting it in front of the
  model would spend tokens on something no tool result depends on.
- A write that the harness refuses (read-only mode, a path outside the
  workspace, a stale `expectedSha256`) appends **nothing**, because no file
  changed. A row that describes a write which did not happen is worse than a
  missing row.

## 5. Append-only, never rewrite

1. Records are **only appended**, at the end, one line each. An existing record
   is never edited, reordered, renumbered or deleted.
2. State is derived from the log; it is never written back into it. A correction
   is a **new event**, not an edit.
3. Compaction appends a `compaction/summary` event that describes a range of
   earlier `seq` values. It does not remove them.
4. `seq` values are dense for the life of the log. If a log ever appears to have
   a gap, that is corruption: report it, do not renumber.
5. The **only** permitted non-append write is truncating a torn tail back to the
   last complete record during recovery (§8). That deletes no complete record.

Writers MUST `fsync` before reporting success, or document the durability window
they accept. `hearth` defaults to `fsync` on every append, with an opt-out
(`SyncEvery`, `NoFsync`) used only by benchmarks and tests.

### 5.1 Compaction (`compaction/summary`)

Compaction shortens what a reader renders without touching what the log holds. It
appends **one** event and nothing else:

```json
{"summary":"turns 1-2: the agent read a.txt and answered.","replaces":{"from":2,"to":13}}
```

- `summary` is non-empty text. It is what the old range is now said to have been.
- `replaces.from` and `replaces.to` are inclusive `seq` values naming the range
  the summary stands in for. It is descriptive: it **never** authorises deleting
  or editing those records, and a full replay of the log still sees them.

**What may be replaced.** A writer selecting a range must satisfy all of these:

1. **Turns are atomic.** A turn runs from `turn/start` to `turn/end`; a range may
   not end inside one. A log that begins mid-turn (the tail of a longer session)
   is read as if the first event naming a turn opened it, and a `turn/end` with no
   `turn/start` is a one-event turn, exactly as the projection synthesises it.
2. **Calls and results are atomic.** A `tool/call` and its matching `tool/result`
   are one unit. If the result lies beyond the chosen cut, the cut moves back
   before the call: a result whose call has been summarized is unusable.
3. **A replaced range is never replaced again.** Selection starts after the last
   `compaction/summary` event, so summaries do not nest and no `seq` is described
   twice.
4. **The recent tail stays verbatim.** The most recent turns (two by default) are
   never included, so the model always sees the live edge of the conversation in
   full.
5. **Nothing eligible is not an error.** A short log, or one whose tool pairs
   glue every early turn to the kept tail, produces no event at all.

**What a writer refuses.** A plan whose range overlaps one an earlier summary
already replaced, a range naming a `seq` the log does not have, or an empty
summary is rejected with a typed error and **no bytes appended**, so a retry after
a corrected plan cannot double-write. A successful compaction appends exactly one
record and the dense `seq` sequence continues unbroken (§3).

**How a reader renders it.** The visible conversation is the **last**
`compaction/summary` event plus every event appended after it; older messages are
no longer rendered. Cumulative counters are **not** reset: `turns`, `steps`, tool
calls and token totals still describe the whole session, because compaction hides
messages, it does not un-happen work. A reader that needs to know precisely which
range a summary replaced reads `replaces`; a reader that ignores compaction
entirely still loads the log, it just renders more messages.

---

## 6. Forward compatibility: unknown types and unknown members

An old reader must survive a log written by a newer writer. Therefore:

1. **Unknown `type` values MUST be accepted** and preserved. A reader that
   understands only part of the catalogue keeps the event as an opaque record: it
   still counts for `seq` density, still participates in ordering, and is still
   returned by replay. It MUST NOT be dropped or turned into an error.
2. **Unknown members inside `payload` MUST be accepted** and MUST survive a
   decode/encode round trip unchanged. If an implementation re-encodes an event
   whose payload it does not fully understand, the unknown members must still be
   present with the same values. (A payload is carried as raw JSON, not
   re-modelled through a narrower struct.)
3. **Unknown members inside the envelope** (siblings of `v`/`seq`/...) MUST NOT
   cause an error, but MAY be dropped on re-encode. The v1 envelope is a closed
   set for writers, so a future envelope member is treated as a version change
   (§9) rather than an additive tweak.
4. **Unknown values of a known member** (e.g. a new `stopReason`) MUST NOT be
   rejected by the store or the reader. Interpretation is projection-level: an
   unrecognised value is carried through and rendered generically.
5. Nothing in v1 validates payload schemas, so "unknown fields" inside a payload
   are indistinguishable from "fields this version does not know yet" — which is
   the point.

Two conformance cases pin this: `ok_unknown_event_type` (a `future/...` type that
must round-trip) and `ok_unknown_payload_fields` (extra payload members that must
survive). `ok_unknown_envelope_member` asserts only that unknown envelope
members are not an error, because dropping them is allowed.

---

## 7. Versioning policy

- `v` states the envelope version of a **single record**, not of the file. Mixed
  versions are not valid: a log is v1 throughout. If a writer must change
  something a v1 reader cannot handle, it bumps `v` and starts a new log version
  (or migrates by writing a new file).
- **Additive changes do not bump `v`:** new event types, new payload members,
  new payload value enumerations, and new `reason`/`status` strings.
- **Breaking changes bump `v`:** changing a member's type or meaning, removing or
  renaming a member, making an optional member required, changing the meaning of
  `seq`, changing `ts` units or the id format, or adding a new envelope member
  (§6.3).
- A reader that meets a record with `v != 1` MUST fail closed: refuse the record
  with `unsupported event version`, do not guess, do not partially apply it.
- Event **types** are versioned by their `namespace` (`tool/call` in v1 stays
  compatible forever; a redesigned variant is `tool/call2` or lives behind a `v`
  bump).
- `hearth` itself is pre-1.0 (`0.0.0-dev`); **the event format is not** — v1 is
  frozen as of the M0 conformance suite, and format changes after that follow the
  rules above.

> Implementation status: **done** (M0). `internal/session.Open` refuses a log
> whose first unsupported-version record appears anywhere in it, returning
> `session.ErrUnsupportedVersion` (`"session: event log uses an unsupported
> format version"`), and leaves the file byte-for-byte untouched — no truncation
> and therefore no appending after a foreign prefix. `ReadFrom` re-checks `v` on
> every record for the same reason. Tests: `TestRefusesUnsupportedVersion`,
> `TestRefusesVersionUpgradeMidLog` in `internal/session/version_test.go`.
>
> Note that recovery uses a **cheap envelope check** (`seq`, `id`, `session`,
> `type`, `ts`), not the full `validate()` above: validating the payload of every
> record would rescan the whole file on open, which the 10k-event budget cannot
> afford (measured 102 ms for 39 MB / 10k events at 300 ms budget). Payload
> validity is therefore enforced on write and on `ReadFrom`, not during recovery.

---

## 8. Torn tail guarantee

A crash can leave the final line incomplete: no `\n`, or JSON cut mid-value. This
is the expected failure, not corruption, and it is recoverable.

**Reader rule (recovery).** Keep the longest **prefix** of complete, valid
records:

- Walk the file record by record, splitting on `\n`.
- A record without a terminating `\n` is incomplete: stop, it is not part of the
  prefix. (Even if its bytes happen to be valid JSON.)
- A record that fails to parse or fails any invariant in §3 is invalid: stop; it
  and everything after it are not part of the prefix.
- A blank line is ignored and does not stop the scan.
- Everything before the stopping point stays readable, in order, with its `seq`
  values unchanged.

**Writer rule (recovery, then append).** After computing the prefix, the writer
MAY truncate the file to exactly the byte offset where the last kept record ends,
then append new records with `seq = lastKeptSeq + 1`. This is the one permitted
non-append write. If it does not truncate, it MUST ignore (and never read back)
the garbage tail, and the next append must still leave the file parseable — in
practice, truncate.

Guarantees an implementation must provide:

1. A torn final record never makes earlier records unreadable.
2. Recovery never renumbers or deletes a complete record.
3. After recovery, the log is valid again and appending continues at
   `lastSeq + 1`.
4. A torn **middle** record is not recoverable by this rule (it is corruption,
   not a crash artifact); the prefix before it is still readable, and the tail is
   lost. A writer MUST NOT silently close the gap.
5. Recovery is idempotent: running it twice yields the same prefix and file size.

Fixture: `tests/fixtures/events/v1/torn-final-line.jsonl` — six complete records
followed by a seventh line truncated mid-JSON with no trailing newline. Opening
it with `session.Open` must yield six events (`seq` 1..6), truncate the file to
the six-record prefix, and let the next append produce `seq` 7.

Fixture discipline (enforced by the runner): every other fixture ends its last
record with a newline. A fixture whose tail is dropped must be torn **mid-value**
(invalid JSON), because a real crash does not stop neatly on a value boundary —
a dropped-but-complete JSON tail means a missing newline, and the runner fails
rather than silently losing that fixture's last event.

---

## 9. Reading a log: reference algorithm

```text
read_log(path) -> list<Event>:
    events = []
    offset = 0                      # byte offset of the next record
    for each chunk split by b'\n' in file(path):
        if chunk is not terminated by b'\n':      # torn tail
            break
        text = chunk without trailing '\r'
        if text is blank: offset += len(chunk)+1; continue
        rec = parse_json(text)                    # error -> stop (invalid JSON)
        if rec is not object                       # error -> stop (envelope)
            stop("does not match the v1 envelope")
        e = decode_envelope(rec)                   # missing/mistyped members
        validate(e)                                # v, id, ts, session, type
        if e.seq != len(events)+1: stop("not dense")
        if e.id in seen_ids: stop("duplicate event id")
        events.append(e); seen_ids.add(e.id)
        offset += len(chunk)+1
    return events, offset
```

`v != 1` stops the read with `unsupported event version` (§7). Duplicate
detection uses the set of ids already accepted in this log, not a global set.

Appending:

```text
append(store, session_id, type, payload) -> Event:
    e.seq  = store.last_seq + 1
    e.v    = 1
    e.id   = 32 random lowercase hex chars      # 128 bits, CSPRNG
    e.ts   = now() in Unix milliseconds
    e.session = session_id
    validate(e)
    write(json_dumps(e) + "\n")                # one line, LF
    fsync()                                    # per durability policy
    store.last_seq = e.seq
```

---

## 10. What is deliberately not in v1

- No signatures, checksums or encryption: the log is local state, protected by
  filesystem permissions.
- No per-record compression, no binary framing, no length prefixes.
- No schema registry or payload validation (see §4).
- No log rotation: one session, one file, for the life of the session.
- No cross-session ordering: `seq` is per log, `ts` is the only
  cross-session hint, and it is not monotonic across processes.

These are all additive later: a checksum could be a new envelope member behind a
`v` bump, and a payload schema registry needs no format change at all.

---

## 11. Conformance suite

- `tests/conformance/cases/events/*.json` — one case per file. Adding a case must
  never require touching Go code.
- `tests/fixtures/events/v1/*.jsonl` — realistic whole logs, including the torn
  tail.
- `tests/conformance/conformance_test.go` — the runner (Go, but it only uses the
  public session API; a port in another language is the intended use).

Case file schema (a closed schema: unknown members are a test failure, so a typo
fails loudly):

| Member | Type | Required | Meaning |
| --- | --- | --- | --- |
| `name` | string | yes | Must equal the file stem. |
| `description` | string | yes | Why the case exists (≥ 10 chars). |
| `lines` | array | yes | Input log. An **object** element is compacted to one JSONL line; a **string** element is used verbatim as a raw line (for malformed input, e.g. a truncated fragment). |
| `expect` | `"ok"` \| `"error"` | yes | Whether the input log must be accepted. |
| `errorContains` | string | iff `expect == "error"` | Substring the diagnostic must contain (§3 table). |
| `canonical` | array | no | Required re-encoded form, element per accepted event. Compared as **parsed JSON**, so key order and whitespace do not matter. Omitted when the contract does not fix the re-encoding. |

For every `ok` case the runner additionally asserts: each line decodes to the
envelope, `v == 1`, `seq` dense from 1, `id` non-empty and unique, `ts > 0`,
`session` and `type` non-empty; and that decode → encode → decode is a fixed
point with the payload byte-equal (after JSON normalisation). For every `error`
case it asserts the failure message contains `errorContains`.

A reimplementation passes when it reproduces the accept/reject decision, the
`canonical` bytes (as parsed JSON) and the fixture behaviour of §8 for all cases.