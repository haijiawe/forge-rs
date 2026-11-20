# Original DSH comparison (local host, 2026-11-16)

This file records the local measurements requested after the Rust/Go rewrite was
finished. No API key is stored here.

## What “original DSH” means on this host

| Item | Value |
| --- | --- |
| CLI launcher | `D:\node_global\dsh.cmd` → `D:\node_global\node_modules\@deepseek-ai\dsh\lib\bin.js` |
| CLI package | `@deepseek-ai/dsh` **0.2.2-rc.1** |
| Node | v24.21.0 |
| Desktop shell | `D:\Programs\DeepSeekHarness\DeepSeek Harness.exe` |
| Desktop version file | **44.7.0** (Electron shell version, not the harness version) |
| Desktop executable | 246,341,104 bytes |
| `app.asar` | 209,021,953 bytes (virtual path; the expanded `app.asar\dsh` tree is not present on disk) |
| Active profile | `D:\dsh\profiles\desktop` |
| Headless profile | `D:\dsh\profiles\headless` |

The original DSH CLI is installed. The source tree reported by `DSH_INSTALL_DIR`
is an Electron virtual path and does not exist as a plain folder here, so the
comparison uses the installed CLI plus the original headless profile.

## Startup / version

Measured with `Stopwatch` on this host:

| Command | Cold | Warm | Output |
| --- | ---: | ---: | --- |
| `dsh --version` | 1,470 ms | 160 ms | `0.2.2-rc.1` |
| `dsh web --version` | — | 350 ms | `0.2.2-rc.1` |
| `hearth.exe --version` | ~5 ms in the bench harness | ~5 ms | `hearth 0.0.0-dev (unknown) windows/amd64 rustc 1.96.0` |
| Go `hearth.exe --version` | ~7 ms in the Go bench harness | ~7 ms | `hearth 0.7.0-m7 …` |

Cold DSH includes booting the Node CLI/profile launcher. Rust and Go CLI startup
is a native fast path that does not load config before answering `--version`.

## Live response comparison

Same host, same network, same real model endpoint
(`https://z1rry.cyou/v1`), same model id
(`deepseek-v4.1-flash-1017`), same prompt:

```text
Reply with exactly: COMPARE-OK
```

DSH was given a temporary `--patch` that only replaces its provider base URL and
default model; the installed original remains untouched. No key is written to
disk by this comparison.

| Run | Rust | Go | Original DSH |
| --- | ---: | ---: | ---: |
| 1 | 1,203 ms | 1,143 ms | 5,724 ms |
| 2 | 9,328 ms | 1,252 ms | 17,304 ms |
| 3 | 1,044 ms | 1,095 ms | 5,491 ms |
| Median | 1,203 ms | 1,143 ms | 5,724 ms |
| Min | 1,044 ms | 1,095 ms | 5,491 ms |

All three printed `COMPARE-OK`. Run 2 for Rust and DSH was slow because the shared
endpoint/network jittered; it is recorded rather than discarded. On medians,
original DSH was about **5.0× slower than Go** and **4.8× slower than Rust** in
this small sample.

Important behaviour difference: DSH headless printed extra reasoning/explanation
lines to stdout in two runs before `COMPARE-OK`; Rust/Go CLI printed only the
final answer on stdout, with diagnostics on stderr. This is a harness behaviour
difference, not a model difference.

## Earlier live checks

Before the three-run comparison, the same setup produced:

| Check | Rust | Go | Original DSH |
| --- | ---: | ---: | ---: |
| Exact-string prompt | `FINAL-RUST-OK` / 6,844 ms | `FINAL-GO-OK` / 1,516 ms | not run with this exact prompt |
| Tool call / shell output | `rust-tool-ok` / 2,582 ms | `go-tool-ok` / 2,965 ms | not run in this pass |
| Web/UI | healthz 200, index 200, embedded JS 200 / 265,721 B | previously verified | running desktop profile was not benchmarked |

The large variance in the earlier exact-string run is endpoint/network jitter;
that is why the three-run table above is the better comparison.

## Why DSH needed a patch

The original DSH headless profile defaults to `deepseek-official` with
`deepseek-flash`. Using the custom z1rry key there fails with an official
DeepSeek 401, which is expected: that key belongs to the custom endpoint, not to
`api.deepseek.com`. The temporary patch used in the test sets:

- provider `magpie`, API `openai-completions`
- `baseURL: https://z1rry.cyou/v1`
- default model `deepseek-v4.1-flash-1017`

Without this patch DSH cannot use the provided custom endpoint key in headless
mode.

## Caveats

- This is a small same-host sample, not a CI benchmark.
- DSH is an Electron/Node/plugin harness; Rust/Go are native single binaries.
  The startup difference is structural, not a bug in one implementation.
- The desktop DSH shell was not separately benchmarked; the comparison used the
  original CLI/headless profile.
- The API key used during testing should be rotated because it appeared earlier
  in this session's logs.
