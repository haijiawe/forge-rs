# Release artifacts

This document describes the binaries produced by `.github/workflows/release.yml`,
what they do and do not claim, and how to build and verify them yourself.

## 1. Artifacts

The workflow fires on tags matching `v*` and on `workflow_dispatch`. Each build
runs on a **native** GitHub-hosted runner and links a binary for that host:

| Artifact | Runner (host triple) | Built and executed in CI? | UI embedded? |
| --- | --- | --- | --- |
| `forge-windows-amd64.exe` | `windows-latest` (`x86_64-pc-windows-msvc`) | yes | yes, verified by byte marker |
| `forge-linux-amd64` | `ubuntu-latest` (`x86_64-unknown-linux-gnu`) | yes | yes, verified by byte marker |
| `forge-darwin-arm64` | `macos-latest` (`aarch64-apple-darwin`) | yes | yes, verified by byte marker |

A `SHA256SUMS` file is generated from exactly these three bytes-on-disk
artifacts, checked in CI, and attached to the same release. On a tag the release
is created with `gh release create --verify-tag`; a `workflow_dispatch` run only
builds, checks and uploads artifacts (it never publishes).

## 2. Claims and non-claims

**Claims** (each one is checked on every hosted run):

- The three binaries above compile, link, **run** (`--version`) on their native
  runner, and report the correct OS/architecture in that output.
- `web/dist/` is built from `web/` with `npm ci && npm run build` *before*
  `cargo build`, so `crates/server/build.rs` embeds the real UI instead of its
  built-in placeholder page. `web/dist/index.html` is non-empty,
  `web/dist/assets/` contains at least one JS bundle, and the linked binary
  contains the `assets/index-` marker (the hashed Vite entry bundle).
- `SHA256SUMS` matches the uploaded files; the checksum step re-verifies them
  with `sha256sum --check` before anything is published.
- Publication uses only the built-in `GITHUB_TOKEN`, gated on a `v*` tag.

**Non-claims** (do not read more into the artifacts than this):

- **No other platform is shipped.** In particular there is **no**
  `windows-arm64`, **no** `linux-arm64`, and **no** `darwin-amd64` (Intel Mac)
  binary. Cross-compilation targets (`x86_64-unknown-linux-gnu`,
  `aarch64-apple-darwin`) exist on developer machines only as a local
  *compile check* (see the `release` target in the Makefile); there is no
  target linker for either, and `ring` needs a C compiler for the target, so a
  working cross-linked binary is **not** claimed. `macos-latest` is an
  Apple-silicon runner, hence `darwin/arm64` only.
- **The tag is not baked into the binary.** `VERSION` and `COMMIT` are frozen
  constants in `crates/cli/src/lib.rs` (`"0.0.0-dev"` / `"unknown"`). Only the
  rustc version is injected at build time by `crates/cli/build.rs`. The release
  tag names the release and the artifacts; it does not change `--version`.
- **No installer, no package-manager feed, no self-update** subcommand is
  provided. Upgrading means downloading a new binary.
- **No code signing, no notarization.** The macOS binary is unsigned (see §7).
- **No runtime/sandbox/performance certification in CI.** The release job does
  not exercise providers, tools, sandboxes or long-running sessions; those are
  separate concerns from packaging.
- **No LICENSE is included** and the workspace is declared `UNLICENSED` (see §8).

## 3. Version string

`forge --version` prints, from `forge_cli::version_line()`:

```
forge <VERSION> (<COMMIT>) <os>/<arch> <rustc>
```

For a released binary the literal prefix is always
`forge 0.0.0-dev (unknown) ` (the constants are not overridden at packaging
time). `<os>/<arch>` reflects the platform the binary was *built for*
(`windows/amd64`, `linux/amd64`, `darwin/arm64`) and `<rustc>` is the compiler
version captured by `crates/cli/build.rs`. The CI version check asserts this
shape and the expected OS/arch; it never assumes a tagged version appears.

## 4. Local build

### Prerequisites

- Rust with the MSVC toolchain (GNU is not supported on this project — see
  `AGENTS.md`).
- Node.js and npm for the web UI.
- Cargo's target directory must be an **ASCII path**. `.cargo/config.toml`
  already points it at `D:/forge-rs-target/forge-rs`; keep that, or set
  `$env:CARGO_TARGET_DIR` to another ASCII directory. A non-ASCII path makes
  `cl.exe` fail with `D8050`.
- Local crate access is offline through the configured mirror, so use
  `--offline`. Do not copy the CI command's lack of `--offline` to this box.

The web recipe is exactly:

```
cd web; npm ci; npm run build
```

(`web/package.json` builds with Vite into `web/dist/`, which
`crates/server/build.rs` picks up on the next Cargo build.)

### PowerShell (this workspace)

