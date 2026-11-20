---
dir: crates/exec
layer: L1
role: process spawn, stdio capture, timeout, process-tree kill, sandbox hook
public: [Command, Result, Runner, Pipe, Sandbox, DEFAULT_MAX_OUTPUT_BYTES, ENV_INHERIT]
may_import: [hearth-core]
must_not: [hearth-sandbox, hearth-session, hearth-llm, hearth-tool, hearth-protocol, hearth-server, hearth-cli]
gate: make arch
---

# hearth-exec

## 做什么

`hearth-exec` 只做一件事：把一条命令变成一个可观察、可终止的进程树。它负责
启动子进程、把 stdin 送进去、按上限收集 stdout/stderr、在超时或取消时杀掉整棵
进程树，并把结果（退出码、输出、耗时、是否截断）作为数据返回。

非零退出码不是错误：`exit 3` 会得到 `exit_code = 3` 和 `Ok(Result)`。只有启动
失败、超时、取消、非法参数和 sandbox 拒绝才返回 `Error`。

## 公开面

- `Command { path, args, dir, env, stdin, timeout, max_output_bytes, cmd_line, sandbox }`
  与链式构造器。`max_output_bytes` 默认 `DEFAULT_MAX_OUTPUT_BYTES`（1 MiB，按
  stdout/stderr 各算一份）。
- `Result { exit_code, stdout, stderr, duration, truncated }`。`truncated` 在任
  一流超过上限时为真；超出部分被丢弃，但管道仍被持续读空，避免子进程因短写而死锁。
- `Runner` trait 与 `Pipe`：`Pipe::run(command)`；`Pipe::run_with_cancel(command, &AtomicBool)`
  接受外部取消令牌。取消在退出码之前判定。
- `Sandbox` trait：`name()`、`apply(&mut std::process::Command)`，以及可选的
  `abort_apply()`（`apply` 成功后 `spawn` 失败时恰好调用一次）和
  `after_start(pid)`（进程已启动、尚未等待时调用）。exec 不知道任何具体沙箱实现。
- `ENV_INHERIT`：显式环境里出现这个哨兵值时，以父进程环境为底，再叠加显式项。
- `cmd_line`：Windows 原样命令行。非 Windows 上带非空 `cmd_line` 返回
  `ErrorKind::Refused`，绝不静默忽略；`cmd_line` 与 `args` 同时非空返回
  `ErrorKind::Invalid`。
- `PTY`（Go 参考里的 `pty.go`）不在 M0 实现：当前 runner 只提供管道，
  不假装是终端；需要交互式终端时另开 trait/实现，不能偷偷用管道模拟。

## 依赖与边界

只依赖 `hearth-core`（错误类型）和标准库。不得依赖 `hearth-sandbox`：沙箱通过
`Sandbox` trait 注入，exec 不需要知道 job object、Landlock 或 Seatbelt 的存在。
不得知道会话、LLM、工具策略或 HTTP。进程树终止由本 crate 自己负责（Windows
Job Object，Unix 进程组），沙箱的 job object 是额外一层，不是替代品。

## 不要做

- 不要把非零退出码当错误；调用方需要读到它。
- 不要在输出超过上限时停止读管道或短写：子进程会阻塞在写上，整个 run 卡死。
- 不要在 `apply()` 失败后调用 `abort_apply()`；只有 `apply()` 成功且 `spawn()`
  失败才调用一次。
- 不要在 `Drop`/`close` 时杀掉已经成功结束的运行留下的后台进程；只终止本次
  run 自己的进程树。
- 不要在库代码里对 I/O 使用 `unwrap()`/`expect()`，也不要引入 async 运行时或新依赖。
