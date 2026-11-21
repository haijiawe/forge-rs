<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# M6 冻结面 · 安全与平台

本文件是 M6 并行单元的**接口冻结单**。规则同 `m4-surface.md`：

- 这里写的签名，实现必须逐字相同；做不到时**先报告**再由集成者改本文件，不许各写一份。
- 归属表里不属于你的文件，只读不写。
- 「为什么」写在代码注释里，这里只写契约和理由摘要。

M6 的一句话目标：**让"沙箱模式"从一条策略规则变成操作系统真的挡得住的东西，并且在挡不住的时候明确说不，而不是装作挡住了。**

---

## 0. 现状（M6 开始时的真实状态）

`internal/tool/policy.go` 已经有 `read-only` / `workspace-write` / `full-access` 三档，但它是
**策略**：`DefaultRules(mode)` 决定某个工具调用是 allow / ask / deny。它挡的是"模型想做但没被允许的事"，
挡不住"被允许的进程真的去做了什么"。

具体缺口，逐条对应下面的单元：

| 缺口 | 今天的行为 | M6 要变成 |
| --- | --- | --- |
| `shell` 在 `workspace-write` 下被允许后，命令可以写工作区外 | 只有 `fs_*` 工具做路径遏制，`shell` 不做 | 进程级文件系统限制（能做的平台），否则拒绝执行 |
| 子进程树泄漏 | `internal/exec` 用 job object 杀树（已有） | 保持，并把限制加强为内存/进程数上限 |
| 危险命令 | 一律 `ask`，人不在就 `deny` | 一份**恒拒绝**名单，不产生审批弹窗（问了也不该允许） |
| 网络 | 无任何限制 | 能限制的平台限制；不能的限制就报告不能 |
| 沙箱不可用 | 静默地按策略正常运行 | **fail-closed**：要求强制的模式下拒绝执行并说明原因 |
| `hearth doctor` | 检查 config/data/workspace/git/proxy/pty | 加沙箱可用性、磁盘、模型端点连通性、MCP 连通性 |

---

## 1. U1 · `internal/sandbox`（新包，L1，零 `hearth/internal` 依赖）

放 L1 的理由和 `internal/exec`、`internal/mcp` 一样：它只认识操作系统，不认识会话、工具、模型。

```go
package sandbox

// Mode 是配置里的三档，字面量与 internal/config 的同名常量一致。
type Mode string

const (
	ReadOnly       Mode = "read-only"
	WorkspaceWrite Mode = "workspace-write"
	FullAccess     Mode = "full-access"
)

// Spec 是一次执行需要的保证。Workspace 必须是绝对且已解析（EvalSymlinks）的路径：
// 沙箱层不做路径猜测，猜错的方向永远是"更宽松"。
type Spec struct {
	Mode       Mode
	Workspace  string
	AllowNet   bool     // 只有 full-access 或用户显式开启时为 true
	ExtraWrite []string // 额外可写目录，绝对路径（临时目录、缓存目录）
}

// Support 描述"这台机器能保证什么"。零值表示什么都不保证。
type Support struct {
	Filesystem  bool   // 能限制写路径
	Network     bool   // 能限制网络
	ProcessTree bool   // 能保证子进程随父进程结束
	Enforced    bool   // 是否真的装了限制（false = 调用方必须自己决定是否接受）
	Detail      string // 人话，进 doctor 与错误信息
}

// Enforcer 装一次限制，用于多次执行。实现必须幂等、并发安全。
type Enforcer interface {
	Name() string
	Support() Support
	// Apply 在 cmd 上装好限制。返回错误 = 调用方必须放弃这次执行（fail closed）。
	Apply(cmd *exec.Cmd) error
	// Close 释放句柄（job object、landlock fd 等）。可重复调用。
	Close() error
}

// AfterStarter 是可选的第二段：有些限制必须等进程存在才能装（Windows job
// object 要一个活 pid，而 Start 之前没有 pid）。实现了它的 enforcer，
// internal/exec 会在 cmd.Start 成功后、Wait 之前回调一次。没有实现它 = 只有
// Apply 生效，这不是降级。（M7 审计补：本接口自 M6 落地起就在代码里——
// internal/sandbox/sandbox.go:167——但冻结面漏写了它，而 Windows 的进程树约束
// 全靠它；漏写的代价是平台实现者会以为 Apply 就够。）
type AfterStarter interface {
	AfterStart(pid int) error
}

// New 按 Spec 选一个实现。返回 (nil, err) 时调用方不得执行命令。
// FullAccess 返回一个什么都不做的实现（Name() == "none"），因为"不装限制"本身
// 是一个要说出来的决定，不是一条默认路径。
func New(spec Spec, logger *slog.Logger) (Enforcer, error)

// Available 报告本机在不装任何限制时能提供什么，供 doctor 使用。
func Available() Support
```

