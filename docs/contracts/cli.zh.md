> 本文件是 [cli.md](cli.md) 的中文翻译。**英文原文是规范**：两者冲突时以英文为准，并回来修本文件。
> 命令名、flag、路径、退出码一律不译，它们必须与二进制逐字一致。

<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# hearth CLI 契约

> **中文导读。** 这份契约管一件事：`hearth` 这个二进制的命令行面——哪些命令存在、每个命令输出什么、
> 退出码是什么。它**逐字**约束输出，因为脚本和 agent 会按字节比对。
>
> 怎么读：§1 是调用模型（`hearth [命令] [参数]`、`--flag` 可以后置、`--` 结束选项）；
> §2 是**今天存在的**命令，一条一节，给用法、输出、退出码；§3 是保留但**没实现**的名字
> （写了也不许出现在 `help` 里）；§4 给实现者的兼容说明；§5–§10 是几个子命令族的细节。
>
> 几个术语：**usage 文本**指 `hearth help` 打出的那段，它被测试逐字节比对；
> **退出码** 0 成功、1 失败、2 用法错误——三者都是契约的一部分；
> **`--json`** 出现时输出稳定的键集而不是给人看的排版，脚本应该只用这种形态。
>
> 改动时必须同步两处：① `crates/cli/src/lib.rs` 的 `USAGE` 常量——
> `crates/cli/tests/cli.rs` 的 `usage_is_byte_equal_to_the_frozen_contract` 会逐字节比对它；
> ② 本文对应的那一节。只改代码不改本文，或者只改本文不改代码，都会红。

