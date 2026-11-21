# ADR-0005 —— 终端是一条被记录的命令通路，不是一个 shell 复用器

- **状态：** 2026-11-21 由用户确认（"好，你做吧"），**已接受**。实现已交付：`crates/cli/src/terminal.rs` 起进程、装沙箱、执行取消，`crates/server/src/server.rs` 管 socket、帧与两条事件。本 ADR 记录它最终长成的样子——起草时有三处与实现不同（不逐命令审批、帧与事件的载荷、尾巴大小），已在 §3、§4、§5 按实现改写。
- **决策人：** 用户。
- **不取代** [09-platform-tier.md](09-platform-tier.md) §6 里"浏览器内终端复用器"这条非目标——它仍然成立，本 ADR 说明为什么它是非目标，以及取代它的那条东西是什么。

## 1. 改了什么，什么仍成立

**仍然成立：** 浏览器里没有 shell 复用器。没有全屏 TUI，没有持久 shell 会话，没有能跑 `vim` 的窗口。

**新增：** IDE 里多一个**行式命令通路**——一次一条命令。服务端先写 `terminal/run`，写不进日志就不跑；然后起进程，stdout 与 stderr 按 UTF-8 字符边界切块实时回传；结束时写 `terminal/end`，带退出码、耗时与两条流各自的尾巴。

用户的原始要求（交接文档里那句"交互式终端需要 ConPTY 与双向流，而一条能跑任意命令的 UI 通路必须先经过策略与审批，否则就是一个不写日志的后门"）有一半被推翻了。这个 ADR 是修正后的回答：**双向流给，任意命令不给，但不逐命令审批**——"不写日志的后门"那条担心用两条事件解掉，而不是用审批（§5）。

## 2. 为什么不是 PTY

PTY 只能记录**一个**动作：开终端。开完之后，每条命令都在那个 shell 里，不落事件，不记退出码，也说不清哪一段输出属于哪条命令，更没法给单条命令设边界（哪一条被取消了、哪一条退出得不对）。于是日志会声称"这一轮没有任何命令被执行"，而磁盘、网络、进程树都已经被动过——这正是 [11-adr-agent-hooks.md](11-adr-agent-hooks.md) §4 那条原则的反面：那里是"没有*不记日志的*重写器"，这里是"没有*不记日志的*执行"。

ConPTY 买得到的是 `vim`、`htop`、进度条原地刷新。这些是**人的**工作台，不是 agent 的工作台；而人已经有自己的工作台了（真的终端、真的 VS Code）。这个产物要买的是"不用切窗口就能验证 agent 干的事"，那条路径上一条 `cargo test` 的退出码比一个能跑 `vim` 的窗口值钱。

## 3. 形态

- 传输：`GET /api/sessions/{id}/terminal`，WebSocket 升级，子协议 `hearth.v1`。它与事件流共用同一个握手函数 `ws_upgrade`——升级头、Origin 检查、`Sec-WebSocket-Accept`、子协议回声都在那里，所以两条 socket 的鉴权规则不会走散。
- 客户端 → 服务端，两种帧：`{"kind":"run","command":"…","cwd":"…"}`、`{"kind":"cancel"}`。判别字段是 `kind`。
- 服务端 → 客户端，四种帧，都带 `protocol: 1`：`start{command,cwd}`、`output{stream:"stdout"|"stderr",text}`、`exit{exitCode,durationMs,cancelled,stdoutTail,stderrTail}`、`denied{reason}`。一条 `output` 帧只装一条流，stdout 与 stderr 各有各的帧。
- `start` 在进程启动之前发出：客户端没收到 `start`，就说明什么都没跑。`exitCode` 为 `-1` 表示进程没跑起来（被拒或起不来），原因在 `terminal/end` 的 `message` 里——`exit` 帧不带 `message`，它只报数字。
- 危险命令名单与 `shell` 工具共用一份（`hearth_tool::builtin::dangerous`）：命中的行不跑，`exitCode` 为 `-1`，原因进 `terminal/end` 的 `message`。人可以在自己的终端里跑它们，这个面不能带着工作区的名分跑。
- cwd 是工作区相对路径，省略或空串是工作区根，每次 `run` 带上；服务端用 `workspace_route::resolve` 校验（拒 `..` 与符号链接逃逸），不存在则 `denied`。`cd` 是面板的内建动作，不是 shell 的 cwd。
- 取消：`cancel` 翻转该次运行的取消令牌，走 `crates/exec` 已有的杀进程树路径（每 10 ms 轮询一次，与 exec 自己的等待循环同频）。取消**不写第三条事件**，`terminal/end` 的 `cancelled: true` 就是它的记录。socket 一起关掉时，正在跑的那条同样被取消——不留看不见的进程。
- 同一 socket 同一时刻只跑一条：第二条 `run` 得到 `denied`（"a command is already running"），不排队。被拒的帧（命令为空、cwd 不存在、已有一条在跑、日志写不进去）只回 `denied`，不写任何事件。
- 没有超时：一个 build 要多久就多久，只有 `cancel` 或 socket 关闭能让它提前结束。

## 4. 事件