### 平台实现与诚实标注

| 平台 | 文件 | 手段 | 状态 |
| --- | --- | --- | --- |
| Windows | `windows.go` | 两层环：Job Object（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`、`JOB_OBJECT_LIMIT_PROCESS_MEMORY`、`JOB_OBJECT_LIMIT_ACTIVE_PROCESS`、`JOB_OBJECT_LIMIT_JOB_MEMORY`；`AssignProcessToJobObject` 在 `cmd.Start` 之后由 `exec` 回调）+ AppContainer（`CreateAppContainerProfile` 得到 package SID，给它一条继承式 ACE 授予 workspace 写权限，用 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 在容器里起进程；`allow_net` 才追加 `internetClient` 能力）；因为 `CreateProcessW` 的 attribute list 无法由 `std::process::Command` 表达，`Apply` 把命令改写成 `hearth <program> <args…>`，wrapper 在 `main` 第一句回答 `maybe_exec_helper()` | **本机实测**（写工作区成功、写同级目录被 OS 拒绝、无能力时 loopback 连接被拒） |
| Linux | `linux.go` | `landlock_create_ruleset` + `landlock_add_rule` + `landlock_restrict_self`（原始 syscall，`x/sys/unix` 若已提供则用包），ABI ≥ 1 限制文件系统，ABI ≥ 4 限制网络 | **运行时验证**（Ubuntu CI runner 每次 push 跑 `crates/sandbox/tests/linux.rs` 五个用例：workspace 内写成功、外被拒、read-only 拒写、`extra_write` 按名授权、无 `allow_net` 时 connect 被内核拒绝且 `allow_net` 时连通） |
| macOS | `darwin.go` | 生成 `sandbox-exec` profile：`(deny default)` + 放行工作区读写 + `(deny network*)` | **运行时验证**（`sandbox macos-latest` job 每次 push 跑 `crates/sandbox/tests/macos.rs` 五个用例：模式矩阵、workspace 内写成功/外被拒、read-only 拒写、`extra_write` 按名授权、`allow_net` 两向） |
| 其它/不支持 | `unsupported.go` | `New` 在要求限制时返回错误 | — |

**Windows 的成本（如实写在 `Support.Detail` 与 README 里）**：AppContainer 的 package profile 按 workspace 路径命名，一个路径一个 profile，创建后长期留在 `%LOCALAPPDATA%\Packages`；容器里的进程读不到 `%USERPROFILE%` 与启动目录，所以依赖 home 目录的工具（git、npm、pip）可能失败。`CreateAppContainerProfile` 会因 profile 服务繁忙返回 `ERROR_ACCESS_DENIED`/`ERROR_PATH_NOT_FOUND`，实现里对这两个瞬时代码做 8 次退避重试（约 8 秒）——一次瞬时失败曾经让沙箱在能用的机器上拒绝执行命令。本机实测到一个更坏的形态：攒到约九十个 `hearth.sandbox.*` profile 之后，profile 服务会成段时间**拒绝创建任何新名字**（新建 workspace 全线失败，而已有 profile 照常可用，几分钟后自行恢复）。因此实现分三级：**已存在就按目录 lookup 直接 derive（从不重建）→ 新建名失败则退避重试 → 仍失败则退到全用户共享的 `hearth.sandbox` 身份**。积累的身份可以用 `hearth doctor --prune-profiles` 删除（本机实测 106 个删到 0，第二次运行报 `0 found`）；它的默认清单式读授权见 §8。删除撞的是**同一个**限流：本机实测同一个命令半小时后变成 `57 found, 0 removed, 57 failed`（全 `0x80070005`），而同一时刻新建身份也全线失败——即服务整体拒绝，不是某个身份顽固。因此 prune 只对**第一个**名字付出完整重试预算（约 4 秒）作探针：全预算后仍被瞬时错误拒绝且一个都没删成，就停止并明说"服务正在拒绝，其余未尝试，过几分钟重试才是解法"（实测 3.9 秒结束，而不是逐个数完 57 个、白等 182 秒）。共享身份仍然把命令关在授予它的目录里，但不再保证两个 workspace 彼此隔离；这层降级必须被说出来，不能静默改用。

**强制完整性级别是另一条不可忽视的前置条件**：进程的完整性级别取“父进程”和“自身镜像标签”中较低者，所以放在带 `Low` 标签目录里的二进制（浏览器解压的下载常见）以 Low 运行，而 Low 进程写不了 `%LOCALAPPDATA%`——profile 服务每次都以 `ERROR_ACCESS_DENIED` 拒绝，`GetNamedSecurityInfoW` 读 DACL 也会被拒。本机实测：整个项目树带 `Low` 标签，于是 `D:\...\bin\hearth.exe` 全部失败、`%TEMP%` 下逐字节相同的二进制全部成功，一度被误诊为“非 ASCII 路径”问题。因此 `available()` 在 Low 进程里直接返回 `false`（严格模式宁可拒绝，也不声称做不到的遏制），`Support.Detail` 与拒绝文案都会点名这一条并给出 `icacls <dir> /setintegritylevel (OI)(CI)M /T /C`；边界判断用 `< Medium (0x2000)` 而不是 `< Low (0x1000)`，因为 Low 本身**就是** `0x1000`。

**macOS 与 Linux 的诚实要求**：`Support.Enforced` 只有在真的装成功后才为 `true`。
两个平台现在都由 CI 在真实内核上跑过：Linux 是 ubuntu job 的 `crates/sandbox/tests/linux.rs`，
macOS 是 `sandbox macos-latest` job 的 `crates/sandbox/tests/macos.rs`。仍未被证明的是
**其他 macOS 版本与 Intel 主机**——CI 的 `macos-latest` 是 Apple Silicon。`Support.Detail`
不再带 "unverified on this host"，但也不能说"在所有 macOS 上都验证过"。
已退役的 Go 树里的 `docs/deps.md` 与 `docs/05-roadmap.md` 由集成者同步这条限制。

### fail-closed 规则（写进 `New` 的注释并测试）

1. `Mode == FullAccess` → 返回 no-op enforcer，`Support.Enforced == false`，不报错。
2. `Mode == ReadOnly` 且本机无法限制**文件系统** → 返回错误，错误里带 `Support.Detail`。read-only 的全部承诺就是"不写"，进程树/内存上限挡不住 `echo x > C:\Windows\foo`，所以这里不能降级成更小的承诺却用同一个名字。
3. `Mode == WorkspaceWrite` 且本机能装**至少一个真实维度**（`Filesystem || ProcessTree`）→ **允许**，但必须如实报告：`Support.Filesystem == false`，`Support.Detail` 点明缺的是文件系统维度，`New` 打一条 Warn，`hearth doctor` 给 Warn 而不是 Fail。本机一个维度都装不了 → 按第 2 条返回错误。
4. `Platform` 支持但装失败（job object 创建失败、landlock 返回 EPERM）→ 返回错误，**不许降级**（`Support.Enforced` 仍为 false 的实现一律视为装失败）。
5. 用户显式要求"我知道这里挡不住"的唯一途径是改用 `full-access` 并承担它的后果；没有第二个开关。

**第 3 条是 2026-11-16 的修订**（原规则要求 workspace-write 同样必须有文件系统遏制）。修订理由：本项目的开发与首发平台是 Windows，job object 能限制进程树、内存与生命周期但限制不了路径；按原规则 Windows 上默认配置直接不可用，而一个被告知"沙箱不可用"的操作者会改用 `full-access`——那是**严格更少**的限制却挂着更好听的名字。所以规则从"所有维度或报错"改成"永不宣称一个没有实施的维度"：可以只限制部分维度，但缺哪个必须写在 `Support` 与警告里，让人在批准命令前看得见。read-only 不适用这条，因为它的承诺本身就是被保护的那个维度。

---

## 2. U2 · `internal/exec` 挂载点（L1）

在 `Cmd` 上增加一个字段，接口定义在 **`exec` 里**（消费方定义接口，R4），不 import `sandbox`：

```go
// Sandbox 在进程启动阶段装限制。实现来自 internal/sandbox，但接口归 exec 定义：
// 谁执行进程谁定义它需要什么。
type Sandbox interface {
	Name() string
	Apply(cmd *exec.Cmd) error
}

