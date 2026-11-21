# 发布产物

本文描述 `.github/workflows/release.yml` 产出的二进制文件、它们主张什么又不主张什么，以及你自己怎么构建和校验。

## 1. 产物

工作流在匹配 `v*` 的标签上触发，也可以 `workflow_dispatch` 手动触发。每次构建都跑在 **原生**的 GitHub 托管 runner 上，并为该宿主编链出一个二进制：

| 产物 | Runner（宿主三元组） | CI 里构建并执行过？ | 内嵌 UI？ |
| --- | --- | --- | --- |
| `hearth-windows-amd64.exe` | `windows-latest`（`x86_64-pc-windows-msvc`） | 是 | 是，按字节标记校验过 |
| `hearth-linux-amd64` | `ubuntu-latest`（`x86_64-unknown-linux-gnu`） | 是 | 是，按字节标记校验过 |
| `hearth-darwin-arm64` | `macos-latest`（`aarch64-apple-darwin`） | 是 | 是，按字节标记校验过 |

`SHA256SUMS` 文件就是由这三个磁盘上的产物生成的，在 CI 里校验，然后挂在同一个 release 上。打标签时用 `gh release create --verify-tag` 创建 release；`workflow_dispatch` 运行只构建、校验并上传产物（它从不发布）。

## 2. 主张与不主张

**主张**（每一条都在每次托管运行中被检查）：

- 上面三个二进制在各自的 runner 上能编译、链接、**运行**（`--version`），并在输出里报出正确的操作系统与架构。
- `web/dist/` 是在 `cargo build` **之前**用 `npm ci && npm run build` 从 `web/` 构建出来的，所以 `crates/server/build.rs` 内嵌的是真 UI，而不是它自带的占位页面。`web/dist/index.html` 非空，`web/dist/assets/` 至少含一个 JS bundle，且链出的二进制里含 `assets/index-` 标记（Vite 带哈希的入口 bundle）。
- `SHA256SUMS` 与上传的文件一致；在发布任何东西之前，校验那一步会用 `sha256sum --check` 再验一遍。
- 发布只用内置的 `GITHUB_TOKEN`，并以 `v*` 标签为门槛。

**不主张**（别从产物里读出比这更多的东西）：

- **不发别的平台。** 特别地，**没有** `windows-arm64`、**没有** `linux-arm64`、**没有** `darwin-amd64`（Intel Mac）二进制。交叉编译目标（`x86_64-unknown-linux-gnu`、`aarch64-apple-darwin`）在开发机上只作为本地的**编译检查**存在（见 Makefile 的 `release` 目标）；两者都没有可用的目标链接器，而 `ring` 需要目标平台的 C 编译器，所以**不主张**有一个能跑起来的交叉链接二进制。`macos-latest` 是 Apple 芯片的 runner，因此只有 `darwin/arm64`。
- **标签没有烘进二进制。** `VERSION` 和 `COMMIT` 是 `crates/cli/src/lib.rs` 里的冻结常量（`"0.0.0-dev"` / `"unknown"`）。构建时只有 rustc 版本被 `crates/cli/build.rs` 注入。发布标签命名的是 release 和产物，它不改变 `--version`。
- **没有安装器、没有包管理源、没有自更新**子命令。升级就是下载一个新二进制。
- **没有代码签名，没有公证。** macOS 二进制未签名（见 §7）。
- **CI 里没有运行时/沙箱/性能认证。** 发布作业不跑 provider、工具、沙箱或长会话；那些和打包是两回事。
- **没有包含 LICENSE**，且 workspace 声明为 `UNLICENSED`（见 §8）。

## 3. 版本字符串

`hearth --version` 由 `hearth_cli::version_line()` 打印：

```
hearth <VERSION> (<COMMIT>) <os>/<arch> <rustc>
```

对已发布的二进制，字面前缀永远是 `hearth 0.0.0-dev (unknown) `（打包时那些常量不会被覆盖）。`<os>/<arch>` 反映二进制**为哪个平台构建**（`windows/amd64`、`linux/amd64`、`darwin/arm64`），`<rustc>` 是 `crates/cli/build.rs` 捕获的编译器版本。CI 的版本检查断言这个形状和预期的 OS/arch；它从不假定会出现一个带标签的版本号。

## 4. 本地构建

### 前置条件

- 带 MSVC 工具链的 Rust（本项目不支持 GNU——见 `AGENTS.md`）。
- 构建 Web UI 需要 Node.js 和 npm。
- Cargo 的 target 目录必须是**ASCII 路径**，通过环境变量设置：`.cargo/config.toml` 刻意什么都不钉（钉死的 `D:/...` 把第一个 Linux CI 作业搞挂了），所以自己把 `$env:CARGO_TARGET_DIR` 导到一个 ASCII 目录，或者让 `check.ps1` 替你推出来一个。非 ASCII 路径会让 `cl.exe` 以 `D8050` 失败。
- 本地 crate 访问走配置好的镜像，是离线的，所以要带 `--offline`。不要把 CI 命令里没有 `--offline` 这一点照搬到本机。

Web 的配方就是：

```
cd web; npm ci; npm run build
```

（`web/package.json` 用 Vite 构建到 `web/dist/`，`crates/server/build.rs` 在下一次 Cargo 构建时取走它。）

### PowerShell（本 workspace）

