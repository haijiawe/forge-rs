---
dir: crates/sandbox
layer: L1
role: containment enforcers for job objects, Landlock, and sandbox-exec
public: [Mode, Spec, Support, Enforcer, Logger, NoopLogger, UnavailableError, new, available, maybe_exec_helper, mode_from_str]
may_import: [forge-core]
must_not: [forge-exec, forge-session, forge-llm, forge-tool, forge-protocol, forge-server, forge-cli]
gate: make arch
---

# forge-sandbox

## 做什么

`forge-sandbox` 决定“这台机器能限制什么”，并安装对应的执行器。它有三种模式：
`read-only`、`workspace-write`、`full-access`。`full-access` 是一个明确声明的
空操作（`enforced=false`）；其余模式必须真的执行，做不到就拒绝，绝不静默降级。

能力矩阵按平台实现：Windows 用 Job Object 限制进程树、进程/作业内存和进程数；
Linux 用 Landlock（必要时把 harness 自己 re-exec 成限制器）；macOS 用
`sandbox-exec` 的 Seatbelt profile。没有实现的平台一律拒绝非 full-access。

## 公开面

- `Mode`：`ReadOnly` / `WorkspaceWrite` / `FullAccess`；`as_str()` 返回配置拼写
  `"read-only"` / `"workspace-write"` / `"full-access"`；`mode_from_str()` 只认
  这三个拼写。
- `Spec { mode, workspace, allow_net, extra_write }`：workspace 必须是绝对路径、
  存在且是目录；文件系统根被拒绝（那等于 full-access）。
- `Support { filesystem, network, process_tree, enforced, detail }`：`detail` 必须
  说清楚缺什么。`available()` 报告能力但从不声称 `enforced`。
- `Enforcer`：`name()`、`support()`、`apply(&mut std::process::Command)`、
  `close()`，以及可选的 `abort_apply()` / `after_start(pid)`（Windows 的挂起
  启动与 adopt 需要后两者）。
- `new(spec, logger) -> Result<Box<dyn Enforcer>>`：唯一安装入口。失败语义：
  非法 spec → `ErrorKind::Invalid`；本机不支持或安装后无法执行 → `ErrorKind::Refused`；
  enforcer 已关闭 → `ErrorKind::Conflict`。
- `maybe_exec_helper()`：Linux Landlock 的 re-exec 子进程入口，`main` 必须最先调用；
  其他平台为空操作。

## 依赖与边界

只依赖 `forge-core` 和标准库；Linux 的 helper payload 用 workspace 的 serde/serde_json。
不得依赖 `forge-exec`：`apply` 接收的是 `std::process::Command`，由 exec 调用。
不得知道会话、LLM、工具或 HTTP。本 crate 只负责“安装限制”，进程树的日常终止
仍然由 `forge-exec` 负责；Windows 的 job object 是额外一层限制。

## 不要做

- 不要在 `filesystem=false` 时声称文件系统被限制；`workspace-write` 的部分执行
  必须如实报告缺哪一维。
- 不要在无法执行时静默放行；拒绝比“看起来安全”的假象好。
- 不要在非 Linux 平台依赖 Landlock 代码，也不要在非 Windows 平台依赖 kernel32。
- 不要用 `windows-sys` 等新依赖；FFI 绑定必须手写在 `extern "system"` 块内并带
  安全性注释。
- 不要在 Linux/macOS 的能力文案里省略 `unverified on this host`：这两个平台只在
  本机编译检查过，没有实测。