// Cmd 增加：
	Sandbox Sandbox
```

语义（每条一个测试）：

- `Sandbox == nil` = 不装限制（今天的行为，保持向后兼容）。
- `Apply` 返回错误 → `Run` 返回 `ErrSandbox`（新哨兵，`errors.Is` 可判），**不启动进程**。
- **`Start` 失败时释放 Apply 准备好的东西**（`SandboxAborter` 可选接口，`AbortApply()`，2026-11-16 补）：Windows 的 `Apply` 每条命令建一个 job object 并排进 FIFO，`AfterStart(pid)` 弹队首；`Start` 失败则没有 pid 可收养，那条 job 既不被使用也不被释放，于是下一次成功的运行会收养**上一条**命令的 job，而每次失败的 start 泄漏一个句柄。限制值相同所以执法等价，配对却是错的。因此 `exec` 在 `Start` 失败时调用 `AbortApply()`（仅此时调用；成功后绝不调用——那会释放正夹着活进程的 job）。
- 非 Windows 平台且 `CmdLine != ""` 的既有规则不变。
- 现有的 job object 进程树回收**保留**：它是 `exec` 自己的保证（超时必杀树），
  与 sandbox 的"限制"是两件事，两者叠加而不是互相替代。这条要写进注释，否则下一个人会删掉其中一个。

---

## 3. U3 · 危险命令名单（`internal/tool`，L2）

新文件 `internal/tool/builtin/danger.go`（M6 落地时放在 builtin 包内——`Dangerous` 只被内置 `shell` 工具调用，与它同包；早期草稿写作 `internal/tool/danger.go`，M7 审计更正）：

```go
// Dangerous 判断一条 shell 命令是否属于"问了也不该允许"的那一类。
// 返回非空 reason 表示必须拒绝；reason 会进 Result.Output 与 Meta["blocked"]。
func Dangerous(command string) (reason string)
```

名单（大小写不敏感、词边界匹配、先归一化多余空白；命中即 deny）：

| 类别 | 例子 | reason |
| --- | --- | --- |
| 递归删根/家目录 | `rm -rf /`、`rm -rf ~`、`rm -rf C:\`、`del /s /q C:\` | `recursively deletes a root or home directory` |
| 磁盘覆写 | `dd of=/dev/`、`mkfs`、`format C:`、`diskpart`、`wipefs` | `overwrites a disk or filesystem` |
| 关机/重启 | `shutdown`、`reboot`、`halt`、`poweroff` | `powers the machine off` |
| 权限交给全世界 | `chmod -R 777 /`、`icacls ... /grant Everyone:F` | `grants everyone full control` |
| 从网络直灌解释器 | `curl ... \| sh`、`iwr ... \| iex`、`wget -O- ... \| bash` | `runs a remote script without inspection` |
| 改注册表关键项 | `reg delete HKLM`、`reg add HKLM\...\Run` | `edits a system-wide registry key` |
| 杀安全软件/日志 | `taskkill /f /im` 针对系统进程、`wevtutil cl` | `disables system protection or logging` |
| 计划任务持久化 | `schtasks /create`、`crontab -`（从 stdin 覆盖） | `installs persistence` |

规则：

1. **名单是 deny，不是 ask**。`Dangerous` 命中时 `shell` 工具**不产生审批**、不执行，返回
   `Result{IsError:true, Code:"blocked", Output:"blocked: <reason>\n\n<command>"}`。
2. 匹配必须是**保守**的：宁可漏（漏掉的还有审批），不可误杀（误杀会让人学会关掉这个功能）。
   所以只在能说清楚的形状上命中，`rm -rf ./build` 必须放行。
3. 名单不是策略的替代品：`full-access` 下**仍然拒绝**——它保护的是"人按了回车"这件事本身。
   这条要写测试，因为它是唯一一条策略不能覆盖的规则。
4. `Meta["blocked"] = reason`，供 UI 显示成一个红条而不是普通输出。

---

## 4. U4 · `internal/doctor` 扩展（L2）

`doctor` 的 `Check` 结构已有 `{Name, Status, Detail, Hint}`，只加检查项，不改结构。

新增四项，顺序固定在 `git` 之后、`proxy` 之前：

| Name | 判据 | Fail/Warn |
| --- | --- | --- |
| `sandbox` | 构造真实 enforcer（`sandbox.New`，`o.newSandbox` 为测试缝）并关闭；判据来自 `Enforcer.Support()`，doctor 不自己推导平台矩阵 | 构造失败（`UnavailableError`）→ **Fail**，`Detail` 用 `Support.Detail` 原文；装上了但 `Support.Filesystem == false` → **Warn**；`full-access`（故意不装限制）或 `ErrInvalidSpec` → Skip |
| `disk` | 数据目录所在卷剩余空间（`GetDiskFreeSpaceExW` / `Statfs`，向上找最近的已存在祖先） | < 512 MiB → Fail，< 2 GiB → Warn |
| `model` | 对配置的 base URL 发 `HEAD <base>/models`，硬 2s 超时，不重试 | 连不上 → Warn（离线可用是特性）；401/403 → Warn 并提示 `HEARTH_API_KEY`；其它状态 → Warn；**永不 Fail**；无 model → Skip |
| `mcp` | **配置层**：不启动任何 server（只读诊断不该 spawn 进程），只核对条目可用性 | 无 server → Skip；有丢弃/重复 → Warn 并把 `MCPWarnings()` 放 `Hint`；全部可用 → OK 列出名字 |

`-json` 输出的键不变（`checks[]` 里多四项），断言键表与检查项名字的测试要同步。

**2026-11-16 修订**（实施后回填，两处原措辞已过时）：
- `sandbox` 原写作"要求限制但不可用 → Fail；可限制但未启用 → Warn"。实施后按 §1 第 3 条修订执行：**部分执法是 Warn 而不是 Fail**，只有"一个维度都装不了"才 Fail。`full-access` 不是"未启用"而是"故意不装"，所以是 Skip 而不是 Warn——把用户的选择报成警告会训练人忽略警告。
- `mcp` 原写作"对每个配置的 server 握手"。实施改为配置层核对：doctor 是无副作用诊断，spawn 一个 server 可能拉网络、写缓存、留下子进程，而它要回答的问题只是"你的配置里有没有坏条目"。真实握手留给运行时（`cmd/forge` 的 dial 循环）与 `hearth mcp`（未做）。

---

## 5. U5 · 接线（集成者，`cmd/forge` + `internal/config`）

- `hearth chat` / `hearth web` 加 `--sandbox=read-only|workspace-write|full-access`（默认取 `config.Sandbox`；非法拼写 exit 2，任何 I/O 之前）。
- 启动时 `sandbox.New(...)`：失败则**在打印 banner 之前**退出 1，错误里带 `Support.Detail`；部分执法时向 stderr 打**恰好一行** `hearth: warning: <mode> is only partially enforced on this host: <detail>`。
- `ShellTool.Sandbox`（`internal/tool/builtin`）拿到 enforcer：类型是 `exec.Sandbox` 窄接口，不是 `sandbox.Enforcer`（工具不该拥有 `Close`/`Support`，enforcer 由启动者创建与关闭）。
- `hearth doctor` 打印 `sandbox: <name> <detail>`。
- **未做**：`config.SandboxAllowNet`（`sandbox.Spec.AllowNet` 已在契约里，但没有任何平台实现能真正限制网络，加一个配置项等于给一个不执行的开关）；`hearth web` 的 per-turn enforcer 重建（enforcer 是"这台服务器所有命令"的属性，每轮重建会让两轮对"遏制是什么"各说各话，所以每进程一次）。

---

## 6. 归属矩阵

| 路径 | 归属 |
| --- | --- |
| `internal/sandbox/**` | agent U1 |
| `internal/exec/exec.go`（`Sandbox` 接口 + `Cmd.Sandbox` + `ErrSandbox`） | agent U2 |
| `internal/tool/builtin/danger.go`、`internal/tool/builtin/danger_test.go`、`internal/tool/builtin/shell.go`（拒绝分支） | agent U3 |
| `internal/doctor/doctor.go` + 测试 | agent U4 |
| `cmd/forge/**`、`internal/config/config.go`、`architecture.yaml`、所有 README frontmatter、已退役的 Go 树里的 `docs/05-roadmap.md`、`docs/deps.md`、本树的 `docs/README.md` | **集成者** |

## 7. 验收（集成者跑，命令与真实输出进 roadmap）

```
go build ./... && go vet ./... && go test ./... -count=1
go run ./scripts/archcheck
go run ./scripts/gen-protocol -check
bin/hearth.exe doctor                       # 六项 + 四项新检查
bin/hearth.exe doctor --json | ...          # 键表不变
bin/hearth.exe chat --sandbox=read-only "写一个文件到 /tmp"   # 必须被拒且说明原因
```

另有两条**必须人工确认**的：

- `workspace-write` 下一段试图写工作区外的 shell 命令被操作系统拒绝（本机 Windows 可实测；
  Linux/macOS 的同类证据由 CI 的两个沙箱 job 提供，见上表）。
- 断网环境下 `hearth doctor` 的 `model` 检查是 Warn 不是 Fail，且 `hearth chat` 仍能跑（本地模型）。

## 8. 不做（明说，不是遗漏）

- 不做"受限令牌 + ACL 重整"（Windows 上另一种降权），因为它需要管理员权限且会让 git/npm
  这类工具莫名失败；改用 AppContainer（见上表）——它不需要管理员，代价是容器里的进程
  读不到启动目录，凡是要 home 目录的工具会失败。这条不再只是写在 `Support.Detail` 里
  让操作者自己撞：`Spec.extra_read` / `extra_write` 让宿主显式授予少数工具自有路径
  （`~/.gitconfig`、`~/.cargo`、`~/.npmrc`、`%APPDATA%\npm`、`%APPDATA%\pip`、`~/.config`
  只读，`%TEMP%` 读写），CLI 在 `crates/cli/src/grants.rs` 里算出默认清单，`config.json` 的
  `sandboxAccess.read` / `.write` 可整份替换（显式空数组＝什么都不额外授予，这是唯一能要到
  最严格行为的方式）。本机实测：容器内能读到 `~/.gitconfig`，读同一目录下未授予的文件被拒
  （`windows_reads_are_granted_only_where_asked`）；用真实 `git config --global --get user.name`
  端到端验证过，输出即配置里的名字。
  网络是第三个开关，同样是 `sandboxAccess` 的成员：`sandboxAccess.network: true` 才给
  `internetClient` 能力，缺省（以及显式 `false`）是没有 socket 的容器。本机实测
  （`windows_network_follows_the_spec_switch`，连跑 8 次稳定）：关掉时对 `8.8.8.8:53`、
  `1.1.1.1:53`、`114.114.114.114:53`、`192.168.1.1:80` 全部 `refused: permission denied`，
  打开时全部 `connected`。loopback 不做正向用例：AppContainer 要连回环还得
  `NetworkIsolationSetAppContainerConfig` / `CheckNetIsolation LoopbackExempt`，那需要管理员、
  进程全局、且影响所有 AppContainer，crate 刻意不创建它；因此 `127.0.0.1:9` 两个方向都是
  `connection refused`，只用于拒绝方向。
  授权的完整条件是"目标 + 祖先可穿过"：打开 `~/.gitconfig` 要逐组件 stat，缺 `FILE_TRAVERSE`
  的 `git` 直接死在 `failed to stat '...\AppData\Local': Permission denied`，执行前就退出。
  因此授权会连带祖先目录，跳过盘根（重写 `E:\` 的 DACL 实测 160 秒，且盘根本来就放行）。
  写 DACL 会让 Windows 重算整棵子树的继承：本机实测 `%TEMP%` 8–9 秒、`AppData\Local` 10–12 秒、
  `AppData` 18–28 秒、用户主目录 23–28 秒。重复授权先用 `GetEffectiveRightsFromAclW` 问 ACL，
  已有就跳过；超过两秒会打印"正在为某目录首次授权"而不是装作没卡。
  这份成本原本按**工作区**付，因为祖先的 traverse ACE 授给每工作区自己的 package SID（profile 名是
  workspace 路径的哈希）。所以祖先的 traverse ACE 改授给一个本项目所有容器共享的 capability SID
  （`S-1-15-3-2048-40`，写进 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 的能力数组里），
  而工作区与 `sandboxAccess.read/write` 的每个路径**本身**仍只授给那个工作区的 package SID：
  数据隔离不变，只有"穿过祖先目录"从每工作区一次变成每机器一次
  （`a_fresh_workspace_does_not_pay_the_ancestors_again`：第一块新工作区 61 秒、第二块 5 秒内）。
  这个 capability 不单独给予访问权：只有 traverse ACE 提到它，而 traverse 不让人列目录或读文件。
  冷启动那一分钟仍发生在子进程里，所以算在那条命令的耗时里（`shell` 工具默认 120 秒超时）。
  也是因为这份成本，workspace 自身只授权目录本身、不连带祖先：容器对 workspace 的祖先
  本来就可穿过（测试里子进程从 `%TEMP%` 下面读到自己的副本即是证据）。
- 不做网络代理/防火墙规则（需要管理员）；`AllowNet=false` 在 Windows（无
  `internetClient` 能力，loopback 也一样不通）、macOS 与 landlock ABI≥4 上生效。
- 不做容器/VM（那是另一个产品）。