```powershell
# From the workspace root (forge-rs):
cd web
npm ci
npm run build
cd ..

# Keep the target dir ASCII; .cargo/config.toml already sets this, but an
# explicit override is fine and documents the requirement:
$env:CARGO_TARGET_DIR = "D:\forge-rs-target\forge-rs"

cargo build --release --offline

# Result:
#   target\release\forge.exe
```

`make web` now exists and runs `cd web; npm ci; npm run build` under a POSIX shell. On this Windows host run the `cd web` commands above directly instead. The Makefile's `release` target only runs cross-target `cargo check`s (it does not link or package anything), so it is **not** a substitute for the commands in this section.

### Unix (for reference)

```sh
cd web && npm ci && npm run build && cd ..
CARGO_TARGET_DIR=/tmp/forge-rs-target cargo build --release --offline
# ./target/release/forge
```

To reproduce the marker check manually on a built binary:

```sh
grep -aq 'assets/index-' target/release/forge   # exit 0 == real UI embedded
```

## 5. Verifying a download (SHA256SUMS)

Download the binary **and** `SHA256SUMS` from the same release, then run the
check for your platform. `SHA256SUMS` is generated on the Linux runner with
`LC_ALL=C sha256sum forge-*` over exactly the uploaded artifact names.

**Linux**

```sh
sha256sum --check SHA256SUMS
```

**macOS**

```sh
shasum -a 256 -c SHA256SUMS
```

**Windows (PowerShell)**

```powershell
Get-Content .\SHA256SUMS | ForEach-Object {
    if ($_ -match '^([0-9a-fA-F]{64})\s+\*?(.+)$') {
        $expected = $matches[1].ToLowerInvariant()
        $file = $matches[2]
        if (-not (Test-Path $file)) {
            Write-Error "missing: $file"; return
        }
        $actual = (Get-FileHash -Algorithm SHA256 -Path $file).Hash.ToLowerInvariant()
        if ($actual -eq $expected) {
            Write-Host "OK   $file"
        } else {
            Write-Error "FAIL $file (expected $expected, got $actual)"
        }
    }
}
```

A mismatch means the file is not the one published by CI — discard it.

## 6. Install and upgrade

There is no installer and no `self-update` command. Download the artifact for
your platform, verify it (§5), place it on your `PATH`, and (on Unix) make it
executable:

```sh
chmod +x forge-linux-amd64     # or forge-darwin-arm64
mv forge-linux-amd64 forge
./forge --version
```

Useful first commands:

```powershell
forge doctor   # checks config, data dir, endpoint reachability and key
forge web      # serves the embedded UI (default 127.0.0.1:8420)
forge chat     # interactive session
```

`forge --version` is a fast path: it does not read configuration, so it works
before anything is set up. The data directory is `$FORGE_HOME` when set,
otherwise `~/.forge` (`%USERPROFILE%\.forge` on Windows). User config lives at
`$FORGE_HOME/config.json`. Provider settings can be supplied through the
environment: `FORGE_BASE_URL` (default `https://api.openai.com/v1`) and
`FORGE_API_KEY`; `forge doctor` reports their status.

To upgrade, repeat the download + verification for a newer tag and replace the
binary. Session logs live in the data directory, so keep it when upgrading.

## 7. macOS: unsigned binary

The `forge-darwin-arm64` artifact is **not code signed or notarized**. macOS
Gatekeeper may quarantine a binary downloaded from the browser, producing
"cannot be opened because the developer cannot be verified". Either:

- Right-click the file → **Open** → confirm **Open** in the dialog (one time), or
- remove the quarantine attribute from a terminal:

```sh
xattr -d com.apple.quarantine ./forge-darwin-arm64
chmod +x ./forge-darwin-arm64
```

Do this only after verifying the checksum (§5).

## 8. No LICENSE

This workspace contains **no LICENSE file** and declares
`license = "UNLICENSED"` in `Cargo.toml`. No copyright or redistribution grant
is provided with these artifacts; do not redistribute them under an assumed
license. Any third-party license notices under `web/node_modules/` belong to
those dependencies and are not a license for this project.

## 9. What CI verifies vs. what was never verified locally

**Verified on hosted runners every release run:** native compile+link for the
three targets, `npm ci && npm run build`, non-empty `index.html`, existence of
JS bundles, the `assets/index-` marker inside each binary, `--version` shape and
OS/arch, SHA256SUMS generation and `sha256sum --check`, and on tags the
`gh release create --verify-tag` call.

**Never verified on the developer Windows host** (and therefore not claimed
here): execution of the Linux and macOS binaries, any `windows-arm64` /
`linux-arm64` / `darwin-amd64` artifact (none is produced), cross-linking with
`ring`'s C code, signing/notarization, installer behaviour, and self-update
(neither exists). The local box uses MSVC + offline mirror + an ASCII target
dir; the CI path differs (online Cargo, `CARGO_TARGET_DIR=target`) and is
described here from the workflow, not from a local run of the full pipeline.
