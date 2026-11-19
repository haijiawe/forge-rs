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
  路径、存在且是目录；文件系统根被拒绝（那等于 full-access）。`allow_net` 缺省为
  `false`，唯一来源是配置里的 `sandboxAccess.network`。它是**真的开关**，不是占位：
  Windows 上它决定 AppContainer 有没有 `internetClient`（`S-1-15-3-1`）能力，实测
  `false` 时对 `8.8.8.8:53` 得到 `WSAEACCES`（`permission denied`）、`true` 时得到
  `connected`；loopback 例外——Windows 把 loopback 也算网络，要在容器里连上它还需要
  `NetworkIsolationSetAppContainerConfig`（`Firewallapi.dll`，需管理员、进程级、影响所有
  AppContainer）的豁免，本 crate 刻意不建这种豁免，所以 loopback 只能用作“拒绝”那一半。
  macOS 对应的是一行 `(allow network*)`，Linux 是 Landlock ABI ≥ 4（更低版本时
  `Support.network` 为 false 并拒绝非 full-access）。macOS 的这句是
  `unverified on this host`；Linux 不是——Ubuntu CI 每次 push 跑
  `crates/sandbox/tests/linux.rs` 的五个用例（workspace 内外写、read-only、
  `extra_write`、网络两向），所以它的 `Support.Detail` 不再带这句话。`wrapper` 是“`main`
  第一句调用 `maybe_exec_helper` 的可执行文件”，默认 `None` 表示本进程（`forge`
  自己）；把本 crate 当库嵌进别的宿主才需要设置它。`extra_read` 是“workspace 之外
  仍可读”的路径，只对默认拒绝读的平台有意义（Windows 的 AppContainer 从零可读开始；
  Linux 的 Landlock 策略只管写、macOS 的 profile 已经 `allow file-read*`，所以那两个
  平台上是空操作）。不存在的读路径会被跳过而不是拒绝：这份清单描述的是“这台机器
  通常有什么”，机器上没有 `~/.npmrc` 不该变成沙箱拒绝启动。
  `extra_read`/`extra_write` 里的路径还要**祖先可穿过**：Windows 上打开
  `~/.gitconfig` 会先逐个组件 stat，缺 `FILE_TRAVERSE` 的命令死在
  `failed to stat '...\AppData\Local': Permission denied`，一个字节都没读到，所以
  授权会连带祖先（跳过盘根：重写 `E:\` 的 DACL 实测 160 秒）。写 DACL 会让
  Windows 重算整棵子树的继承，实测 `%TEMP%` 9 秒、`AppData\Local` 11 秒、
  `AppData` 20 秒、用户主目录 25 秒；每个目录每个身份只付一次（重复授权先问
  ACL，已有就跳过），超过两秒会打印正在做什么。
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
  用户下创建的 `forge.sandbox.*` 包身份，返回 `{found, removed, failed, stopped_early}`。
  profile 服务没有枚举 API，所以索引就是 `%LOCALAPPDATA%\Packages` 里的目录名。这是
  维护而不是打扫：攒到约九十个身份后，服务会成段时间拒绝新名字——**删除撞的是同一个
  限流**（实测同一命令半小时后 57 个全 `0x80070005`，而当时新建也全线失败）。所以它
  只给第一个名字完整重试预算作探针，失败且一个都没删成就置 `stopped_early` 并停手：
  "服务正在拒绝，过几分钟重试"比逐个数完更接近事实。

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
- 不要在 macOS 的能力文案里省略 `unverified on this host`：这个平台只在本机编译
  检查过，没有实测。Linux 不是——`crates/sandbox/tests/linux.rs` 的五个用例在 CI
  的 ubuntu job 里跑真实内核；把 Linux 也说成 "unverified" 同样是撒谎，只是方向
  相反。
- 不要把 `ERROR_ACCESS_DENIED` 一律当权限问题去改 ACL。进程的强制完整性级别取
  “父进程级别”和“自身镜像标签”中较低的那个：镜像放在打了 `Low` 标签的目录里
  （浏览器解压的下载常见），进程就以 Low 运行，而 Low 进程**写不了 `%LOCALAPPDATA%`**，
  profile 服务因此每次拒绝，连读 Medium 对象的 DACL 都会拒。本机栽过：项目树带
  `Low` 标签，于是 `bin\forge.exe` 全部失败而 `%TEMP%` 下的同一份二进制正常，还
  被误诊成“中文路径”。`integrity_rid()` 现在把这种情况写进拒绝文案并给出
  `icacls <dir> /setintegritylevel (OI)(CI)M /T /C`；`available()` 在这种进程里
  直接返回 false，不让严格模式声称它做不到的遏制。
