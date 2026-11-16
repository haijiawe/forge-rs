---
dir: crates/sandbox
layer: L1
role: containment enforcers for job objects plus AppContainers, Landlock, and sandbox-exec
public: [Mode, Spec, Support, Enforcer, Logger, NoopLogger, UnavailableError, new, available, appcontainer_available, prune_appcontainer_profiles, maybe_exec_helper, mode_from_str]
may_import: [forge-core]
must_not: [forge-exec, forge-session, forge-llm, forge-tool, forge-protocol, forge-server, forge-cli]
gate: make arch
---

# forge-sandbox

## 做什么

`forge-sandbox` 决定“这台机器能限制什么”，并安装对应的执行器。它有三种模式：
`read-only`、`workspace-write`、`full-access`。`full-access` 是一个明确声明的
空操作（`enforced=false`）；其余模式必须真的执行，做不到就拒绝，绝不静默降级。

能力矩阵按平台实现：Windows 是两层环——Job Object 管进程树、进程/作业内存和进程数，
AppContainer 管这个进程能摸到哪些文件和网络（workspace 的写权限、`allow_net` 才给的
`internetClient` 能力）；
Linux 用 Landlock（必要时把 harness 自己 re-exec 成限制器）；macOS 用
`sandbox-exec` 的 Seatbelt profile。没有实现的平台一律拒绝非 full-access。

## 公开面

- `Mode`：`ReadOnly` / `WorkspaceWrite` / `FullAccess`；`as_str()` 返回配置拼写
  `"read-only"` / `"workspace-write"` / `"full-access"`；`mode_from_str()` 只认
  这三个拼写。
- `Spec { mode, workspace, allow_net, extra_write, extra_read, wrapper }`：workspace 必须是绝对
  路径、存在且是目录；文件系统根被拒绝（那等于 full-access）。`wrapper` 是“`main`
  第一句调用 `maybe_exec_helper` 的可执行文件”，默认 `None` 表示本进程（`forge`
  自己）；把本 crate 当库嵌进别的宿主才需要设置它。`extra_read` 是“workspace 之外
  仍可读”的路径，只对默认拒绝读的平台有意义（Windows 的 AppContainer 从零可读开始；
  Linux 的 Landlock 策略只管写、macOS 的 profile 已经 `allow file-read*`，所以那两个
  平台上是空操作）。不存在的读路径会被跳过而不是拒绝：这份清单描述的是“这台机器
  通常有什么”，机器上没有 `~/.npmrc` 不该变成沙箱拒绝启动。
- `Support { filesystem, network, process_tree, enforced, detail }`：`detail` 必须
  说清楚缺什么。`available()` 报告能力但从不声称 `enforced`；
  `appcontainer_available()` 用无副作用的 derive 探测回答“这台 Windows 能不能造
  AppContainer”（拿创建 profile 当探测会与并发调用互相踩，并在能用的机器上偶发
  报 `filesystem: false`）。
- `Enforcer`：`name()`、`support()`、`apply(&mut std::process::Command)`、
  `close()`，以及可选的 `abort_apply()` / `after_start(pid)`（Windows 的挂起
  启动与 adopt 需要后两者）。
- `new(spec, logger) -> Result<Box<dyn Enforcer>>`：唯一安装入口。失败语义：
  非法 spec → `ErrorKind::Invalid`；本机不支持或安装后无法执行 → `ErrorKind::Refused`；
  enforcer 已关闭 → `ErrorKind::Conflict`。
- `maybe_exec_helper()`：wrapper 子进程入口（Linux Landlock、Windows AppContainer），
  `main` 必须最先调用；其他平台为空操作。
- `prune_appcontainer_profiles() -> Result<PruneReport>`（Windows）：删掉本工具在该
  用户下创建的 `forge.sandbox.*` 包身份，返回 `{found, removed, failed}`。profile 服务
  没有枚举 API，所以索引就是 `%LOCALAPPDATA%\Packages` 里的目录名。这是维护而不是
  打扫：攒到约九十个身份后，服务会成段时间拒绝新名字。

## 依赖与边界

只依赖 `forge-core` 和标准库；wrapper payload 用 workspace 的 serde/serde_json。
不得依赖 `forge-exec`：`apply` 接收的是 `std::process::Command`，由 exec 调用。
不得知道会话、LLM、工具或 HTTP。本 crate 只负责“安装限制”，进程树的日常终止
仍然由 `forge-exec` 负责；Windows 的 job object 与 AppContainer 是额外两层限制。

## 不要做

- 不要在 `filesystem=false` 时声称文件系统被限制；`workspace-write` 的部分执行
  必须如实报告缺哪一维。
- 不要在无法执行时静默放行；拒绝比“看起来安全”的假象好。
- 不要在非 Linux 平台依赖 Landlock 代码，也不要在非 Windows 平台依赖 kernel32。
- 不要用 `windows-sys` 等新依赖；FFI 绑定必须手写在 `extern "system"` 块内并带
  安全性注释。
- 不要在 `wrap_command` 里 `env_clear()`，也不要指望 `creation_flags` 能读回来：
  前者让 wrapper 失去 `SystemRoot`（子进程报 203），后者没有 getter，所以
  `CREATE_SUSPENDED` 必须在包装完成之后再设。
- 不要用 `CreateAppContainerProfile` 当能力探测，也不要相信 `Derive…` 对**没有
  创建过**的名字给出的 SID：前者与并发踩，后者让 `CreateProcessW` 报 2 或 87。
- 不要在 profile 已存在时重新创建它：先看
  `%LOCALAPPDATA%\Packages\<name>` 是否存在，存在就 `Derive…` 采纳。profile 服务
  会成段时间拒绝新名字（本机攒到约九十个 `forge.sandbox.*` 后出现过），而采纳一个
  已存在的身份从不受影响；只有真的需要新身份时才付那 8 秒退避重试，最后才退到
  全用户共享的 `forge.sandbox`（共享身份不再隔离两个 workspace，必须在文档里说）。
- 不要在 Linux/macOS 的能力文案里省略 `unverified on this host`：这两个平台只在
  本机编译检查过，没有实测。