```powershell
# 从 workspace 根目录（hearth）开始：
cd web
npm ci
npm run build
cd ..

# 保持 target 目录是 ASCII。`.cargo/config.toml` 刻意什么都不钉 ——
# 钉死的路径把第一个 Linux runner 搞挂了 —— 所以裸 cargo build 时自己设。
# 非 ASCII 的 checkout 路径会让 MSVC 编 C 失败（错误 D8050），
# 影响的是 bundled SQLite 和 ring。
$env:CARGO_TARGET_DIR = "D:\hearth-target\hearth"

cargo build --release --offline

# 结果：
#   target\release\hearth.exe
```

`make web` 现在存在，会在 POSIX shell 里跑 `cd web; npm ci; npm run build`。在本 Windows 主机上，请直接跑上面那几条 `cd web` 命令。Makefile 的 `release` 目标只跑交叉目标的 `cargo check`（它不链接也不打包），所以它**不能**替代本节里的命令。

### Unix（仅供参考）

```sh
cd web && npm ci && npm run build && cd ..
CARGO_TARGET_DIR=/tmp/hearth-target cargo build --release --offline
# ./target/release/hearth
```

想手动在构建好的二进制上复现标记检查：

```sh
grep -aq 'assets/index-' target/release/hearth   # 退出 0 == 真 UI 已内嵌
```

## 5. 校验下载（SHA256SUMS）

从同一个 release 下载二进制**和** `SHA256SUMS`，然后跑你这平台上的检查。`SHA256SUMS` 是在 Linux runner 上用 `LC_ALL=C sha256sum hearth-*` 对上传的产物名生成的。

**Linux**

```sh
sha256sum --check SHA256SUMS
```

**macOS**

```sh
shasum -a 256 -c SHA256SUMS
```

**Windows（PowerShell）**

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

对不上就意味着这个文件不是 CI 发布的那一个——丢掉它。

## 6. 安装与升级

没有安装器，也没有 `self-update` 命令。下载你平台的产物、校验它（§5）、放进 `PATH`，然后（在 Unix 上）给它可执行权限：

```sh
chmod +x hearth-linux-amd64     # 或者 hearth-darwin-arm64
mv hearth-linux-amd64 hearth
./hearth --version
```

几条上手好用的命令：

```powershell
hearth doctor   # 检查配置、数据目录、端点可达性和 key
hearth web      # 提供内嵌 UI（默认 127.0.0.1:8420）
hearth chat     # 交互式会话
```

`hearth --version` 是快路径：它不读配置，所以在什么都没配好之前就能用。数据目录是 `$HEARTH_HOME`（如果设了），否则是 `~/.hearth`（Windows 上是 `%USERPROFILE%\.hearth`）。用户配置在 `$HEARTH_HOME/config.json`。provider 设置可以通过环境变量给：`HEARTH_BASE_URL`（默认 `https://api.openai.com/v1`）和 `HEARTH_API_KEY`；`hearth doctor` 会报它们的状态。

升级就是对新标签重复一遍下载加校验，然后替换二进制。会话日志住在数据目录里，升级时请留着它。

## 7. macOS：未签名二进制

`hearth-darwin-arm64` 产物**没有代码签名、没有公证**。macOS Gatekeeper 可能会隔离从浏览器下载的二进制，报出"无法打开，因为无法验证开发者"。两种办法：

- 右键点文件 → **打开** → 在对话框里确认 **打开**（一次性），或者
- 在终端里去掉隔离属性：

```sh
xattr -d com.apple.quarantine ./hearth-darwin-arm64
chmod +x ./hearth-darwin-arm64
```

只在校验完校验和（§5）之后才这么做。

## 8. 许可证

本 workspace 是 MIT 许可：`LICENSE` 放着 MIT 文本，`Cargo.toml` 声明 `license = "MIT"`。本项目源自的上游 DeepSeek Harness 树也是 MIT，所以这项授权与来源是相容的。

`web/node_modules/` 下以及依赖集内部的第三方许可证属于那些项目；这里的 MIT 授权只覆盖本项目自己的代码。

## 9. CI 验过什么，以及本地从没验过什么

**2026-11-20 端到端演练过**，用的是一次 `workflow_dispatch` 运行
（[44588872748](https://github.com/haijiawe/hearth/actions/runs/44588872748)），
那正是工作流文档里写的干跑模式：它构建、校验、上传，但因为 ref 不是标签而跳过 `gh release create`。三个原生作业全部成功：

| 产物 | 字节数 | sha256（前 16 位） |
| --- | --- | --- |
| `hearth-windows-amd64.exe` | 4,183,350 | `01984073a5253399` |
| `hearth-linux-amd64` | 4,584,901 | `b23c70ba319db59d` |
| `hearth-darwin-arm64` | 3,985,780 | `c4bd1fbfb582b66c` |

每个作业都打印了自己的 `--version`（`… windows/amd64 rustc 1.100.0`、`… linux/amd64 rustc 1.100.0`、`… darwin/arm64 rustc 1.100.0`）并通过了 `assets/index-` 标记检查；随后发布作业生成 `SHA256SUMS`、用 `sha256sum --check` 校验它，并上传了合计 12,754,274 字节的产物集。`Publish GitHub release` 那一步显示为**已跳过**，那正是这道闸在做它该做的事。

**在开发者的 Windows 主机上从没验过**，因此不主张：Linux 和 macOS 二进制超出 `--version` 之外的执行、任何 `windows-arm64` / `linux-arm64` / `darwin-amd64` 产物（一个都没产出）、签名/公证、安装器行为、自更新（两者都不存在）。本机用的是 MSVC 加 ASCII target 目录；CI 路径用的是在线 Cargo 加 `CARGO_TARGET_DIR=target`，而现在已知两条路都能产出二进制。