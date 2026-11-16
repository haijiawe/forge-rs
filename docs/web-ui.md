# The web transcript under load

The claim this settles: **5,000 messages in one session still paint at 60 fps.**
It was the last unmeasured line in the porting checklist, because measuring it
needs a real browser and the earlier unit tests could not tell a virtualised list
from a broken one.

## Method

Everything runs against the shipped binary and the shipped bundle — no test
harness, no mock:

1. `forge` generated a session log in the product's own envelope: 2,500 turns,
   5,000 messages, 10,001 events, 1,997,455 bytes, written by hand so it is byte
   shaped like a real log (canonical key order, dense `seq`).
2. `bin/forge.exe web --no-browser --addr 127.0.0.1:8517` served it.
3. Microsoft Edge (`--headless=new`, `--window-size=1400,900`,
   `ANGLE (NVIDIA GeForce RTX 4060 Direct3D11)`) opened the real UI over CDP.
4. `requestAnimationFrame` ticks for 3 s while idle, then 3 s while scrolling the
   transcript 120 px per frame.

## Results

Three consecutive runs, same host, same session:

| Run | load | rows in DOM | idle fps | scroll fps | longest task | heap | nodes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 686 ms | 27 | 54.5 | 60.0 | 75 ms | 18.7 MB | 208 |
| 2 | 649 ms | 27 | 56.5 | 60.0 | 55 ms | 17.2 MB | 208 |
| 3 | 610 ms | 27 | 58.3 | 60.0 | 0 ms | 17.9 MB | 208 |

The scroll figure is the one the claim was about, and it is a flat 60 fps: the
transcript scrolls a **480,130 px** document by mounting 27 rows and 208 DOM
nodes. Idle sits a few frames below 60 because headless Chromium throttles its
own compositor when nothing is animating; scrolling does not.

## What a browser found that the test suite could not

All three defects were invisible to every test in the repo, and each one made the
UI's live stream fail in a real browser:

1. **No cookie was ever set.** `api.md` §2 says the browser presents the
   `HttpOnly` `forge_token` cookie and "the operator sets it out of band" — but
   `forge web` prints a URL with `?token=…` and nothing bridged the two, so the
   stream answered `401` on every retry. The shell now answers
   `Set-Cookie: forge_token=…; HttpOnly; SameSite=Strict; Path=/` when the
   `token` query parameter matches (a wrong token still serves the shell, without
   the cookie).
2. **The handshake could not carry a version.** `api.md` §3 said WebSocket
   clients send `X-Forge-Protocol` on the upgrade — a header a browser cannot set.
   A client may now offer the `forge.v1` subprotocol instead, and the server
   echoes it in the `101` (RFC 6455 §4.2.2). Neither signal is still `400`.
3. **`Sec-WebSocket-Accept` was unpadded.** `base64_encode` in the server never
   pads because every other caller wants unpadded base64url (the token, the
   cursor). SHA-1 is 20 bytes, so a correct accept always ends in one `=` — and a
   browser compares it byte for byte. The repo's own tests hid this twice: the
   canonical RFC sample key was asserted *without* its `=`, and the WebSocket
   corpus trimmed `=` from both sides before comparing. Both assertions now
   demand padding, and `a_browser_style_upgrade_authenticates_by_cookie_and_names_its_subprotocol`
   pins a key whose accept needs it.

## What this does not measure

- Only headless Edge on this host, on Windows/amd64. No other browser, no
  software rasteriser, no low-end GPU.
- Idle fps is reported as measured (54–58); it is compositor throttling, not
  dropped work, but it is not a 60 fps claim.
- Scrolling was driven programmatically. A human flick, a trackpad, and a
  momentum scroll were not measured.
- The transcript opens at `scrollTop: 0` for a large backlog, with a `↓ latest`
  button — same code path as the retired Go build (`forge-go/web/src/components/Transcript.tsx`,
  deleted 2026-11-17), so this is the product's design rather than a porting
  regression. It was verified by loading an 81-event session and a 10,001-event
  session: both open at the top.