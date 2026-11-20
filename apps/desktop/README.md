# hearth desktop — the shell around the local server

A Tauri 2 window. It starts `hearth web` as a child process, waits for the server
to print the URL it is serving, and opens a window at that URL. Everything in the
window is the same web UI a browser gets; this crate adds a window, a taskbar
entry and an icon, and nothing else.

## Why it is a launcher and not an application

* The UI talks to the server over HTTP with a bearer token. Running that over the
  loopback socket is the same path a browser uses, so a defect in the shell
  cannot quietly diverge from a defect in the browser.
* The server owns the sandbox, the approval prompt and the event log. A shell
  that linked the server would have to be trusted with all three, and a window
  frame has no business being trusted with anything.
* `hearth` stays one binary that runs anywhere. The shell is an extra: a machine
  without a desktop is not made worse by it, and `cargo test --workspace` never
  needs a GUI toolkit.

## Building

```powershell
cd D:\hearth\apps\desktop
cargo build --offline            # debug
cargo build --offline --release  # what you would ship
```

This is a **separate cargo workspace** (`[workspace]` in its own `Cargo.toml`).
It is not a member of the root workspace on purpose: `tauri` on Linux needs
`webkit2gtk`, and a member would make every Linux CI job install a GUI toolkit
to compile a window border. The boundary between the shell and the server is "run
this process", which has nothing for a type checker to check.

The icon set is generated, not drawn by hand:

```powershell
& "…\python.exe" apps\desktop\icons\make-icons.py
```

It needs Pillow and writes `icon.ico` (16–256 px) plus the three PNGs
`tauri.conf.json` lists. Run it only when the mark changes; the outputs are
committed because a build must not need Python.

## Running

```powershell
.\apps\desktop\target\debug\hearth-desktop.exe --model deepseek-chat
```

Every argument is forwarded to `hearth web`. The shell adds `--no-browser`, so
the window is the browser and Edge does not open behind it.

`HEARTH_BIN` overrides which binary is started. Without it the shell uses a
`hearth.exe`/`hearth` sitting next to itself, then `PATH`. A shell that guessed
silently would start a *different* build than the one beside it, which is the
kind of defect that makes a fixed bug look unfixed.

On a model the server refuses to start without one; `hearth web` exits with
`no model configured: pass --model <name> or set "model" in config.json`, and the
shell shows that sentence in the window instead of a blank page.

## What the window does, and when

1. `setup` spawns a worker thread. The main thread is never blocked, because the
   main thread is the one that pumps the window's messages.
2. The worker starts the server with stdin null and stderr piped, then reads
   stderr until a line starts with `hearth web: open `. The server picks its own
   port, so the shell cannot choose one; the URL is the server's own statement
   that it is listening.
3. Back on the **main thread** (through `run_on_main_thread`) the window is
   created directly at that URL, 1440×900, minimum 900×600.
4. If the server could not be started, the window opens on a `data:text/html`
   page that says why. The error page is a document of its own rather than a
   patch applied to a page that is still loading.

Both of those last two details were measured the hard way:

* Creating the window from the waiting thread loads the page (read through
  WebView2's debugging port: `#root` held 7,819 bytes of markup, the stylesheet
  had 267 rules, `errors` was empty) and then paints nothing but black, because
  the WebView2 controller belongs to the thread that runs the event loop.
* Opening a placeholder page and navigating it once the server is up leaves a
  blank document behind when the two loads overlap: the new page is laid out
  against a viewport the old document had not finished setting up.

The window is what the CSP in `tauri.conf.json` covers. It is deliberately
narrow — `default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:` —
because the only document Tauri itself serves is the fallback page in `ui/`.

## Closing

The server is a child process, and the exit handler kills and waits for it, so a
closed window does not leave a listener behind on a loopback port. This is
verified, not assumed: with the window closed, `GET /api/capabilities` on the old
port answers `000` (nothing is listening) and the `hearth.exe` process is gone.

## What this crate does not do

* It does not bundle a model, a token or a configuration. First run wants
  `--model` or a `config.json`.
* It does not open a browser, and it does not own a "quit" menu item: closing the
  window is quitting.
* It does not build installers here. `cargo build` produces
  `target/{debug,release}/hearth-desktop.exe`; `tauri.conf.json` has the bundle
  metadata, but no `tauri` CLI is installed on this machine and none is needed to
  build.
* It is not covered by `check.ps1` or CI. See "Building" for why; the shell has no
  tests of its own, so nothing here can pass a gate it is not part of.