状态：**对今天存在的命令具有规范性**，由 `cmd/forge` 在版本 `0.0.0-dev` 下实现。
凡不在 [§2](#2-commands-that-exist-today) 里的内容，都不属于 CLI 的一部分。

下文的一切都是通过运行构建出的二进制验证的；那些确切的字节序列就是从那一次运行里复现的。
退出码是契约的一部分：一个打出了正确文本、却返回错误退出码的重新实现，是不合规的。

---

## 1. 调用模型

```text
hearth <command> [flags]
```

- 每次调用恰好一个命令。只有一个命令带子命令：`hearth session fork`（§2.6）。其余都是叶子。
- 全局 flag 放在命令之后（`hearth doctor --json`），绝不放在它之前。命令自己的 flag 与它的位置
  参数可以按任意先后出现，因为 `cmd/forge/flags.go` 在 `flag` 解析它们之前就把 flag 挪到位置
  参数前面（§2.3“提示词”）。`--` 仍然终止 flag 解析。
- 输出去 **stdout**，除非下面某条规则说是 stderr。`hearth chat` 把两者分开：答案是 stdout，
  诊断是 stderr（§2.3）。
- `hearth` 从不主动索要输入。只有两种调用会读 stdin：不带 prompt 参数的 `hearth chat`，它读到
  EOF（§2.3）；以及 `hearth chat --approve=prompt`，它每次工具审批读一行（§2.3“工具与审批”）。
  其他所有命令都忽略 stdin，绝不阻塞等待输入。
- 版本字符串：`0.0.0-dev`（构建时通过 `-ldflags` 注入）。commit：`unknown`。两者都会被打印；
  两者在构建之间都不稳定。

### 退出码

| 码 | 名称 | 何时 |
| --- | --- | --- |
| `0` | 成功 | 命令完成。包括 `--version`、`help`、没有任何检查状态为 `fail` 的 `doctor`，以及以 `completed` 结束的一轮 `chat`。 |
| `1` | 失败 | `doctor` 跑完了，但至少一项检查报告 `fail`；配置路径无法确定时也用它。对 `chat` 而言：这一轮跑不起来或以 `error` 结束（§2.3），再加上 `--resume`/`--fork` 可能被拒绝的三种情形——会话 id 不存在、日志里有一轮在等待审批、或者另一个进程正持有这个会话（§2.3“恢复与 fork”）。它不是崩溃。 |
| `2` | 用法错误 | 未知命令、未知 flag、flag 解析失败、非法的 `--session`、`--resume` 或 `--fork`、`--resume` 与 `--fork` 同时给出、`--resume` 与 `--session` 同时给出、空的 prompt、未知的 `--approve` 值、`--no-tools` 与非默认的 `--approve` 同时给出、`--approve=prompt` 配一个不是终端的 stdin、未知的 `--sandbox` 值、一个不在记录边界上或落在打开轮次内部的 fork 切点，以及 `hearth web --approve=prompt`（web 的审批者是 `web`，不是终端）。总会伴随一条打到 stderr 的消息。 |
| `130` | 已取消 | `hearth chat` 被 Ctrl+C 中断。在进程退出之前，日志里这一轮以原因 `cancelled` 关闭（§2.3）。 |

本表之外的任何退出码（包括运行时因 Go panic 产生的 `2`）都是 bug，不是契约。`hearth` 不会
用退出码 `1` 表示“未知命令”。

---

## 2. 今天存在的命令

| 命令 | 别名 | 输出 | 退出 |
| --- | --- | --- | --- |
| `hearth --version` | `-v`, `version` | stdout 上一行版本信息 | `0` |
| `hearth chat [flags] [prompt]` | `hearth "<prompt>"` | 答案流式输出到 stdout，诊断输出到 stderr | `0`、`1`、`2` 或 `130` |
| `hearth web [flags]` | — | stderr 上三行——`serving <baseURL>`、`session=… model=… workspace=… sandbox=… approve=…`、`open <url with token>`——以及它打开的浏览器里的界面 | `0`、`1`、`2` 或 `130` |
| `hearth session fork <parentId> [flags]` | — | stdout 上新会话 id（或一个 JSON 对象），诊断输出到 stderr | `0`、`1` 或 `2` |
| `hearth session archive <sessionId> [flags]` | — | stdout 上一句话（或一个 JSON 对象），诊断输出到 stderr | `0`、`1` 或 `2` |
| `hearth import <old-dir> [flags]` | — | stdout 上的计划或结果，诊断输出到 stderr | `0`、`1` 或 `2` |
| `hearth index [flags]` | — | stdout 上的统计、命中或 JSON | `0`、`1` 或 `2` |
| `hearth goal <command>` | — | stdout 上的目标 id、状态块或 JSON | `0`、`1` 或 `2` |
| `hearth schedule <command>` | — | stdout 上的任务 id、表格、发生时刻或 JSON | `0`、`1` 或 `2` |
| `hearth plugin <command>` | — | stdout 上生成的骨架、清单列举、握手、工具输出或 doctor 报告 | `0`、`1` 或 `2` |
| `hearth expert <command>` | — | stdout 上的名册表格、一条提示词或 JSON | `0`、`1` 或 `2` |
| `hearth skill <command>` | — | stdout 上的技能表格、一份技能正文或 JSON | `0`、`1` 或 `2` |
| `hearth subagent <command>` | — | 一个会话的子会话、某个会话之下的树、名字解析、耗时、恢复、中断、后端，或者 worker | `0`、`1` 或 `2` |
| `hearth team <command>` | — | stdout 上的团队列举、worker 报告或工作板描述 | `0`、`1` 或 `2` |
| `hearth market <command>` | — | stdout 上已安装的 bundle、搜索命中、安装结果或漂移 | `0`、`1` 或 `2` |
| `hearth help` | `-h`, `--help`、完全不带参数 | stdout 上的用法文本 | `0` |
| `hearth doctor` | — | stdout 上给人读的检查报告 | `0` 或 `1` |
| `hearth doctor --json` | — | stdout 上的 JSON 报告 | `0` 或 `1` |
| `hearth doctor --prune-profiles` | — | 删除本工具创建的 Windows AppContainer profile，然后报告发生了什么 | `0` 或 `1` |

`hearth session` 今天有两个已实现的子命令：`fork` 与 `archive`。`list`、`show`、
`resume` 和 `export` 是保留字（§2.6），它们以退出码 `2` 加一条消息收场，而不是假装能用。
`hearth resume` 不是命令：恢复是 `hearth chat --resume <id>`（§2.3），因为恢复是在一份已有
日志上跑一个 chat 轮次，不是一项独立操作。

`--version` 是一条快速路径：它不碰配置文件、数据目录、工作区或网络，所以在一个坏掉的环境里
它照样能用。

### 2.1 `hearth --version`

stdout，恰好一行：

```text
hearth 0.0.0-dev (unknown) windows/amd64 go1.27.0
```

格式：`hearth {version} ({commit}) {goos}/{goarch} {goVersion}`，后面跟一个 `\n`。`{goVersion}`
是构建这个二进制时所用的 Go 运行时版本（`runtime.Version()`）；非 Go 的重新实现应当在自己的
运行时版本处打印它，若没有运行时版本就打印 `unknown`。不会往 stderr 写任何东西。退出码 `0`。

### 2.2 `hearth help`

不带参数的 `hearth`、`hearth help`、`hearth -h` 与 `hearth --help` 都把这同一段文本打到
**stdout** 并退出 `0`。这段文本是：

```text
hearth — an agent harness

usage:
  hearth --version              print version (fast path, no config)
  hearth chat [flags] [prompt]  run one turn and stream the answer to stdout
  hearth "<prompt>"             shorthand for hearth chat
  hearth web [flags]            serve the browser UI on loopback
  hearth session <command>      operate on a session log without running a turn
  hearth import <old-dir>       import a session tree from the old harness
  hearth index [flags]          rebuild or query the search index
  hearth goal <command>         record, inspect or close a goal
  hearth schedule <command>     record recurring prompts and run the ones due
  hearth plugin <command>       inspect the installed plugin set
  hearth expert <command>       list or show the expert roster
  hearth skill <command>        list or show the skills the model was offered
  hearth team <command>         list teams, or run one over a shared board
  hearth market <command>       install or remove plugin bundles
  hearth doctor [flags]         check the environment
  hearth help                   this message

docs: docs/README.md
```

精确地说：第一行用的是 em dash（U+2014），其后有一个空行；十六条缩进两个空格的命令行，用填充
让描述都从同一列开始；然后一个空行，最后是 `docs:` 那行。整块以一个 `\n` 结尾。不会往 stderr
写任何东西，也不会发生任何配置或文件系统访问。

`cmd/forge/main_test.go` 把这段文本与二进制打印的常量逐字节钉住，所以两者不会漂移：加一个命令
意味着在同一次改动里编辑那个常量、这一块和那个测试。

### 2.3 `hearth chat [flags] [prompt]`

对着工作区跑一个 agent 轮次，并把模型的答案流式写到 stdout。`hearth "<prompt>"` 是同一条命令的
简写（§2.5 解释了什么时候第一个参数是 prompt，而不是一个打错的子命令）。

```text
$ hearth chat "summarize internal/session"
The session package owns the append-only event log…
hearth chat: model=gpt-4o-mini sandbox=workspace-write tools=11 approve=never
hearth chat: session=20260101-120000-a1b2c3
hearth chat: log=C:\Users\Administrator\.hearth\sessions\20260101-120000-a1b2c3.jsonl
hearth chat: turn=1 steps=1 reason=completed usage in=812 out=233 total=1045
```

stdout/stderr 的分工是契约：

- **stdout** 只承载助手文本，别的一律不放。分片是到达即写，所以答案是流式的；每个分片之后没有
  换行。如果这一轮产生的文本不以 `\n` 结尾，就在最后一个分片之后补上恰好一个 `\n`，好让 shell
  提示符从新的一行开始。在吐出部分文本之后才失败的一轮，会保留那段文本，并在诊断之前得到那个
  换行。工具输出永远不进 stdout：工具的 `fs_read` 结果、`shell` 命令的 stdout、以及一个审批
  问题，都是诊断，不是答案。
- **stderr** 承载每一条诊断，一行一条，全部以 `hearth chat: ` 开头。顺序是：
  `model={id} sandbox={mode} tools={n} approve={mode}`、`session=`、`log=`，然后是
  `turn={n} steps={n} reason={r} usage in={n} out={n} total={n}`，然后在这一轮失败时给一条
  一行的失败诊断，再然后——仅当某次工具调用因为 `--approve=never` 留下没人可问而被拒时——一条
  提示行，点名 `--approve=always` 和 `--approve=prompt`。只要这一轮开始了，汇总行就会打印，
  包括失败与 Ctrl+C 的情形。在 `--approve=prompt` 下问到并作答的审批也走 stderr，每条之后跟
  `hearth chat: approval: allow` 或 `hearth chat: approval: deny`。
- 因此 `hearth chat "q" > answer.txt` 只捕获答案，而 `2> err.txt` 只捕获诊断（加上任何工具或
  审批文本）。

`usage` 数的是这一轮累加的 token：`in` 是 prompt token，`out` 是 completion token，`total`
是两者之和（缓存 token 不计入）。

#### 提示词

- flag 之后的位置参数以单个空格连接：`hearth chat --model m fix the bug` 的 prompt 是
  `fix the bug`。flag 可以在 prompt **之前或之后**：`cmd/forge/flags.go`（`reorderFlags`）在
  `flag` 解析它们之前把 flag 挪到位置参数前面，让每个带值 flag 紧挨着它的值，并尊重 `--`
  终止符（`--` 之后的一切都保留为 prompt 文本，即使它以 `-` 开头）。它认识的带值 flag 有
  `--model`、`--session`、`--resume`、`--fork`、`--system`、`--approve` 和 `--sandbox`；
  未知 flag 出现在哪里都仍然是错误。
- 没有位置参数时，stdin 被读到 EOF，末尾的 `\r`/`\n` 被去掉：`echo hi | hearth chat` 和
  `hearth chat < prompt.txt` 都能用。这一点加上 `--approve=prompt` 的审批问题，是 `hearth`
  读 stdin 的全部路径。
- 空的或只含空白的 prompt 是用法错误：退出 `2`，stderr 上是
  `hearth chat: empty prompt: pass it as an argument or pipe it on stdin`，且不创建会话文件。
- 出现在 `--` 之前的任何位置的 `-h` / `--help` 会把这条命令的用法文本
  （`usage: hearth chat [flags] [prompt]` …）打到 stdout 并退出 `0`。

#### flag

| flag | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--model` | string | 配置里的 `model` | 按原样发给 provider 的模型 id。flag 与配置都没有时：退出 `1`，`hearth chat: no model configured: …`，且在任何网络调用之前。 |
| `--session` | string | 生成的 | 会话 id，它同时也是日志文件名。必须匹配 `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`；否则退出 `2`，`hearth chat: invalid --session "…": …`，这个判定发生在配置、stdin 或任何文件访问之前。已存在的 id 会追加到那个会话的日志。 |
| `--system` | string | 空 | 这一轮的系统提示词；作为第一条模型消息发送，不记进日志。 |
| `--approve` | string 枚举 | `never` | 对沙箱策略标为 `ask` 的工具调用给出的同意：`never`（拒绝，不问任何人）、`always`（放行每一次调用，记录为 `flag:--approve=always`）、`prompt`（在 stderr 上提问，从 stdin 读一行）。任何其他值都是用法错误：退出 `2`，`hearth chat: invalid --approve "…": use never, always or prompt`。 |
| `--no-tools` | bool | `false` | 让会话完全不带工具启动：请求不宣告任何工具（见下文“工具与审批”）。把它与 `never` 之外的任何 `--approve` 值组合都是用法错误，退出 `2`。 |
| `--json` | bool | `false` | 在 stdout 上写一个报告对象，而不是流式输出（见下文）。诊断仍然去 stderr。 |
| `--resume <id>` | string | — | 继续已存在的会话 `<id>`，而不是新起一个：下一个轮次号是日志里最高的轮次号加一，且模型能看到之前的对话。id 未知、日志在等待审批、或者另一个进程正在写这个会话时拒绝（退出 `1`）。与 `--fork` 和 `--session` 互斥。 |
| `--fork <id>` | string | — | 把会话 `<id>` 复制到它最后一个 `turn/end` 为止，形成一个新会话，并在那里跑这一轮；父日志永远不会被修改。`--session` 给这份拷贝命名（默认：生成的 id）。与 `--resume` 互斥。 |
| `--sandbox` | string 枚举 | 配置里的 `sandbox` | 本进程的限定模式：`read-only`、`workspace-write` 或 `full-access`。任何其他值都是用法错误：退出 `2`，`hearth chat: invalid --sandbox "…": use read-only, workspace-write or full-access`，这个判定发生在任何文件访问之前。本机无法强制执行的模式会在这一轮开始前退出 `1`，并给出 `sandbox: mode "…" cannot be enforced on this host: …`。只被部分强制执行的模式会打印一行警告（§2.8）。 |
| `--no-compact` | bool | `false` | 跳过这一轮开始前跑的那一趟压缩（§2.3“压缩”）。 |

`-flag` 与 `--flag` 两种拼写都被接受（`flag` 包）。未定义的 flag 会把 `flag` 包的错误和一段
简短的 flag 清单打到 stderr，并退出 `2`。

生成的会话 id 是 `YYYYMMDD-HHMMSS-xxxxxx`：本地时间按 `20060102-150405` 格式化，一个连字符，
然后是三个随机字节产生的六个小写十六进制数字。同一秒内的两次运行也不相同。日志路径是
`<dataDir>/sessions/<id>.jsonl`，其中 `<dataDir>` 来自配置（默认：`$HEARTH_HOME`，否则
`~/.hearth`）——也就是打印在 stderr 上的那个路径。

每一轮都追加到那份日志，日志遵循 [`events-v1`](events-v1.md)：`turn/start` … `turn/end`，
其中 `reason` 等于在 stderr 上报告的值，助手文本在 `assistant/message` 里。日志是只追加的；
`hearth chat` 永不重写或删除它。

#### 恢复与 fork

两个 flag 都是在一份已经存在的日志上开始一轮。二者都不需要 agent 里新增代码路径：一轮总是从
日志重建它的历史，所以“继续”就是普通路径加上一个更旧的文件。

`--resume <id>` 打开 `<dataDir>/sessions/<id>.jsonl` 并继续它。追加的第一个轮次是日志里最高的
轮次号加一，且请求里带着之前的对话。恢复是契约的一部分，因为进程被杀正是恢复的正常理由：

- 末尾被撕裂的记录——`write` 中途崩溃——会被丢弃：文件被截回它最后一条完整记录，stderr 上说
  `hearth chat: recovered session <id>: dropped <n> bytes of a torn final record`。那一点
  之前的每一个字节都不动。
- 没有 `turn/end` 的 `turn/start`（进程在轮次中途死了）通过追加一个带 `reason="cancelled"` 与
  `cancelledBy="cli:recovery"` 的 `turn/end` 来关闭——这是修正性事件，永远不是重写。于是下一
  轮就是它之后的那一轮。stderr 上说
  `hearth chat: recovered session <id>: closed turn <n>, which a dead process left open`。
  别的东西一概不发明：一个没被回答的 `step/start` 就保持没被回答，它的助手文本（如果有）已经
  在日志里了。

#### 压缩

在这一轮开始之前，只要配置了 provider 且日志里至少有 200 个事件（`compactMinEvents`；低于这个
下限就什么都不计划），`hearth chat` 就会对日志跑一趟压缩（`session.CompactLog`）。它最多追加
一个 `compaction/summary` 事件（[`events-v1`](events-v1.md) §5.1），并且永不重写记录。事后如果
日志里有一个摘要区段，stderr 上说 `hearth chat: compacted <n> events into a summary`；这趟
返回错误时说 `hearth chat: compaction skipped: <err>`。跳过不是轮次失败，也不改变退出码。
`--no-compact` 完全跳过这趟，没有配置 provider 的运行也一样跳过。`hearth web` 不做压缩。

`--resume` 选择拒绝，而不是让两个写者交错进同一份日志。下列每一种都退出 `1`，并且不创建、不
截断、不追加任何东西：

| 情形 | stderr | 操作者怎么办 |
| --- | --- | --- |
| `<id>.jsonl` 不存在 | `hearth chat: no such session "<id>": <path> does not exist` | 核对 id，或者新开一个会话。 |
| 最新的轮次在等待审批 | ``hearth chat: session "<id>" is waiting for approval "<approvalId>": answer it in `hearth web`, or continue from a copy that stops before that turn with `hearth chat --fork <id>` `` | 在 web 界面里回答它，或者 fork 越过它。 |
| `<id>.lock` 里写着一个活着的 pid | `hearth chat: session is being written by another process: pid <n> is writing session "<id>": let that turn finish (or stop that process), then retry` | 等那一轮跑完，或者停掉那个进程。 |

锁是一个旁车文件 `<dataDir>/sessions/<id>.lock`，里面存着写者的 pid，用 `O_CREATE|O_EXCL`
创建，并在轮次结束时删除。pid 已死、无法解析、或者就是本进程的锁是过期的，会被接管。它是
建议性的：它管的是 `hearth chat --resume` 对另一个 `hearth chat --resume`，不管 `hearth web`
——后者的轮次活在服务器的内存里，一个活着的 web 轮次在一个独立的 CLI 进程看来就像一个过期的
锁。数据目录里别的任何东西都不动，锁也不属于日志。

`--fork <id>` 写出一个**新**会话，它的日志是父会话事件逐字复制到一个切点为止，然后在那里跑这
一轮。切点默认是父会话最后一个 `turn/end`，所以 fork 从不在轮次中途开始，也从不继承一个打开
的轮次。被复制的记录保留自己的信封与 payload；只有 `session` 字段被改写为新 id，`seq` 从 `1`
重新编号，头部记录多出 `forkedFrom`、`forkedAtSeq` 与 `forkedAt`（[`events-v1`](events-v1.md)
§4.1）。整个操作期间父日志是只读的：事后它的字节完全相同，fork 也从不截断父日志里被撕裂的
尾部。

`--fork` 对不存在的父会话、或 `--session` 指定的一个日志已存在的 id 予以拒绝（退出 `1`，什么
都不创建）——拷贝永不覆盖日志，所以请另选一个 id。`hearth chat --fork` 还会在这一轮自己的行
之前，往 stderr 打印
`hearth chat: forked <parent> at seq <n> into session <new> (<events> events)`。

#### 工具与审批

`hearth chat` 构建工具流水线，并在每次模型调用时把它宣告出去。工作区是本进程的当前目录，在
会话日志被打开之前用 `os.Getwd` 解析；如果那一步失败，命令以 `hearth chat: working directory: …`
退出 `1`，而不是回退到一个空路径。同一个值被传给 agent（记录在 `session/start` 里）和工具
执行器。

十个工具是无条件注册的：七个文件/进程工具（`fs_read`、`fs_write`、`fs_edit`、`fs_glob`、
`fs_grep`、`shell`、`todo_write`）、`read_image`、`worktree` 和 `present`。流水线随后再加上
M5 桥接与已配置的平台能力所贡献的东西，所以 stderr 上的 `tools={n}`——注册表里一切东西的
计数——至少是十，并随配置增长：

- 任何会写会话日志的面上再加三个——也就是 `hearth chat` 与 `hearth web`，所以一段对话宣告
  十三个：`compress`、`decompress` 和 `search_context` 回答关于正在被写的那份日志的问题，且
  永不编辑它（[`events-v1`](events-v1.md) §4.0.3）。一个不给 wiring 任何日志的调用者拿不到
  它们中的任何一个，因为一个块是指向某份具体日志的地址，而一个解析不到日志的工具就是个陷阱；

- 每个 MCP 工具一个工具，命名 `mcp__<server>__<tool>`，其中 `_` 转义为 `__`，`.` 转义为
  `_dot_`（[`m5-surface`](m5-surface.md) §5），任何模式下策略都是 `ask`；
- 当 `skills.roots` 产出至少一个可用的 `SKILL.md` 时的 `skill`；
- 当 `subagent.maxDepth` 大于零时的 `subagent`；
- 当网络客户端能被构建出来时的 `web_fetch`，以及当 `search.endpoint` 被设置时（§2.5）的
  `web_search`；
- 永远有的 `present`，因为每个命令都有一个数据目录来记录一个被展示的文件；
- 当 `plugins.enabled` 列出一个能加载的插件时，每个插件工具一个工具，命名
  `<plugin>.<tool>`；任何模式下策略都是 `ask`；
- 当专家名册里至少有一位可召唤的专家时的 `expert`；
- 当 `team.maxParallel` 大于零且名册里有一个团队时的 `team`；任何模式下策略都是 `ask`，因为
  它启动的每一个 worker 都要花钱。

一个建不起来的能力是一个**缺席**的工具，而不是一个会失败的工具：模型读工具清单，就知道这一轮
能做什么。一个坏的 `caFile` 会让 `web_fetch` 和 `web_search` 缺席，并往 stderr 写一条警告。

`--json` 的 `tools` 是所宣告的一切的排序名字列表，桥接包含在内。一个配置坏了的 MCP 服务器或
技能根从来不是致命的：那个桥接不贡献任何东西，并往 stderr 写一条警告。下面的表是那十个无条件
工具、以及搭在会话日志上的那三个工具的策略。

| 工具 | 沙箱策略 `read-only` | `workspace-write` | `full-access` |
| --- | --- | --- | --- |
| `fs_read`、`fs_glob`、`fs_grep`、`read_image` | allow | allow | allow |
| `fs_write`、`fs_edit` | deny | allow，但仅限工作区内部 | allow |
| `todo_write` | deny | allow | allow |
| `shell`、`worktree` | deny | **ask** | allow |
| `present` | allow | allow | allow |
| `compress`、`decompress`、`search_context` | allow | allow | allow |
| `web_fetch`、`web_search` | **ask** | **ask** | allow |
| 未列出的任何工具 | deny | deny | allow |

第十一个工具 `goal_end` 只在轮次属于一个目标循环（`hearth chat --goal`）时才注册：它记录
那个循环读来决定是否开下一轮的判定。它在任何模式下策略都是 `allow`，因为结束一个目标不碰任何
文件——把它拦起来，会让一个被限定的轮次无法停下那件它被要求完成的事。

模式来自 `config.json` 里的 `sandbox`（`read-only`、`workspace-write`、`full-access`；默认
`workspace-write`），而 `--sandbox` 为一次调用覆盖它。它在 stderr 上以 `sandbox={mode}` 报告。

`web_fetch` 与 `web_search` 在两种受限模式下特意是 `ask`：沙箱禁止子进程的网络，但这两个是
**在** harness 进程里跑的，而 harness 进程不受沙箱约束，所以审批提示是一个模型选定的 URL 与
操作者的网络之间唯一的东西。`full-access` 跳过它，因为操作者已经说了这一轮可以干任何事。

模型可以在一步里要几个工具；agent 在这一轮内部执行它们，并追加 `tool/call` 与 `tool/result`
事件。一个失败了的、被拒绝的、或者超时了的工具是**数据**：结果里带着原因，模型看得见，这一轮
继续。它不改变退出码——只有以 `error` 结束的轮次才改变（§2.3 的退出码）。

一个 `ask` 的策略决定被交给一个*审批者*。审批者是什么取决于 `--approve`：

| `--approve` | 审批者 | 效果 |
| --- | --- | --- |
| `never`（默认） | 无 | 不用问任何人就直接拒绝这次请求。`tool/result` 里 `ok=false`、`code="no_approver"`，并且**没有** `approval/request` 或 `approval/decision` 被记录——没人被问过，所以没有什么可记。汇总行之后会打印一行 stderr 提示（`--approve=always` / `--approve=prompt`）。 |
| `always` | 放行一切 | `approval/request`，然后 `approval/decision`，其中 `decidedBy="flag:--approve=always"`、`decision="allow"`。 |
| `prompt` | 人 | 每次请求在 stderr 上问一个问题，`hearth chat: approve tool={name} in {workspace}? args=… [y/N] `，然后从 stdin 读一行：`y` 或 `yes`（不区分大小写）放行，其他任何东西——包括 EOF——都拒绝。`decidedBy="user"`。一次拒绝是 `code="denied_refused"`。 |

`--approve=prompt` 要求 stdin 是一个终端。在管道、重定向或 `go test` 之下它是用法错误：
退出 `2`，stderr 上带着

```text
hearth chat: --approve=prompt needs a terminal on stdin: use --approve=always to allow every tool call, or run hearth chat in a terminal
```

并且不创建会话文件。它在管道上从不阻塞。因为不带 prompt 参数的 `hearth chat` 会把 stdin 读到
EOF，`--approve=prompt` 需要把 prompt 作为参数给出；否则审批看到的是 EOF，于是拒绝。

`--no-tools` 什么都不注册：请求不宣告任何工具（`--json` 的 `tools` 是 `[]`），而一个仍然索要
工具的模型会得到一条被记录下来的拒绝。它不能与 `--approve=always` 或 `--approve=prompt` 组合
——没有工具就没有什么可审批——那种组合是用法错误。

#### 环境

| 变量 | 效果 |
| --- | --- |
| `HEARTH_BASE_URL` | provider 的 base URL。默认 `https://api.openai.com/v1`。 |
| `HEARTH_API_KEY` | provider 的 API key，以 `Authorization: Bearer …` 发送。未设置意味着没有 `Authorization` 头；它不是用法错误。 |
| `HEARTH_HOME` | hearth home，同 `doctor`；数据目录默认就是它，所以它也决定 `sessions/` 落在哪里。 |

endpoint 与 key 的优先级是：显式的 adapter 选项，然后是环境变量，然后是默认值。CLI 不传任何
显式选项，所以这些变量是把 `hearth chat` 指向非默认 endpoint 的唯一办法。

#### `--json`

stdout 上一个 JSON 对象，紧凑（无缩进），后面跟一个 `\n`：

```json
{"turn":1,"reason":"completed","steps":2,"text":"Hello, hearth","usage":{"inputTokens":7,"outputTokens":3},"events":11,"tools":["fs_edit","fs_glob","fs_grep","fs_read","fs_write","shell","todo_write"],"approvals":[{"callId":"call-1","tool":"shell","decision":"allow"}]}
```

| 成员 | 类型 | 含义 |
| --- | --- | --- |
| `turn` | integer | 会话内的轮次号；新会话为 `1`。 |
| `reason` | string | `turn/end` 的原因：`completed`、`error` 或 `cancelled`。 |
| `steps` | integer | 尝试过的模型调用数；一次失败的调用也算一次。 |
| `text` | string | 这一轮的助手文本；这一轮没产出文本时为空。 |
| `usage` | object | `inputTokens` 与 `outputTokens`，在这一轮内累加；`cacheReadTokens` / `cacheWriteTokens` 只在非零时出现。 |
| `events` | integer | 这次调用追加到日志的事件数。 |
| `tools` | array of string | 请求所宣告的名字，已排序。`--no-tools` 下是 `[]`，从不是 `null`。 |
| `approvals` | array of object | 这一轮内做出的审批，按事件顺序；没有时是 `[]`，从不是 `null`。 |
| `approvals[].callId` | string | 这条审批所回答的 `tool/call` id。 |
| `approvals[].tool` | string | 工具名，例如 `shell`。 |
| `approvals[].decision` | string 枚举 | `allow` 或 `deny`，与记录在 `approval/decision` 里的一致。 |

`tools` 是被*宣告*的东西，不是跑过的东西；模型可能一个都不用。`approvals` 只列这一轮内做出的
决定——因缺少审批者而被拒绝的一次请求（`--approve=never`）不算，因为没人被问过。做出决定的
一方（`decidedBy`）留在事件日志里，这里不重复。

这个对象在**每一条**开始过轮次的路径上都会被写出——包括 `reason` 为 `error` 与 `cancelled` 的
情形——并且在这种模式下它是 stdout 上唯一的东西（答案不再同时流式输出）。退出码仍然遵循下面
那张表。

#### 退出码

| 码 | 何时 |
| --- | --- |
| `0` | 这一轮以 `completed` 结束。因 `max_tokens` 停下也算完成。被策略拒绝、被审批者拒绝、失败或超时的工具都是模型读到的数据，不是失败的轮次：这一轮仍然以 `completed` 结束，退出码是 `0`。 |
| `1` | 这一轮跑不起来或以 `error` 结束：配置读不了、没有配置模型、工作目录无法解析、provider/transport 失败、流断了，或者模型索要了不存在的工具（在 `--no-tools` 下索要工具本身也算）。一行诊断打到 stderr。 |
| `2` | 用法错误：未知 flag、非法的 `--session`、空的 prompt、未知的 `--approve` 值、`--no-tools` 与 `--approve=always`/`--approve=prompt` 组合，或者 `--approve=prompt` 配一个不是终端的 stdin。不创建会话文件。 |
| `130` | Ctrl+C：第一次信号取消这一轮，在日志里以原因 `cancelled` 关闭它，打印汇总行并退出 `130`；第二次 Ctrl+C 立刻退出，不做栈展开。 |

### 2.4 `hearth doctor [flags]`

跑环境检查并报告它们。这些检查是只读的，唯一例外是数据目录探针，它在数据目录里创建并删除
`.write-probe`。

flag：

| flag | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `-json` / `--json` | bool | `false` | 输出 JSON 报告而不是文本报告。两种拼写都被接受（`flag` 包）。 |
| `-prune-profiles` / `--prune-profiles` | bool | `false` | 仅 Windows：删除本工具创建的每一个 `hearth.sandbox.*` 包 profile，然后打印一行汇总（带 `--json` 时是一个 JSON 对象）。它跑*而不是*那些检查，因为这操作会改动机器，而检查必须保持只读。只要有任何一个 profile 删不掉就退出 `1`。 |

检查，按这个顺序（十一个总是全跑；顺序与名字都是契约）：

| # | 名字 | 验证什么 |
| --- | --- | --- |
| 1 | `config` | 找到一份配置文件并解析成功；否则用默认值。 |
| 2 | `data dir` | 数据目录存在（或能被创建）并且可写。 |
| 3 | `workspace` | 当前工作区存在；注明它是不是一个 git 仓库。 |
| 4 | `git` | `PATH` 上有一个 `git` 可执行文件。 |
| 5 | `sandbox` | 验证器为所配置的模式构造真正的执行者，并报告它的能力。`full-access` 下（按设计什么都没被强制）或 spec 非法时是 `skip`；本机无法强制执行该模式时是 `fail`；该模式只被部分强制时是 `warn`。 |
| 6 | `disk` | 数据目录所在卷的空闲空间：低于 512 MiB 是 `fail`，低于 2 GiB 是 `warn`。 |
| 7 | `model` | 对 provider 的 base URL 发一个 `HEAD` 请求，超时 2 秒。从不是 `fail`：401/403 时是 `warn`（指向 `HEARTH_API_KEY`），传输有问题时是 `warn`，没有配置模型时是 `skip`。 |
| 8 | `mcp` | 只看 MCP 配置——不拉起任何服务器。没有配置服务器时是 `skip`，有条目被丢弃或重复时是 `warn`（原因在 hint 里），`ok` 时列出被接受的名字。 |
| 9 | `proxy` | HTTP(S) 代理环境变量，如果有。 |
| 10 | `pty` | PTY 可用性。在 windows/linux/darwin 上是 `skip`，细节是 `shell runs on pipes, not a pty: interactive programs will not render`（shell 工具刻意用管道；M0 的试验证明 ConPTY 能用，但产品里没有接 PTY）；其他任何平台上都是 `warn`。 |
| 11 | `office` | 这台机器能不能做文档工作，用跑程序的方式探测，而不是去找它：一个 Python 启动器（`python`、`python3`、`py -3`），由那个启动器导入的 `python-docx`、`openpyxl` 与 `python-pptx` 三个库，以及用于格式转换的 `soffice`。四项都有回应时是 `ok`；缺东西时是 `warn`，并带上那句拒绝的话；某个探针定不下来时（超时、启动器起不来、预算里没时间了）是 `skip`。从不是 `fail`：缺一个库意味着 office 技能必须拒绝写文档，而那不是 harness 坏了。 |

每项检查的状态词汇：`ok`、`warn`、`fail`、`skip`。**只有 `fail` 影响退出码**；`warn` 与
`skip` 不影响。

#### 文本输出

```text
hearth 0.0.0-dev (unknown)  windows/amd64  go1.27.0
config     C:\Users\Administrator\.hearth\config.json
workspace  D:\魔改版Agent框架\hearth

OK    config     using defaults (no config file)
FAIL  data dir   config: data dir C:\Users\Administrator\.hearth is not writable: open C:\Users\Administrator\.hearth\.write-probe: Access is denied.
                 -> pick a writable dataDir in C:\Users\Administrator\.hearth\config.json
OK    workspace  D:\魔改版Agent框架\hearth (git repo)
OK    git        E:\Tools\PortableGit\cmd\git.exe
WARN  sandbox    windows-job-object: windows job object: process tree (KILL_ON_JOB_CLOSE), process memory 1 GiB, job memory 2 GiB, active processes 64 — filesystem and network are NOT confined
                 -> the enforcer installed, but filesystem writes are not confined on this host; the detail is its own account of what it does enforce
OK    disk       38.5 GiB free of 149.1 GiB on the volume holding C:\Users\Administrator\.hearth
SKIP  model      no model configured
SKIP  mcp        no MCP servers configured
OK    proxy      none set
SKIP  pty        shell runs on pipes, not a pty: interactive programs will not render
                 -> ConPTY/pty is available on this platform and can be wired when a terminal is needed
WARN  office     docx=python 3.14 xlsx=python 3.14 pptx=python 3.14 convert=missing
                 -> unavailable on this machine: format conversion (LibreOffice soffice). install LibreOffice for format conversion; say so instead of writing an empty file.

11 checks in 22 ms
```

逐行看结构：

1. `hearth {version} ({commit})  {goos}/{goarch}  {goVersion}`——三组之间是两个空格。
2. `config     {configPath}`——标签在一个 11 列的字段里左对齐，然后是路径。没有配置文件时，
   路径是那个*本来会*被用到的路径，不是空。
3. `workspace  {workspacePath}`——同样的 11 列标签字段。
4. 一个空行。
5. 每项检查一行：`{STATUS}` 填充到最宽的状态，至少两个空格，`{name}` 填充到最宽的名字，
   至少两个空格，`{detail}`。`STATUS` 是大写化的状态（`OK`、`WARN`、`FAIL`、`SKIP`）。列宽
   是在整张表的所有行上算出来的（一个 tabwriter，最小宽度 0、padding 2、填充字符是空格），
   所以它们会随内容移动。
6. 对带 hint 的检查，多一行：状态列为空、名字列为空，细节列里放 `-> {hint}`。hint 被缩进到
   表格的细节列。
7. 一个空行。
8. `{n} checks in {d} ms`——`n` 是跑过的检查数（今天是 11），`d` 是耗时的毫秒数。两者都是
   整数。耗时每次运行都不同，不是契约。

报告写到 stdout。退出码：任何检查状态为 `fail` 就是 `1`，否则 `0`。

#### JSON 输出（`--json`）

一个 JSON 对象，2 空格缩进（Go 的 `json.Encoder` 配 `SetIndent("", "  ")`），后面跟一个
`\n`：

```json
{
  "version": "0.0.0-dev (unknown)",
  "os": "windows",
  "arch": "amd64",
  "goVersion": "go1.27.0",
  "configPath": "C:\\Users\\Administrator\\.hearth\\config.json",
  "workspace": "D:\\魔改版Agent框架\\hearth",
  "durationMs": 16,
  "checks": [
    {
      "name": "config",
      "status": "ok",
      "detail": "using defaults (no config file)"
    },
    {
      "name": "data dir",
      "status": "fail",
      "detail": "config: data dir C:\\Users\\Administrator\\.hearth is not writable: open C:\\Users\\Administrator\\.hearth\\.write-probe: Access is denied.",
      "hint": "pick a writable dataDir in C:\\Users\\Administrator\\.hearth\\config.json"
    }
  ]
}
```

（节选：真实的一次运行在 `checks` 里有十一个条目。）

模式：

| 成员 | 类型 | 出现 | 含义 |
| --- | --- | --- | --- |
| `version` | string | 总有 | `Version` 与 commit 组合，对这个构建恰好是 `"0.0.0-dev (unknown)"`。注意这是整条版本行里的第一组，不是 `--version` 打在它前面的那个裸 `0.0.0-dev`。 |
| `os` | string | 总有 | `runtime.GOOS`。 |
| `arch` | string | 总有 | `runtime.GOARCH`。 |
| `goVersion` | string | 总有 | 构建工具链版本。 |
| `configPath` | string | 总有 | 所使用的配置文件路径（已存在的或默认的）。 |
| `workspace` | string | 总有 | 被检查的工作区目录。 |
| `durationMs` | integer | 总有 | 耗时的毫秒数；不稳定。 |
| `checks` | array of object | 总有 | 十一个条目，顺序与上面那张表一致。 |
| `checks[].name` | string | 总有 | 检查名，例如 `data dir`。 |
| `checks[].status` | string 枚举 | 总有 | `ok` \| `warn` \| `fail` \| `skip`。 |
| `checks[].detail` | string | 总有 | 一行解释；可以为空。 |
| `checks[].hint` | string | **为空时省略** | 接下来该试什么。 |

输出里的键顺序是结构体顺序（如所示），但消费者**不得**依赖它。`hint` 只在非空时出现。
`checks` 是数组，从不是 `null`。在兼容的发布里可能新增成员；消费者必须忽略未知成员。退出码
与文本模式相同：当且仅当有检查状态为 `fail` 时是 `1`。

#### 环境

| 变量 | 效果 |
| --- | --- |
| `HEARTH_HOME` | 覆盖 hearth home 目录。配置路径变成 `$HEARTH_HOME/config.json`；默认数据目录是 `$HEARTH_HOME`。 |
| *(未设置)* | home 是 `~/.hearth`；配置路径 `~/.hearth/config.json`。 |

如果配置路径无法确定（根本没有 home 目录），`doctor` 退出 `1`。本项目开发所在的文件系统沙箱
只允许在仓库内部写，所以默认 home 目录会报告 `data dir … is not writable`，`doctor` 退出
`1`——那是正确行为，不是 bug。

#### `doctor` 周围的用法错误

未定义的 flag 是用法错误：退出 `2`，stderr 上带着 Go `flag` 包的消息：

```text
flag provided but not defined: -nope
Usage of doctor:
  -json
    	machine-readable output
  -prune-profiles
    	delete the Windows sandbox profiles this tool created
```

`doctor` 今天忽略它 flag 之后的位置参数：`hearth doctor junk` **不是**用法错误（绝不退出
`2`），退出码跟任何一次普通的 `doctor` 运行一样由检查决定——实测在不可写的数据目录下是 `1`，
在可写的数据目录下是 `0`。不要依赖这一点：它是未规定的，将来可能变成用法错误。

### 2.5 未知命令

```text
$ hearth nope
hearth: unknown command "nope"

<usage text>
```

`hearth: unknown command "nope"` 那一行、一个空行、以及完整的用法文本都去 **stderr**，退出码
是 `2`。用法文本与 `hearth help`（它去 stdout）逐字节相同。命令名以 `%q` 的语义加引号（对
不可打印字符用 JSON 风格的转义）。

只有*单个裸 ASCII 词*才会这样被当成打错的子命令。任何含空白或非 ASCII 字符、或以 `-` 开头的
东西都会被送去 `hearth chat`（§2.3），所以 `hearth fix the bug` 与 `hearth 总结这个仓库` 都是
chat 轮次，而 `hearth fix` 是退出 `2`。因此一个单词的 prompt 需要写显式形式：`hearth chat fix`。

---

### 2.6 `hearth session fork <parentId> [flags]`

把一个会话复制成一个新会话，而不跑轮次，供那些想要一个分叉点、稍后再跑轮次（或通过
`hearth chat --resume` 跑）的脚本使用。它与 `hearth chat --fork` 是同一个操作，只是少了那一轮。

```text
hearth session fork <parentId> [--at <seq>] [--id <newId>] [--json]
```

| flag | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--at <seq>` | uint | 父会话最后一个 `turn/end` | 要复制的最后一条记录，含本身。必须指向一条已存在的记录，且父会话在那个位置不能有打开的轮次（`0` 被拒绝：seq 从 `1` 起是稠密的）。 |
| `--id <newId>` | string | 生成的 | 这份拷贝的 id。若 `<newId>.jsonl` 已存在则拒绝。 |
| `--json` | bool | `false` | 打印一个 JSON 对象而不是裸 id。 |
| `-h`, `--help` | flag | — | 把用法打到 stdout，退出 `0`。 |

- **stdout**：新会话 id 单独一行；带 `--json` 时恰好一个对象
  `{"parent":"…","session":"…","atSeq":n,"events":n,"bytes":n}`——`atSeq` 是父会话里的切点，
  `events` 是被复制的记录数，`bytes` 是新日志的大小。stdout 上没有别的东西。
- **stderr**：一行汇总
  `hearth session fork: forked <parent> at seq <n> into <new> (<events> events, <bytes> bytes)`，
  或者一行诊断，总以 `hearth session fork: ` 开头。
- **退出 `0`**：新日志已写出并同步。**`1`**：父会话不存在、目标 id 被占用、或者写失败——每种
  情形下目标日志都不会被留下。**`2`**：用法错误——没有父会话或多于一个、未知 flag、非法 id、
  `--at` 落在日志之外、`--at` 落在一个打开的轮次内部。

父会话只被读取。它不被加锁、不被截断、也不被重写：同一个父会话的两次 fork，或者在另一个进程的
轮次期间做 fork，看到的都是同样的字节。这份拷贝遵循 [`events-v1`](events-v1.md) §4.1：记录
相同、`session` 被改写、`seq` 从 `1` 重新编号、并且头部记录指明了父会话。

```text
$ hearth session fork 20260101-120000-a1b2c3 --id branch-1
branch-1
hearth session fork: forked 20260101-120000-a1b2c3 at seq 12 into branch-1 (12 events, 3072 bytes)
```

---

### 2.7 `hearth session archive <sessionId> [flags]`

记录一个会话不再开始任何新工作。它是 [`events-v1.md`](events-v1.md) 里记录的 `session/archived`
事件的唯一写者，也是“一个已归档的会话、或者它下面的任何东西，都不再启动新的子代理”这句话背后
的机制。

```text
hearth session archive <sessionId> [--reason <text>] [--json]
```

| flag | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--reason` | string | 空 | 为什么，留在记录里。为空时整个省略：一条什么都不说的记录，好过一条写着 "" 的记录。 |
| `--json` | bool | `false` | 打印一个 JSON 对象而不是一句话。 |
| `-h`, `--help` | flag | — | 把用法打到 stdout，退出 `0`。 |

**它是幂等的。** 已经归档过的会话不会被写第二次；命令报告 `archived <id> (already archived)`
并退出 `0`，因为一个在失败之后重跑的操作者要的是同一个状态。

- **stdout**：`archived <id>`，或 `archived <id> (already archived)`，或带 `--json` 时一个
  对象 `{"session":…,"archived":true,"already":bool,"reason":…}`。
- **stderr**，总以 `hearth session archive: ` 开头：id 指不到任何东西时是
  `no such session log: <path>`（一个点名的失败，不是沉默——否则一个笔误就变成一句自信的
  断言），或者是存储错误。
- **退出码**：`0` 已归档或本来就已归档；`1` 日志不存在或写不进去；`2` 用法错误
  （`needs a session id`、`one session id at a time`、`unknown flag`、`--reason` 没有值）。

```console
$ hearth session archive 20260101-120000-a1b2c3 --reason "branch finished"
archived 20260101-120000-a1b2c3

$ hearth session archive 20260101-120000-a1b2c3
archived 20260101-120000-a1b2c3 (already archived)
```

---

### 2.8 `hearth web [flags]`

启动 [`api.md`](api.md) 规定的回环 HTTP + WebSocket 服务器，并从同一个 origin 提供浏览器
界面：`/api/*` 与 `/healthz` 是 JSON API，其他每个路径都是界面的 app shell。它阻塞到 Ctrl+C，
然后带着 5 秒的宽限期关停，让在途工作结束。

```text
hearth web [--addr host:port] [--model <name>] [--workspace <dir>]
          [--approve web|never|always] [--sandbox <mode>] [--no-browser]
```

| flag | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--addr` | string | `127.0.0.1:8420` | 监听地址。协议 1 只允许回环：非回环的 host 被拒绝，退出 `1`（`server: refusing to bind "<addr>": protocol 1 is loopback only`）。端口 `0` 挑一个空闲端口并把它打印出来。 |
| `--model` | string | 配置里的 `model` | 这个服务器所跑的轮次用的模型 id。flag 与配置都没有时：退出 `1`，`hearth web: no model configured: pass --model <name> or set "model" in config.json`。 |
| `--workspace` | string | 当前目录 | 服务器打开的那个会话的工作区。必须是一个存在的目录；否则退出 `1`。 |
| `--approve` | string 枚举 | `web` | 谁来回答工具请求：`web`（在浏览器里，`decidedBy` 是 `user`）、`never`（拒绝，在工具结果上记为 `no_approver`）、`always`（放行，`decidedBy="flag:--approve=always"`）。`prompt` 是用法错误——退出 `2`，`hearth web: --approve=prompt reads stdin, which `hearth web` does not ask on: use --approve=web to answer in the browser`；其他任何值都是退出 `2`，`hearth web: unknown --approve value "…": use web, never or always`。 |
| `--sandbox` | string 枚举 | 配置里的 `sandbox` | 同 `chat`（§2.3）：`read-only`、`workspace-write` 或 `full-access`；未知值退出 `2`，无法强制执行的模式退出 `1`，两者都发生在监听器打开之前。 |
| `--no-browser` | bool | `false` | 打印 URL 而不是打开浏览器。 |
| `-h`, `--help` | flag | — | 把用法打到 stdout，退出 `0`。 |

`hearth web` **不接受位置参数**：任何剩下的东西都是用法错误，退出 `2`，
`hearth web: unexpected argument "<arg>": the command takes flags only`。

- **stdout**：什么都不写。
- **stderr**，按这个顺序，在监听器起来并且存在一个会话之后：
  1. `hearth web: serving <baseURL>`
  2. `hearth web: session=<id> model=<model> workspace=<workspace> sandbox=<mode> approve=<mode>`
  3. `hearth web: open <url>`——URL 把 bearer token 作为查询参数带在 hash 路由之前
     （`<baseURL>/?token=<token>#/s/<sessionId>`），所以它是一份凭证，只打印一次。
- **stderr**，只在失败时：`hearth web: <message>`，单行，带前缀。
- **退出 `0`**：监听器干净地关闭（一次 Ctrl+C）。**`1`**：服务器起不来——配置、工作区、模型、
  沙箱、token 文件、bind，或者那次端到端的会话调用。**`2`**：用法错误。**`130`**：关停期间
  第二次 Ctrl+C；进程不等待宽限期就退出。

启动时通过浏览器所用的同一套 HTTP API，为这个工作区打开（或复用）恰好一个会话；它会复用一个
工作区匹配、且状态为 `idle` 或 `running` 的已存在会话；否则创建一个。因此如果路由、认证或
token 文件坏了，命令在打印 URL 之前就失败。关掉浏览器标签页不结束任何东西：服务器一直跑到被
中断为止，每条被接受的消息一个轮次，并且它**不**压缩事件日志（§2.3“压缩”只属于
`hearth chat`）。

**从编辑器保存。** 界面可以从会话的工作区打开一个文件并把它存回去（[`api.md`](api.md) 的
§4.18 与 §4.20）。一次保存追加一个 `file/write` 事件，记下路径、字节数以及所写入字节的
SHA-256——它**不是** `tool/call`，因为没有模型参与，而 transcript 不得声称相反（见
[`events-v1.md`](events-v1.md) 的 §4.0.10）。操作者能观察到两个后果：

- 除非会话是以 `read-only` 之外的沙箱模式启动的，否则保存被拒，返回 `403`。只读会话的工具
  写不了，所以它的编辑器也写不了；两个面用不同的话描述同一个会话，正是这条规则要防止的失败。
- 一次被拒绝的保存——只读、路径在工作区之外、过期的 `expectedSha256`——追加**任何东西**都
  不做。一条关于没有发生之事的记录，比缺一条记录更糟。

**从编辑器恢复文件。** 同一个窗口可以把一次编辑扔掉，放回 git 所提交的那个版本（
[`api.md`](api.md) 的 §4.21）。这是第三类动作，所以它拿到第三个事件：一次恢复追加一个
`file/revert`，记下路径、字节数以及现在在磁盘上的那些字节的 SHA-256——不是 `file/write`，
因为没有任何东西是被手写进去的；也不是 `tool/call`，理由同 `file/write` 不是（见
[`events-v1.md`](events-v1.md) 的 §4.0.11）。三个后果：

- 请求只带路径，不带别的。没有 `expectedSha256`，也没有文本：已提交的字节是服务器自己去读的，
  所以一次恢复可以重复，也可以由一个从未打开过那个文件的客户端发出。
- git 从未提交过的文件被拒，返回 `409`，而不是被删除。一个未跟踪文件的“已提交版本”并不
  存在，在它上面做恢复会删掉一个操作者没有要求删的文件。
- 一次性保存所有脏 buffer 是界面的便利，它自己没有 wire 形态：它按顺序为每个标签页发一次
  保存，并且在失败之后继续走，好让一个过期的 buffer 不把其余的搁浅。

---

## 3. 保留，未实现

这些名字是**保留**的，好让将来的发布能加上它们而不弄坏脚本。它们**今天没有实现**：调用其中
任何一个都是未知命令，也就是退出 `2`，并在 stderr 上给出 `hearth: unknown command "..."`。
暂时不要对着它们写工具。

| 保留命令 | 计划里程碑 | 用途 |
| --- | --- | --- |
| `hearth session list` | M4 | 列出数据目录里的会话。 |
| `hearth session show <id>` | M4 | 一个会话的投影。 |
| `hearth session export <id>` | M4 | 导出事件日志 / 一份 transcript。 |
| `hearth config get` / `hearth config set` | M6 | 读和写配置键。 |
| `hearth mcp list` / `hearth mcp add` | M5 | 管理 MCP 服务器。 |
| `hearth doctor --fix` | M6 | 对失败的检查施加安全的修复。 |

`hearth session` 不再整体是保留的：`hearth session fork <parentId>` 已实现（§2.6）。
`hearth session resume <id>` 由 `hearth chat --resume <id>` 覆盖（§2.3），所以它不打算作为
一个单独的子命令。`hearth "<prompt>"` 也不再是保留的：它和 `hearth chat` 一样在 M1 就发布了
（§2.3），包括上面那条裸 prompt 规则。

特意**没有** `hearth serve`：M3 把本地 HTTP + WebSocket 服务器作为 `hearth web` 发布（§2.8），
它同时从同一个 origin 提供界面，所以一个单独的只做 serve 的命令不在计划内，那个名字继续做
未知命令。

同样保留、但目前是一个脚本而不是子命令：`go run ./scripts/bench`（性能测试台）。它可能会变成
`hearth bench`。

---

## 4. 给实现者的兼容说明

1. **退出码就是接口。** 脚本按 `2`（用法）、`1`（环境或轮次失败）、`0`（成功）和 `130`
   （被中断的 `chat`）分支。把这些保持稳定。
2. **stdout 既是给人的，也是给机器的。** `--version` 打印一行，别的什么都不打印。`--json`
   打印一个 JSON 文档，别的什么都不打印。诊断，包括用法错误，去 stderr。`hearth chat` 流式
   输出给人读的文本；当答案必须被机器解析时传 `--json`。
3. **当前输出里没有颜色、没有 ANSI、没有进度转圈。** 将来任何转圈在 stdout 不是终端时都不
   得出现，并且绝不能污染 `--json`。
4. **`doctor` 不是 harness 的存活检查。** 它报告的是环境，并且总在有界时间内结束；`git` 和
   数据目录探针可能碰文件系统。
5. `--version` 不得依赖配置解析：一份坏掉的配置不得改变版本行或它的退出码。
6. 一个新的子命令，直到本文把它连同它的退出码列出来才算做完（见 `AGENTS.md` 的
   “New subcommand”）。

---

## 5. `hearth import <old-dir>`

把一份上一代的会话导入这个构建的数据目录，作为一份新的 v1 事件日志。源是**只读的**：
`<old-dir>` 之下的任何东西都不被创建、修改或删除。新日志成为这个会话唯一的真相（法则 1）；
旧文件留在那里作为证据，而不是第二个来源。

`<old-dir>` 可以是一个旧的会话目录（`session.jsonl` 或 `session.v<N>.jsonl`，N = 1..4），
也可以是一个完整的旧数据根，后者情形下导入最新的已提交世代，其他世代作为一条警告报告。一份
压缩过的旧日志（`.jsonl.zstd`）被拒绝，而不是被静默跳过。

### 用法

```text
hearth import <old-dir> [flags]
```

| flag | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--dry-run` | bool | `false` | 打印计划，什么都不写——不写日志，不写数据目录。 |
| `--session <id>` | string | 一个带新时间戳的 id | 被导入会话的 id；必须匹配 `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`，与 `hearth chat --resume` 用的是同一条规则。 |
| `--data-dir <dir>` | string | hearth home 里配置的 `dataDir` | 要写进去的数据目录；日志落在 `<dir>/sessions/<id>.jsonl`。 |
| `--json` | bool | `false` | 在 stdout 上打印一个 JSON 文档，而不是给人看的报告。 |
| `-h`, `--help` | flag | — | 把用法打到 stdout，退出 0。 |

flag 可以出现在 `<old-dir>` 之前或之后。

### stdout / stderr

- **stdout，成功时**：第 1 行是新会话 id，第 2 行是写出的日志路径，别的什么都不写。这个两行
  的答案是机器可读的契约；脚本把第 1 行当作 id。
- **stdout，`--dry-run`**：计划——每行一个 `label  value`（`source`、`format`、`session`、
  `events`、`log`、`dropped`、`warnings`），然后每一条被丢弃的记录和每一条警告都作为缩进两个
  空格的 `- ` 条目。
- **stderr**：每一条诊断，以 `hearth import: ` 开头，一行一条。成功时它仍然报告事件数、被
  丢弃的记录和那些警告，所以一次有损的导入永远不会看起来完整。成功时不承诺 stderr 为空；
  承诺的是 stdout。
- **`--json`**：stdout 变成一个 2 空格缩进的文档——dry run：`source`、`format`、`sessionId`、
  `events`、`log`、`dropped`、`warnings`；import：`sessionId`、`log`、`events`、`dropped`、
  `warnings`。空列表是 `[]`，从不是 `null`，而 dropped/warnings 的诊断仍然去 stderr。

Dry run（最后一行是 stderr）：

```text
$ hearth import D:\old\.hearth\sessions\20260101-120000 --dry-run
source    D:\old\.hearth\sessions\20260101-120000
format    hearth-session-v4
session   20260101-120000-a1b2c3
events    5
log       C:\Users\Administrator\.hearth\sessions\20260101-120000-a1b2c3.jsonl
dropped   1
warnings  1
  - legacy/thing: no v1 event type for this old record
  - session D:\old\.hearth\sessions\20260101-120000 holds 2 committed generations (v0,v4); importing the newest, v4
hearth import: dry run, nothing was written (drop --dry-run to import)
```

### 退出码

| 码 | 何时 |
| --- | --- |
| `0` | 计划被打印了（`--dry-run`），或者日志被写出了。 |
| `1` | `<old-dir>` 不存在，或者不是一个旧会话目录；目标 id 已存在（**什么都不写**——日志是只追加的历史，所以导入既不覆盖也不改名）；解析不出数据目录；写失败。 |
| `2` | 用法错误：未知 flag、没有位置参数或多于一个、非法的 `--session`。用法去 stderr。 |

### 示例

```text
$ hearth import D:\old\.hearth\sessions\20260101-120000 --session old-1
old-1
C:\Users\Administrator\.hearth\sessions\old-1.jsonl
hearth import: imported 5 events from D:\old\.hearth\sessions\20260101-120000 into session old-1
hearth import: dropped: legacy/thing: no v1 event type for this old record
```

旧格式比 v1 更宽。映射不了的东西被报告到 stderr，永远不会被发明进日志，也永远不会被静默丢弃。

---

## 6. `hearth index [flags]`

构建、刷新或读取 hearth home 下的 SQLite 会话索引。索引是派生数据：`--rebuild` 把它丢掉，并从
`<dataDir>/sessions/*.jsonl` 重新派生每一个会话。它随时可以被删除并重建——日志仍然是唯一的
真相（法则 1），这里没有任何东西写日志。

### 用法

```text
hearth index [--rebuild] [--stats] [--search TEXT] [--json] [--db <path>]
```

| flag | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `--rebuild` | bool | `false` | 丢掉索引，并从日志重新派生它。唯一会写的调用。 |
| `--stats` | bool | `false` | 只打印总计（会话数、事件数、字节数、工作区数），不列会话。 |
| `--search <text>` | string | — | 搜索被索引的事件。词与词之间是 AND，按前缀匹配；最多打印 50 条命中（API 的上限是 200）。**空查询不是错误**：它什么都不匹配，并退出 0。 |
| `--json` | bool | `false` | 机器可读输出：一个 JSON 文档（`--search` 时是数组），2 空格缩进，键按结构体顺序，行尾没有空格。 |
| `--db <path>` | string | `<dataDir>/index.db` | 要使用的索引文件。它的父目录就是 `--rebuild` 所遍历的数据目录，所以这也是给第二个数据目录建索引的办法。 |
| `-h`, `--help` | flag | — | 把用法打到 stdout，退出 0。 |

`--stats` 与 `--search` 互斥（退出 `2`）。

### 模式

| 调用 | 效果 | 打印的 `mode` |
| --- | --- | --- |
| `hearth index` | 只读：总计加上前 20 个会话。什么都不写。 | `read-only` |
| `hearth index --rebuild` | 重建，然后打印同一份摘要。 | `rebuilt` |
| `hearth index --stats` | 只有总计。 | — |
| `hearth index --search TEXT` | 只有命中。 | — |

没有 `--rebuild` 时，索引不存在是一种失败，而不是一个空答案：退出 `1`，并带一行提示让你跑
`hearth index --rebuild`。问一个数据目录的情况，绝不能顺手造出一个声称它没有任何会话的索引。

### stdout / stderr

- **stdout**：报告。摘要与统计是一个 `label  value` 块（摘要还多一个空行和一张
  `id  updated  status  title` 表）；`--search` 每条命中打印
  `sessionId:seq  [type]  snippet`；`--json` 则打印那个文档。列用 tabwriter：两空格的间隔，
  没有一行以填充空格结尾。
- **stderr**：只有诊断，以 `hearth index: ` 开头。那里不写别的东西，所以一次干净的运行 stderr
  是空的。

```text
$ hearth index --rebuild
index       C:\Users\Administrator\.hearth\index.db
mode        rebuilt
sessions    3
events      7
bytes       916
workspaces  2

id  updated               status  title
s1  2026-07-06T15:34:00Z  idle    hello alpha
s2  2026-07-06T15:34:00Z  idle    gamma delta
s3  2026-07-06T15:34:00Z  idle    hello again alpha
```

`bytes` 是最近一次重建所覆盖日志的总大小；在新日志被追加之后，它就陈旧了，直到下一次
`--rebuild`——这正是默认模式要报告自己的 `mode` 的原因。

### 退出码

| 码 | 何时 |
| --- | --- |
| `0` | 报告被打印了。 |
| `1` | 那个路径上没有索引，而 `--rebuild` 没有给出；解析不出数据目录；SQLite 打开、读取或重建失败。 |
| `2` | 用法错误：未知 flag、一个位置参数、`--stats` 与 `--search` 同时给出。用法去 stderr。 |

### 示例

```text
$ hearth index --search hello --json
[
  {
    "sessionId": "s1",
    "seq": 2,
    "turn": 0,
    "type": "user/message",
    "snippet": "hello alpha"
  },
  {
    "sessionId": "s3",
    "seq": 2,
    "turn": 0,
    "type": "user/message",
    "snippet": "hello again alpha"
  }
]
```

一次什么都没匹配到的搜索什么都不打印（文本），或者打印 `[]`（JSON），并且仍然退出 `0`：一个
空结果是一个答案，不是失败。

## 7. `hearth plugin <command>`

### 用法

```text
usage: hearth plugin <command> [flags]

commands:
  new <name>           write a working plugin skeleton under the plugin root
  list                 show every installed plugin, its runtime and state
  start <name>         start the plugin and print its handshake
  stop <name>          send shutdown to a plugin started in this process
  call <name> <tool>   call one of the plugin's tools with JSON arguments
  doctor               show the plugin root and which runtimes exist

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory to read from
  --dir <dir>          plugin root (default: <dataDir>/plugins)
  --args <json>        JSON arguments for `call` (default: {})
  --timeout <ms>       per-request budget in milliseconds (default: 10000)
  --runtime <name>     runtime for `new`: node (default), python or exec
  --description <text> one-line description for `new`
  --force              let `new` replace a directory that already exists
  -h, --help           this message
```

一张 flag 表服务全部六个子命令。某个子命令用不到的 flag 是被接受并忽略，而不是被拒绝，所以
`--json` 在 `new` 上和它在 `list` 上一样能用。

### 插件从哪里读

`list`、`doctor`、`start` 与 `call` 读**两个**根：插件根（`<dataDir>/plugins`，或 `--dir`）
以及 market 的 bundle 目录（`<dataDir>/market/bundles`）。因此一个由
`hearth market install` 安装的插件对 `plugin list` 可见，也能按名字直接加载，不需要任何额外
步骤——market 的安装承诺不会终止在一个没人加载的目录上。`new` 只写插件根；它从不写进 market
的存储。

两个根里都存在的名字被**拒绝，而不是按顺序解析**：两个目录里可能是不同的代码，而跑一个操作者
并不想跑的那个，比不跑更糟。`list`/`doctor` 把它报告为 broken 并给出两个路径，`start`/`call`
拒绝它。market 目录不存在是正常情形（还没装任何东西），不是错误。

### 哪些子命令会拉起进程

`start` 与 `call` 会 spawn 插件。`list`、`doctor` 与 `new` 不会：一个为了描述而跑遍每个已安装
插件的列举，等于一个执行了操作者只想看看的代码的列举。`doctor` 用 `<program> --version` 探测
解释器，那是对*解释器*的一次 spawn，从不是对插件的。

### `doctor`

探针集合是**从已安装清单派生**的，不是固定的。只有至少一个已安装插件要求的解释器运行时才会被
探测，每个运行时探一次，每个都报告需要它的插件。什么都没安装时，它打印
`runtimes:  none needed (nothing installed)` 并且什么都不探测。

`runtime: "exec"` 没有解释器，所以它按另一种方式检查：每个 exec 插件一行 `entry:`，报告
`<pluginDir>/<entry>` **是不是一个文件**。它从不被 spawn——`--version` 不属于插件协议，而一个
原生插件从 stdin 读帧，所以探测它要么挂住、要么测到无关的东西。因此 `found` 的意思是*存在*，
不是*有效*；校验一个二进制需要运行它，而这个子命令绝不能这么做。

```
root:      <pluginRoot>
exists:    yes | no (nothing installed yet)
plugins:   <n>
broken:    <n>
runtime:   node     found | missing (<plugin names>) cannot start
entry:     <plugin>  <entry> found | MISSING (this plugin cannot start)
note:      this command starts nothing; `hearth plugin start <name>` does.
```

`--json` 输出 `{root, exists, plugins, broken, runtimes, entries}`，其中 `runtimes` 是
`[{runtime, available, plugins}]` 按 runtime 排序（plugins 也排序），`entries` 是
`[{plugin, entry, exists}]` 按 plugin 排序。两个数组在没人需要它们时都是空的。

只要有已安装的东西起不来就退出 `1`：一份坏掉的清单、一个不可用的解释器、或者一个缺失的 exec
入口。一份数出了三个问题却退出 `0` 的报告，会被任何只看状态的脚本读成“一切正常”。否则退出
`0`。

### `new`

写出 `<root>/<name>/`，里面恰好三个文件：

| 文件 | 内容 |
| --- | --- |
| `plugin.json` | `name`、`version`（`0.1.0`）、`description`、`runtime`、`entry` |
| `index.js` 或 `main.py` | 一个会回答 `initialize`、列出名为 `hello` 的一个工具、并调用它的插件 |
| `README.md` | 决定一个插件能不能用的三条协议规则 |

保证，每一条都有测试覆盖：

- 清单是一份 `Host::discover` 不会有任何抱怨地接受的清单——不只是形状正确的 JSON。
- 任何生成的文件里都不残留 `__TOKEN__` 占位符。
- 非法名字在创建任何东西**之前**就被拒绝。名字必须匹配 `^[a-z0-9][a-z0-9._-]{0,63}$`，
  因为它同时变成一个目录和一个工具前缀。
- 已存在的目录在没有 `--force` 时被拒绝，而拒绝之后第一代内容仍然完好。带 `--force` 时整个
  目录被替换。
- `--description` 被压平成一行（换行与制表符变成空格，两端被修剪）；连续的空格被保留，因为
  它们是操作者自己的话。空描述变成 `a <name> plugin`。
- `--runtime exec` 是用法错误，而不是生成一个桩：一个 `exec` 插件是作者已经有的程序，而一个
  带 shebang、指向没人安装过的解释器的桩，会写出一份起不来的清单。

生成的插件不需要任何编辑就能用，但在它的名字出现在 `config.json` 的 `plugins.enabled` 里
之前，它不会为 `chat` 启用。这两件事实都作为后续步骤打印出来，因为从文件里看不出任何一件。

### stdout / stderr

stdout 承载列举、握手、工具输出、doctor 报告，或者 `new` 写了什么。诊断去 stderr。带 `--json`
时，stdout 是一个 JSON 值；`new` 输出 `{name, runtime, directory, files}`，其中 `files` 已
排序。

一份坏掉的清单从不会被 `list` 静默跳过：它的路径、消息和码都去 stderr，因为一个停止解析的
插件正是它的工具缺失的原因。

### 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 命令做到了它说的事。空的插件根是 `0`，而且列举它不会创建那个目录。 |
| `1` | 一个插件起不来；一个工具报告了 `isError`；文件系统拒绝了写入；发现一个已存在的目录而没有 `--force`；没有任何可读的插件，而坏掉的插件是有的。 |
| `2` | 用法错误：未知命令或 flag、缺名字、非法名字、未知 runtime、不是 JSON 的 `--args`、非正的 `--timeout`。用法去 stderr。 |

一个跑了并失败的工具退出 `1`，并打印插件自己的话加上 `the tool itself reported a failure`。
一个从未被尝试的工具——一个未知的工具名——退出 `1`，并给出 JSON-RPC 错误码。这个区分是协议
的（`crates/plugin/PROTOCOL.md`），不是这条命令的。

### 示例

```text
$ hearth plugin new greeter --description "says hello to a name"
created node plugin greeter in <dataDir>/plugins/greeter
  README.md
  index.js
  plugin.json

next: hearth plugin call greeter hello --args '{"name":"world"}'
      add "greeter" to plugins.enabled in config.json to let chat use it

$ hearth plugin call greeter hello --args '{"name":"world"}'
hello, world
```

## 8. `hearth market <command>`

### 用法

```text
usage: hearth market <command> [flags]

commands:
  list                 show the installed bundles and their versions
  search <text>        list index entries whose name or description matches
  install <name>       install a bundle (accepts name or name@version)
  remove <name>        uninstall a bundle, refusing if a file was edited
  verify [name]        report files whose digest no longer matches
  pack <dir>           pack a plugin directory into a bundle tarball

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory (default: the hearth home's configured one)
  --root <dir>         market root (default: <dataDir>/market)
  --index <url>        index URL; required by `search` and `install`
  --force              uninstall before installing (refuses edits either way)
  --out <dir>          where `pack` writes the tarball (default: .)
  --base-url <url>     `pack`: URL prefix for the tarball; writes/merges index.json
  --os <os>            `pack`: target os for a native build (windows/linux/darwin)
  --arch <arch>        `pack`: target arch for a native build (amd64/arm64)
  -h, --help           this message
```

索引是 `{"version":1,"bundles":[...]}`。每个 bundle 带着 `name`、`version`、`description`、
`tarball`（绝对的 http(s) URL）、`sha256` 与 `size`；安装时对着实际收到的字节验证摘要，遇到
不一致、短了的主体、或者比所声明更大的主体都拒绝。版本要求是一个确切版本或 `*`；区间按名字
拒绝。

### `pack`

发布一个插件：从 `<dir>/plugin.json` 读 `name`、`version` 与 `description`，把 `<dir>` 下
每个文件打包进 `<out>/<name>-<version>[-<os>-<arch>].tar.gz`，并打印它的 `sha256` 与 `size`。

带 `--base-url` 时，一条索引条目被写到（或合并进）`<out>/index.json`，其中 `tarball` 设为
`<base-url>/<filename>`。合并键是 name + version + os + arch——与索引所强制的键相同——所以重新
打包一个改动过的插件会**替换**它的条目而不是加一条重复，而合并后的文档在写出之前会重新校验。
不带 `--base-url` 时，tarball 仍然产出，但不写索引；摘要与大小会被打印出来，好在条目将要被托管
的地方把它补全。

`--os` 与 `--arch` 必须一起给出（或者都不给），并且必须来自被接受的集合；只声明一半的平台是
用法错误，不是默认值。tarball 用 market 自己的编码器写出，所以 `pack` 打印的摘要就是 `install`
验证的摘要——同一个 codec，不是第二份实现。

```text
$ hearth market pack examples/plugins/hello --out dist --base-url https://example.invalid/p
packed hello 1.0.0 (3 file(s))
  dist/hello-1.0.0.tar.gz
  sha256 4070399c89c8…
  size   11287
  index  dist/index.json
```

### 退出码

| 代码 | 名称 | 何时 |
| --- | --- | --- |
| `0` | 成功 | `list`、`verify`、`search`、`install`、`remove` 都跑完了。空的根目录上 `verify` 退 `0`：什么都没装，就是什么都没错。 |
| `1` | 失败 | 事情没做成，工具拒绝继续：`verify` 查出某个已记录文件的摘要变了；`remove` 拒绝删这种文件；`install` 下到的字节与索引里的摘要不符；或者点名的 bundle 根本没装。 |
| `2` | 用法错误 | 未知命令；需要索引的命令没带 `--index`（没有默认索引——这个工具自己造一个，就等于发布了一个没人发布过的索引）；flag 写错。 |

### 平台特定 bundle

一个 bundle 可以同时声明 `os` 与 `arch`。两者都缺席意味着一个与平台无关的源码 bundle（一个
tarball 在它的运行时存在的地方都能装）；两者都在意味着一个恰好针对那一对平台的原生构建。被
接受的拼写是发布流水线的那套——`windows`/`linux`/`darwin` 以及 `amd64`/`arm64`——而只声明一半
（两个里只有一个）被拒绝。唯一性键是 name + version + os + arch，所以同一个版本的多个平台
构建可以同时被列出。

`search` 打印一列 `platform`（源码 bundle 显示 `any platform`），并在 stderr 上说明有多少条
命中没有针对本机的构建。`install` 解析到本机的平台：一个只针对其他平台发布的版本被拒绝，并
点名那些*确实*发布了的平台，而不是说“没有这个版本”。当一个源码 bundle 和一个原生构建都适配
本机时，install 选择拒绝而不是猜。

### 安装的 bundle 就是插件

`install` 解包到 `<root>/bundles/<name>/`，而 `hearth plugin` 把那个目录当作第二个根来读
（§7）。所以一个已安装的 bundle 立刻对 `hearth plugin list` 可见，也能按名字加载；不需要拷贝
或链接的步骤。`remove` 只删除安装记录里列出的那些文件，并且只要有任何一个被编辑过就拒绝。

### 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 命令做到了它说的事。空的搜索结果也是 `0`。 |
| `1` | 摘要不一致、被截断或过大的主体、没有针对本平台的构建、歧义的解析、被编辑的文件挡住了 remove、下载失败，或者一个源缺失/为空/不可读的 `pack`。 |
| `2` | 用法错误：未知命令或 flag、缺名字、`search`/`install` 没有 `--index`、`pack` 没有目录、`--os` 没有 `--arch`（或反过来），或者 os/arch 在被接受的集合之外。 |

## 9. `hearth skill <command>`

让扫描器本来就在读的那份名册变成可见的。`hearth_skill::scan` 是 `skill` 工具的真相来源，所以
这条命令跑的也是同一次扫描，并打印它找到的东西：一份 frontmatter 坏掉的 `SKILL.md` 会在
stderr 上被点名，而不是凭空消失；表里列出的每个技能都带着它来自的那个根。

### 用法

```text
usage: hearth skill <command> [flags]

commands:
  list                 every skill the scanner accepted, with the root it came from
  show <name>          print one skill's path and body

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory (default: the hearth home's configured dataDir)
  --root <dir>         skill root; may be repeated (default: the configured skill
                       roots, then <dataDir>/skills)
  -h, --help           this message
```

`--root` 按原样使用，并且可以重复给；不给它时，根就是配置文件里非空的 `skills.roots`
（`crates/config/src/lib.rs`），再不行就是 `<dataDir>/skills`。根按那个顺序被扫描，扫描器对
重名以第一个为准。

### 根与空

一个不存在的根是一份空名册，而它是**成功**：`list` 退出 `0` 并打印 `no skills under <root>`。
那个目录永远不会被创建，因为一个会 mkdir 的列举，会把一个笔误变成一个永久的、将来继续让人
困惑的归宿。那种情形下一条根警告——`<root>: root does not exist (yet); skipped`——会去
stderr。

唯一一种看起来像成功却不是的情形：文件在那里，而一个都没解析成功。这时 `list` 仍然打印
`no skills under <root>`，仍然在 stderr 上点名每一个被跳过的条目，并且以
`hearth skill: nothing parsed; <n> entries were skipped (see above)` 退出 `1`——模型被提供
了空，而操作者以为不是。至少有一个技能解析成功的名册退出 `0`；被跳过的兄弟条目是诊断。

### stdout / stderr

stdout 承载列举或那一个技能。文本列举是一张表：

```text
skill                    description                                                      root
office                   Create, read and edit .docx, .xlsx and .pptx by running python-… D:/app/skills
```

描述被压平成一行，并在 64 个字符处用 `…` 截断，所以一个很长的描述不会把根挤出那一行；
`--json` 带着完整的字符串。`show` 打印 `skill: <name>`、`path:  <path>`、一个空行，然后是
逐字正文。

stderr 承载扫描器自己的诊断——被跳过的条目与警告——逐字地，每条以 `hearth skill: ` 开头，
别的什么都不加，因为那些字符串是*为什么*某个技能缺失的唯一记录。它们不被转述。带 `--json`
时同样的字符串也出现在 payload 里，在 `skipped` 与 `warnings` 之下。

`show <name>` 拒绝读一个它读不了的正文：一份停止解析的 `SKILL.md` 是退出 `1` 并带上扫描器的
消息，永远不会是半截正文。

### JSON 输出（--json）

`skill list --json` 是一个对象：

| 成员 | 类型 | 出现 | 含义 |
| --- | --- | --- | --- |
| `roots` | array of string | 总有 | 所扫描的根，按顺序，用正斜杠。 |
| `skills` | array of object | 总有 | 被接受的名册，按名字再按路径排序。 |
| `skills[].name` | string | 总有 | frontmatter 里的 `name`，或者目录名。 |
| `skills[].description` | string | 总有 | frontmatter 里的 `description`，不截断。 |
| `skills[].root` | string | 总有 | 这个技能来自的那个根。 |
| `skills[].dir` | string | 总有 | 技能目录。 |
| `skills[].path` | string | 总有 | `SKILL.md` 的路径。 |
| `skills[].files` | array of string | 总有 | 技能目录里的其他文件。 |
| `skipped` | array of string | 总有 | 扫描器拒绝的目录或文件，连同它的原因。 |
| `warnings` | array of string | 总有 | 非致命的备注，包括一个不存在的根。 |

`skill show --json` 是一个对象：同样的六个技能成员，加上 `body`，也就是该文件的 frontmatter
之后的文本。

### 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 列举或那个技能被打印了。根不存在也是 `0`。 |
| `1` | 没有名为 `<name>` 的技能（会把已知名字列出来）；一份只有被跳过条目的空名册；一个读不出来的正文。 |
| `2` | 用法错误：未知命令或 flag，或者 `show` 没有名字。 |

### 示例

```text
$ hearth skill list
skill                    description                                                      root
office                   Create, read and edit .docx, .xlsx and .pptx by running python-… D:/hearth-target/skill-evidence/populated-home/skills
hearth skill: D:/hearth-target/skill-evidence/populated-home/skills/broken: frontmatter is not closed by a second --- line

$ hearth skill show nope
hearth skill: no skill named "nope"; known: office
$ echo $?
1
```

## 10. `hearth subagent <command>`

```text
usage: hearth subagent <command> [flags]
```

`hearth subagent` 有八个命令。`list` 读一个会话所启动的**直接子会话**，来源是那个会话日志上的
`subagent/catalog` 事件（events-v1 §4.0.5）——也就是后来的轮次用来再次指名同一个子会话的那个
持久身份。`tree` 递归遍历一个根之下的同一批行，每个节点一层，并把一个日志缺失、读不了、或者
由未知描述符版本写出的子会话报告为一条诊断行，而不是把它丢掉。`resolve` 把一条相对于 lead 的
名字路径变成它所指向的会话 id。`timing` 从子会话自己的日志投影出它的已关闭轮次耗时。`prompt`
往一个 `continuable` 子会话追加一轮。`interrupt` 从一个不同的进程请一个*正在跑*的子会话停下。
`providers` 列出配置里点名的进程外后端，并说明每个程序能不能被找到。`worker` 是进程后端的另
一端：它从 stdin 读一行请求、往 stdout 写一行响应，在*本*进程里跑那个子会话。`list`、`tree`、
`resolve` 与 `timing` 从不启动任何东西；`prompt` 只恢复一个已经存在的子会话；`interrupt` 先
核验是谁在请求，然后写一个标记文件，由正在跑那个子会话的 `Runner` 遵守；`worker` 通常由
harness spawn，不是由人敲出来的。

### `hearth subagent list --session <id> [--json] [--data-dir <dir>]`

`--session <id>` 是必需的：一个子会话属于一份父日志，而列举“任何地方的所有子会话”是另一个
（也是没有作用域的）问题。日志是 `<dataDir>/sessions/<id>.jsonl`。

文本输出是一张表，每个子会话一行：

```text
name	child	session	mode	label
agent-1	20261120-143012-ab12cd34	—	one-shot	—
```

列是 `name`（持久的兄弟名）、`child`（子会话 id，而对一个 `continuable` 子会话，`session`
一格重复它）、`mode`（`one-shot` 或 `continuable`）以及 `label`。

`--json` 打印一个 catalog payload 的 JSON 数组，每个
`{version, childId, mode, label?, name?}`——events-v1 §4.0.5 里的行形状。

### `hearth subagent prompt <child> --text <prompt> [--model <name>] [--data-dir <dir>]`

往一个 **continuable** 子会话追加一轮，并打印它的答案。这就是 `subagent` 工具的 `resume`
参数所执行的那次冷恢复（一个子会话完全由它的持久日志驱动，所以一个不同的进程也能继续它）。
一个 `one-shot` 子会话、一个已经写过 `session/end` 的子会话，或者一个不是子会话 id 的 id，
都会在一个事件被写出之前就失败。`--text` 是必需的。

### `hearth subagent tree <root> [--json] [--data-dir <dir>]`

按前序走遍 `<root>` 之下的每一个后代，每个节点一行缩进，每行带着子会话的持久名字、会话 id 与
模式。一个不是会话日志的根是**失败**（`no such session log`），不是一棵空树——与 `list` 做出
的是同一种区分。

一个描述不了的子会话被保留为带一条诊断的行，而不是被省略，因为一棵缺失的子树正是你要找的
症状：

```text
  agent-1  one-shot  20261120-143012-ab12cd34
  agent-2  continuable  20261120-143140-77fe01aa  [unavailable: …\sessions\77fe01aa.jsonl: no session log for this child]
  agent-3  one-shot  20261120-143200-0badf00d  [corrupt: line 2: invalid JSON: expected ident at line 1 column 2]
```

每一行是每层两个空格，然后是持久名字、模式、以及子会话 id。一个没有子会话的根打印
`no subagents in this session`。

三种诊断是 `corrupt`（一行被撕裂或解析不了）、`unsupported`（日志的描述符点名了一个本构建
不认识的版本）以及 `unavailable`（catalog 行存在，但子会话的日志不存在）。

`--json` 把同一棵树打印成嵌套对象，每一行是 catalog payload 加上 `path`、`depth`、
`children`，以及一个可选的 `diagnostic`（`{kind, detail}`）。

### `hearth subagent resolve <root> <name> [--json] [--data-dir <dir>]`

把一条相对于 lead 的名字路径（例如 `agent-1/agent-2`，可以带 `lead/` 前缀）解析成它所命名的
会话 id，只打印那个 id。这次查找走的是路径上每个会话的 catalog 行，所以它不需要加载任何子会话
就能工作。一个未知的段落以那个解析不了的名字失败。

### `hearth subagent timing <child> [--json] [--data-dir <dir>]`

从子会话自己的日志投影出它的耗时：`settledMs`（已关闭的 `turn/end - turn/start` 区间之和）、
`active`（此刻有一个轮次是打开的，在 `session/end` 之后永远是 false）、`lastTurnCompleted`，
以及 `turns`。

```text
20261120-143012-ab12cd34 idle, 2 turn(s), settled 4210 ms, last closed at 1763643012000
```

`hearth subagent timing --session <parent>` 则改为为那个父会话的每个子会话打印一行。这里没有
任何东西写：这个投影是从循环早就追加过的行派生出来的。

### `hearth subagent interrupt <child> [--as <id|user>] [--data-dir <dir>]`

请一个**正在跑**的子会话停下。`Runner::interrupt` 通过触发取消 token 来取消同一进程里跑着的
子会话；一次 `hearth subagent interrupt` 调用是另一个进程，看不到那个 token，所以它改为在
`<dataDir>/sessions/<child>.cancel` 放一个标记文件。一个正在跑那个子会话的 `Runner` 轮询这个
标记并触发自己的 token——与进程内中断走同一条路径——然后删掉标记，所以它取消不了同一个子会话
后来的一次运行。命令打印标记路径并退出 0。它触达不到一个跑在 `worker` 进程里的子会话（那个
进程是短命的）；它是给长命的本地 `Runner` 用的，比如 `hearth chat` 一轮所驱动的那个。

`--as` 说明是谁在请求。默认是 `user`——敲这条命令的人是操作者，他可以停掉任何东西。
`--as <session-id>` 以那个会话的身份请求，于是要受树的规则约束：一个会话不得中断它自己、一个
会话不得中断一个祖先、一个会话不得中断另一棵树里的子会话。每一次拒绝都点出双方。一个自己的链
读不出来的调用者被拒绝，而不是被信任。检查发生在标记被写出**之前**，所以一个被拒的请求不留
下任何传输物。

### `hearth subagent providers`

列出 `config.json` 里的 `subagent.providers`——一个子会话可以被送去的有名后端——外加
`subagent.backend` 默认选哪一个，以及每个 `program` 在 `PATH` 上能不能解析到。最后一列才是
重点：一个没安装的程序，看起来和安装了的一模一样，直到第一次委派失败。

```text
name	program	found	default
sandboxed	hearth	yes	yes
default backend: sandboxed
```

没有配置任何 provider 时，它打印一行说每个子会话都在本进程里跑，并退出 0：一份空列表是一种
配置，不是错误。什么都不会被启动，所以这条命令随时都可以安全地跑。

### `hearth subagent worker`

进程后端的 worker 那一半。它从 stdin 读一行 JSON 请求：

```text
{"v":1,"op":"run","prompt":"…","system":"…","model":"…","workspace":"…","schema":{…},"mode":"one-shot"}
```

并往 stdout 写恰好一行 JSON 响应：

```text
{"v":1,"ok":true,"sessionId":"…","text":"…","structured":{…},"turns":2,"steps":3}
```

或者在失败时 `{"v":1,"ok":false,"error":"…","code":"…"}`。一个本构建不讲的协议版本被拒绝，
从不被强行解释。worker 总是写一行响应——一次失败是一行 `ok:false`，不是干脆的退出——所以
调用者永远不会把一个崩溃误当成一个卡住的子会话。它只跑一次性子会话：一个必须由*后来的*进程
恢复的 `continuable` 子会话走本地路径，因为这一行被写出的那一刻，这个 worker 的进程就没了。

### 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 列举或子会话的答案被打印了（worker：一行响应被写出了）。 |
| `1` | `--session` 指向不了任何会话日志、日志读不出来、子会话恢复不了，或者没有 provider 能被构建出来。 |
| `2` | 用法错误：未知命令或 flag、`list` 没有 `--session`、`prompt` 没有子会话 id 或 `--text`、`interrupt` 没有子会话 id，或者一个格式错误的 worker 请求。 |

### 示例

```text
$ hearth subagent list --session 20261120-142900-9f3c1a2b
name     child                       session   mode       label
agent-1  20261120-143012-ab12cd34    —         one-shot   —
$ hearth subagent list --session nope
hearth subagent: no such session log: …\sessions\nope.jsonl
$ echo $?
1
```