两个新类型，五处齐改（`EventType::ALL`、契约目录表与 §4、conformance 用例、契约测试计数、`docs/README.md` 语料计数）：

| 类型 | 何时 | 载荷 |
|---|---|---|
| `terminal/run` | 一条命令被接受、进程启动之前 | `command`、`cwd` |
| `terminal/end` | 该命令结束（含被取消、进程起不来） | `command`、`cwd`、`exitCode`、`durationMs`、`cancelled`、`stdoutTail`、`stderrTail`，可选 `message` |

两条都由**服务端**写，不由 runner 写：runner 只负责起进程与取消，日志归管 socket 的那一层。socket 先断也照写 `terminal/end`——否则日志里会留下一条没有结尾的命令。人发起的，所以**不是** `tool/call`，理由与 `file/write` 相同：没有任何模型参与。

输出正文不落事件（它可能很大且大多无用），只落两条流各自的末 4 KiB（`TAIL_BYTES = 4 << 10`；起草时写的是 8 KiB，实现砍到 4 KiB，因为两条尾巴同乘一条 append-only 的事件）。实时回传另有预算：每流 1 MiB，之后截断并留一条说明（`TERMINAL_STREAM_BUDGET`），完整输出留在面板里。

被拒的帧不写任何事件：一条没跑起来的命令若留下 `terminal/run`，一次误点的拒斥就会在日志里看起来像跑过。这与 §3 的"写不进日志就不跑"是同一条规则的两端——能跑的必定有记录，没跑过的必定没有。

## 5. 为什么不逐命令审批

起草时写的是"每条命令都要过 `Policy` 与 `Approver`"。实现改了这一点，理由三条：

1. **与 `file/write` 同一 doctrine。** 人发起的动作不是模型调用工具。审批提示约束的是**模型无人看管时能做什么**，而键盘前的那个人正是审批要保护的对象——让一个人批准自己的按键是装样子。
2. **它不增加安全性，只增加摩擦。** 终端与 agent 的 `shell` 工具同权：`full-access` 下 shell 连问都不问。一个人既然已经能用面板让模型跑同一条命令，再为他亲手敲的同一条命令弹一次窗，买到的是被训练出来的无脑"允许"——那会削弱审批本来要保护的那条通路。
3. **取而代之的是三条真约束。**（i）read-only 会话根本不提供终端：`new_terminal` 为 `None`，升级前就 403。（ii）每条命令必写 `terminal/run` 与 `terminal/end` 两条事件，写不进日志就不跑。（iii）沙箱 enforcer 逐命令装在子进程上——这一点比 `shell` 工具更严，不是更松：`hearth-server` 的 `--sandbox` 过去只约束策略层提供哪些工具，现在真的落在每条终端命令的子进程上。

所以答案是：**不审批，但每条命令都在日志里，而且不比 shell 工具更能干。**

## 6. 这个不买到什么

- **不是全屏 TUI。** `vim`、`htop`、`less`、以及任何靠光标定位重绘的程序都不工作。服务端只把控制台代码页转成 UTF-8（见下一条），不解析 ANSI 颜色，也不解释光标控制序列：`output` 里就是进程写出来的那些字符（控制序列会原样出现），解释它们不是这一层的事。
- **不是持久 shell。** 每条命令是独立进程：`export`、`alias`、`cd`（shell 的那个）、后台作业都不跨命令存活。面板的 cwd 是 UI 状态，不是 shell 状态。
- **不是交互式 stdin。** `sudo` 密码提示、`ssh` 会话、`read` 提示都拿不到输入——子进程的 stdin 是 EOF。
- **不是 PTY。** 没有窗口尺寸，没有 `SIGWINCH`，没有作业控制；进程树随 `cancel` 或 socket 关闭被整棵杀掉。
- **只转控制台代码页，不是任意编码的转换器。** 帧里的 `text` 永远是 UTF-8，但子进程写出的字节先在 **runner** 这一层转过来（`crates/cli/src/terminal.rs` 的 `Decoder`）：Windows 上取 `GetConsoleOutputCP`、`GetACP`、`GetOEMCP` 这组页当候选（双字节页优先、去掉 0 与 65001），逐页试，第一个解不出 U+FFFD 的赢。所以中文控制台的 GBK（`ping`、`ipconfig` 那类）现在直接读出中文，`output` 与两条尾巴用的是同一套解码。
  放 runner 而不是放前端，是因为**只有 runner 知道自己在哪个控制台上起了子进程**；协议与前端因此一行没改，也不必知道 936 是什么。起草时写的是"让帧改传字节、前端按代码页解码"，实现没走那条路。
  仍然做不到的：候选页里两个都能"干净"解码时（单字节页对任何字节都有个字母，无法自证），由候选顺序裁定——顺序来自控制台，不是猜出来的；真正的二进制输出照样会变成替换字符，因为这一层不猜"这不是文本"。
- **不是沙箱的替代品。** 命令照样过 `crates/exec` 的沙箱钩子，enforcer 逐条装在子进程上；但它**没有超时**——一个 build 要多久就多久，面板不是"人在前面所以不用沙箱"的口子，也不是"反正有超时"的保证。
