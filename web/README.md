# hearth web UI (M3)

The browser UI for a `hearth` session: a session list, a virtualized transcript,
tool cards, the approval dialog and the composer. It talks to `hearth web` over
HTTP and one WebSocket per session. No UI kit, no CSS framework, no
virtualization library — the virtualizer and the stream reducer are in this
directory because they are the interesting part.

## Run it

The UI is embedded in the `hearth` binary from `web/dist` and served at `/`. For
development, run the API and Vite side by side:

```powershell
# terminal 1 — the API on 127.0.0.1:8848 (the token is printed to stderr)
cd D:\hearth
.\bin\hearth.exe web --addr 127.0.0.1:8848

# terminal 2 — the UI on 127.0.0.1:5173, proxying /api and /ws to :8848
$env:npm_config_cache="D:\hearth\.cache\npm"
$env:npm_config_store_dir="D:\hearth\.cache\pnpm-store"
cd web
npm run dev
```

Open the dev URL with the session token once:

```
http://127.0.0.1:5173/?token=<token>
```

`?token=` is read **once**, stored in `sessionStorage` (never `localStorage`),
and then removed from the address bar with `history.replaceState` — a token in
the URL ends up in screenshots, shell history and the browser history.
Reloads reuse the stored token. Routes are hash routes (`#/s/<id>`) so the
server needs no SPA fallback rule.

Point the dev proxy at a different server with
`$env:HEARTH_DEV_API="http://127.0.0.1:9000"`.

## Build, test, check

```powershell
$env:npm_config_cache="D:\hearth\.cache\npm"
cd web
npm ci            # exact pinned versions, lockfile committed
npm run typecheck # tsc --noEmit
npx vitest run    # tests
npm run build     # -> web/dist/index.html + web/dist/assets/*
```

The npm cache must live inside the repo: the sandbox blocks the default cache
under the user profile (`E:\Cache\npm`), so a bare `npm install` fails. The
variables above are the fix, not a preference.

`npm run build` writes `web/dist`, which the `hearth-cli` binary embeds with
`include_dir!` at compile time — a running binary does not read this directory,
so a rebuilt bundle needs a rebuilt binary (`cargo build --release -p hearth-cli`
and copy the artifact to `bin/hearth.exe`). Source maps stay off (they would ship
the sources in the binary).

## What is where

| Path | Owns |
| --- | --- |
| `src/api/protocol.ts` | the wire shapes, mirroring `crates/protocol/src/protocol.rs` — the two change in one commit (the Go generator behind this file did not come across) |
| `src/api/types.ts` | the single import door for protocol types (keeps tests independent of the generator) |
| `src/api/events.ts` | the event envelope and payload shapes from `docs/contracts/events-v1.md`, with an unknown-type fallback |
| `src/api/client.ts` | typed fetch wrapper (Bearer + `X-Hearth-Protocol`), token capture, `Idempotency-Key` on POSTs |
| `src/api/stream.ts` | the stream reducer: snapshot → event → delta, dedup by seq, gap detection |
| `src/api/transport.ts` | the WebSocket itself: reconnect with backoff, `?after=<lastSeq>`, pings |
| `src/state/rows.ts` | events → transcript rows (tool call/result and approval request/decision merge) |
| `src/state/sessionStore.ts` | per-session store for `useSyncExternalStore`, one render per animation frame |
| `src/virtual/` | the virtualizer: Fenwick offset index, measured heights by row key, anchor restore |
| `src/components/` | the screens and rows (`Transcript`, `TranscriptRow`, `ToolCard`, `ApprovalDialog`, `Composer`, `StatusBar`, `SessionList`, `SessionView`) |
| `src/markdown/parse.ts` | block-level markdown parser (no syntax highlighting in M3) |

## Rendering rules this UI is held to

From the M3 design in the retired tree's `02-architecture.md` §10.1; the rules are
enforced by tests here, and [../docs/web-ui.md](../docs/web-ui.md) records the
measurements that hold them to account.

1. **Only the visible window ±1 screen is in the DOM**, however long the
   transcript. Variable row heights are measured after commit and cached by row
   key; scroll position is anchored so prepending older rows does not jump.
2. **At most one render per animation frame while streaming.** Stream frames are
   applied to the reducer synchronously (ordering cannot wait) but the React
   snapshot is published from `requestAnimationFrame`; a burst of delta frames
   costs one render.
3. **Markdown is parsed per block and memoized per block.** A growing assistant
   row re-parses its last block, not the whole transcript.
4. **Deltas are advisory.** `assistant/message` carries the authoritative text
   (`docs/contracts/api.md` §4.8); a delta whose seq never becomes a message
   event is dropped rather than shown as a ghost.
5. **Tool output is bounded.** The server caps it at 64 KiB and says so with
   `truncated: true`; the card shows that marker, collapses to the first and
   last 20 lines, and shows a machine error code when the tool failed.
6. **No `any` in exported signatures, no token in `localStorage`.**

## A seq gap is a server bug, not a UI state

The stream must never apply an event twice and never skip one. The reducer
tracks `lastSeq`, drops anything `<= lastSeq` (reconnect replay), and treats a
`seq` beyond `lastSeq + 1` as a protocol violation: it stops trusting the socket
and refetches `GET /events` from the last applied seq until caught up. The status
bar shows `lastSeq` precisely so this is observable.

## Open items

- Compaction: the transcript renders a `compaction/summary` row, but there is no
  "show the compacted-away turns" control in M3.
- Older-history paging: the contract has no backward `GET /events` cursor, so the
  UI always loads the full log from seq 1. The virtualizer's prepend path and its
  scroll-restore behaviour are implemented and tested; they keep working when a
  backward cursor arrives.