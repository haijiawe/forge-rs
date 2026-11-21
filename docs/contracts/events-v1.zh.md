> 本文件是 [events-v1.md](events-v1.md) 的中文翻译。**英文原文是规范**：两者冲突时以英文为准，并回来修本文件。
> 事件类型名、字段名、路径一律不译，它们必须与代码和语料逐字一致。

<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# hearth 事件格式，版本 1

> **中文导读。** 这份契约管一件事：`<dataDir>/sessions/<会话 id>.jsonl` 里每一行长什么样、
> 什么算合法、什么必须拒绝。任何语言的实现只靠这份文本就该能读写本树的事件日志。
>
> 怎么读：§1–§3 讲一条日志是什么、信封长什么样（`v`/`seq`/`id`/`ts`/`session`/`turn`/`step`/`type`/`payload`）、
> 以及三条不变式——**只追加**、**`seq` 必须稠密递增**、**未知 `type` 是数据而未知 `v` 是错误**；
> §4 是类型目录（一张表定全）；后面各节逐类解释载荷。
>
> 几个术语：`seq` 是这条日志内的行号，从 1 开始不能跳号；`payload` 的形状由本契约描述，
> 但**不强制**读者校验（见 [AGENTS.md](../../AGENTS.md) 法则 2）；「稠密」指去掉任何一行都会
> 让后面的 `seq` 出现缺口。
>
> 改动时必须同步五处，少一处就会有东西对不上：
> ① `crates/core/src/event.rs`——加常量**并且**加进 `EventType::ALL`；
> ② 本文 §4 的表格行；③ `tests/conformance/cases/events/` 下一条用例；
> ④ `crates/core/tests/contract.rs`——它逐字核对本表与 `ALL` 的集合差、拼写出来的数字、
> 以及 `docs/README.md` 里的语料计数，漏改会红；
> ⑤ `docs/README.md` 与本文开头的数字。
> 这五处是 `AGENTS.md`「我要加东西」表里事件类型那一行的完整版。

状态：**对磁盘上的 v1 格式具有规范效力。** 本文的写法使得任何语言（Rust、TypeScript、Go、……）
的重实现都能在不看 Go 源码的情况下读写 `hearth` 的会话日志。本文与 Go 代码冲突时，
以代码和 `tests/conformance/` 为准，本文是 bug。

机器可读的配套物：`tests/conformance/cases/events/*.json`。本文里每一条能表述成
输入/输出的规则，那里至少有一个用例。一致性的实现就是通过每一个用例、
外加 [§11](#11 conformance suite) 里的夹具规则的那一个。

关键词 MUST、MUST NOT、SHOULD 与 MAY 按 RFC 2119 的用法使用。

   

## 1. 日志是什么

会话日志是 `<dataDir>/sessions/<sessionId>.jsonl` 这个位置上的一个文件——每个会话一个
扁平文件，不是每个会话一个目录。如果某个会话将来需要伴随文件（索引、锁、二进制块），
它们放进 `<dataDir>/sessions/<sessionId>/`，而日志本身仍然留在那个扁平的 `.jsonl` 路径上。

  编码：UTF 8，无 BOM。
  格式：**JSON Lines**——每行一个 JSON 值，每行以一个 `\n`（LF，`0x0A`）结尾。不写
  `\r\n`；读者 MAY 在解析前接受并剥掉一个尾随的 `\r`。
  每行恰好装一个**事件**（§2 的信封，加上它的载荷）。
  文件是**只追加**的（§5）。它是某个会话的唯一真相来源：任何投影、缓存或索引都不得
  被当作权威。空文件是合法日志（一个还没开始的会话）。文件缺失时，被允许创建它的
  读者把它当作空日志处理。空行（换行之间零字节，或只有空白）被读者忽略，写者 MUST NOT
  写它们。它们既不推进 `seq`，也不产生事件。

   

## 2. 信封

每个事件都是一个恰好带这些成员的 JSON 对象。未知成员也可能存在（§6）。

| 字段 | JSON 类型 | 必需 | 可为 null | 类型 / 范围 | 含义 |
|     |     |     |     |     |     |
| `v` | 数字（整数） | 是 | 否 | 必须为 `1` | 本条记录的格式版本。 |
| `seq` | 数字（整数） | 是 | 否 | `1 .. 2^64 1` | 在日志中的位置。稠密、升序、从 1 开始。 |
| `id` | 字符串 | 是 | 否 | 非空 | 事件身份。写者使用 32 个小写十六进制字符（128 个随机位）。读者 MUST 只要求非空且单条日志内唯一。 |
| `ts` | 数字（整数） | 是 | 否 | `> 0` | 事件的墙钟时间，**Unix 纪元毫秒，UTC**。 |
| `session` | 字符串 | 是 | 否 | 非空 | 本事件属于的会话 id。读者 MUST 接受任何长度不超过文件系统上限的非空字符串；今天写者铸造的两种形状是 `s_` + 26 个小写 Crockford base32 字符（`internal/server`，128 个随机位）与 `<YYYYMMDD HHMMSS> <6 lowercase hex>`（`cmd/forge`，例如 `20260101 120000 a1b2c3`）。 |
| `turn` | 数字（整数） | 否 | 否 | `1 .. 2^32 1` | 会话内的轮次号。事件不属于任何轮次时**省略**（而不是写 `0`）。 |
| `step` | 数字（整数） | 否 | 否 | `1 .. 2^32 1` | 轮次内的步号。不适用时省略；SHOULD NOT 在没有 `turn` 时出现（读者不会因此拒绝它）。 |
| `type` | 字符串 | 是 | 否 | 非空 | 事件类型，`namespace/name`（§4）。 |
| `payload` | 任意 JSON 值 | 否 | 是 | 合法 JSON | 类型相关的正文。v1 写者 SHOULD 使用对象。没有东西可携带时省略。 |

实现容易搞错的地方：

  `v`、`seq` 与 `ts` 是 **JSON 数字、整数，没有小数也没有指数**。写 `1`，绝不写 `1.0` 或
  `1e0`。`seq` 是无符号 64 位整数：JavaScript 实现必须为 `seq` 使用 `BigInt`（超过
  `2^53` 的值作为 `number` 会丢精度）。
  `turn` 与 `step` 为零时省略。不写 `"turn": 0` 与 `"step": 0`；看到它们的读者 MUST 把它
  们当作不存在（Go 的解码器把该字段留在零值，正好就是这条规则）。除轮次为零这一情形
  外，其余每个必需字段都有 `error_` 用例。
  信封里没有哪个字段可为 null。`v`、`id`、`ts`、`session` 或 `type` 里显式的 `null`
  是校验错误，而不是「不存在」：null 的 `v` 反序列化成 0，因而通不过版本检查；
  null 的 `id`/`session`/`type` 是空字符串；null 的 `ts` 是 0。把这些字段建模成
  `Option` 的读者必须拒绝 `null`，而不是把它强制转换。`payload: null` 是唯一被接受的
  null（§3.6）。
  信封里的未知成员被**忽略**，并且 MAY 在事件被重新编码时丢掉（§6）。

### 一个完整事件的示例

```json
{"v":1,"seq":7,"id":"708192a3b4c5d6e7f801122334455667","ts":1794787203302,"session":"s_01jya7c9q3r5t7v9w1x3y5z7ab","turn":1,"step":2,"type":"assistant/message","payload":{"text":"Added a retry wrapper with exponential backoff and jitter.","stopReason":"end_turn"}}
```

   

## 3. 不变式

当每条记录都满足以下全部条件时，日志是**合法的**。违反它们的读者 MUST 拒绝该日志，
并且 MUST NOT 悄悄修复它（例外：§8 的撕裂尾）。

1. **`v == 1`。** 任何其他值都是不受支持的版本（§9）。
2. **`seq` 从 1 开始**，并且**稠密且升序**：第 *n* 条记录的
   `seq == n`。没有缺口、没有重复、没有重排。`seq` 由写者在
   追加时分配，绝不复用。
3. **`id` 非空**，并且**在日志内唯一**。`id` 是随机的，所以两份
   独立的日志可能共享 id；唯一性是按日志算的。
4. **`ts > 0`。** 时间戳是纪元毫秒。它们**不**要求
   单调：时钟回拨是可能的，且不得让一次读取失败。
   定义顺序的是 `seq`，不是 `ts`。
5. **`session` 非空**，并且**在一条日志的每条记录里都相同**。一个
   文件恰好装一个会话。Go 的存储从文件路径推导会话，而不逐条复查，
   所以不一致意味着这个文件被合并过或被手工编辑过：夹具测试会拒绝它，
   读者 SHOULD 把它当作损坏，而不是当作两个交错会话。
6. **`type` 非空。** 写者 MUST 使用小写的 `namespace/name`
   （`^[a z][a z0 9 ]*(/[a z][a z0 9 ]*)+$`，可选更深，例如
   `tool/call`、`future/step/heartbeat`）。读者 MUST NOT 因为某个
   `type` 不符合该形状就拒绝这个事件——只有空的才不合法。未知
   类型是接受的（§6）。
7. **`payload` 若存在，必须是合法 JSON。** 它的形状取决于 `type`，
   v1 读者不校验它（§4）。
8. **记录是一个 JSON 对象。** 顶层数组、字符串、数字或布尔值
   都不是信封。

一致性实现必须能够报告的校验错误身份，以及一致性用例用来匹配的子串：

### 拒绝诊断

| 条件 | 稳定的诊断子串 | 今天的 Go 消息 |
|     |     |     |
| 无法解析为 JSON（撕裂行、尾随垃圾） | `invalid JSON` | `invalid JSON: record is not a JSON value` |
| 能解析，但不是带 v1 成员的对象 | `does not match the v1 envelope` | `record does not match the v1 envelope: ...` |
| `v` 存在但不是 1 | `unsupported event version` | `session: unsupported event version %d (want %d)` |
| `id` 为空/缺失/null | `empty id` | `session: event has empty id` |
| `session` 为空/缺失/null | `empty session` | `session: event has empty session` |
| `type` 为空/缺失/null | `empty type` | `session: event has empty type` |
| `ts` 缺失、为零或为负 | `empty ts` | `session: event has empty ts` |
| `seq` 有缺口 / 位置不对 | `not dense` | `record %d: event seq %d is not dense (want %d)` |
| 同一个 `id` 在一条日志里出现两次 | `duplicate event id` | `duplicate event id %q (first seen at record %d)` |

Go 的消息是信息性的，不是规范性的：实现 MUST 把这个条件暴露出来，
而上面这些子串才是一致性套件所断言的东西。

   

## 4. 事件类型

v1 的类型目录是**开放的**。今天 `hearth` 里这三十五种类型各有生产者：

| `type` | 发出时机 | 当前生产者里的载荷成员 |
|     |     |     |
| `session/start` | 会话被创建时 | `workspace`、`model`、`profile`、`startedBy`，可选的 `system`（仅当配置了系统提示词时）、当该提示词超过 8 KiB 被截断时的 `systemBytes` + `systemTruncated`；分叉会话的头部还携带 `forkedFrom`、`forkedAtSeq`、`forkedAt`（§4.1） |
| `session/end` | 一个**终结性**会话结束时（子会话有；可续的会话没有——见下文） | `reason`（`completed` \| `aborted`）、`turns` |
| `session/archived` | 操作者声明该会话不再开始新工作 | 可选的 `reason` |
| `turn/start` | 用户轮次开始时 | `number`（从 1 开始的轮次号；旧日志里没有，此时读者回退到到达顺序） |
| `turn/end` | 轮次结束时 | `reason`（`completed` \| `cancelled` \| `error`）、`steps`，可选的 `cancelledBy`（自由形式；循环写一个原因词，恢复路径写 `cli:recovery`） |
| `step/start` | 轮次内一次模型调用开始时 | `index`（轮次内从 0 开始）、`model` |
| `step/end` | 一次模型调用结束时 | `index`、`reason`（`end_turn` \| `tool_use` \| `cancelled` \| `max_tokens` \| `error`） |
| `llm/call` | 一个请求即将发出时 | `model`、`messages`（数量）、`tools`（名字），可选的 `temperature`、`maxTokens` |
| `user/message` | 用户输入被接受时（在轮次开始时，或在支持转向的界面上于轮次中途——该界面在收下输入时写这一行，而循环只会在下一个步边界把文本放到模型面前，绝不会写第二行） | `text`、`attachments`（数组） |
| `assistant/message` | 某一步的模型输出 | `text`、`stopReason`（`end_turn` \| `tool_use` \| `cancelled` \| `max_tokens` \| `error`），可选的 `partial`、`usage`（`inputTokens`/`outputTokens`/`cacheReadTokens`/`cacheWriteTokens`）、`durationMs` |
| `tool/call` | 工具即将运行时 | `callId`、`tool`、`args`（对象） |
| `tool/result` | 工具结束时 | `callId`、`tool`、`ok`（布尔）、`output`，可选的 `code`（当 `ok` 为 false 时）、`truncated`、`meta`（自由形式的工具事实：`exitCode`、`bytes`、`durationMs`、……；`read_image` 把 `imagePath`/`mediaType`/`bytes` 放在这里——§4.0.2） |
| `approval/request` | 一个工具需要同意 | `approvalId`、`callId`、`tool`、`args`、`reason`，可选的 `timeoutMs` |
| `approval/decision` | 用户答复了 | `approvalId`、`callId`、`decision`（`allow` \| `deny`）、`decidedBy`，可选的 `note` |
| `compaction/summary` | 历史被压缩时 | `summary`、`replaces`（`{from,to}` 的 seq 范围） |
| `error` | 框架让某一步失败了 | `message`，可选的 `code`、`retryable` |
| `goal/created` | 目标开始：一直干到完成或被阻塞 | `goalId`、`text`、`maxRounds`，可选的 `revision`（u64） |
| `goal/continued` | 一个续作轮由框架开启，而不是由人开启 | `goalId`、`round`，可选的 `revision`（u64） |
| `goal/ended` | 目标停止 | `goalId`、`reason`（`complete` \| `blocked` \| `cancelled` \| `exhausted`），可选的 `note`，可选的 `revision`（u64） |
| `context/compressed` | 一段 seq 范围被一个模型能叫出名字的摘要替换，仅限本窗口 | `blockId`、`from`、`to`（含端点的 seq）、`summary`，可选的 `messages`，可选的 `surfaceOp`（§4.0.3） |
| `hook/rewrite` | 一个钩子改变了 provider 即将收到的东西（§4.0.4） | `phase`（`pre step` \| `request`）、`hook`（改变了东西的钩子名字，逗号分隔）、`changed`（有几个钩子动了） |
| `subagent/catalog` | 循环记录本会话启动了一个子会话（§4.0.5） | `version`（3）、`childId`、`mode`（`one shot` \| `continuable`），可选的 `label`，可选的 `name` |
| `subagent/descriptor` | 子会话在它第一个轮次内一次性记录自己是如何被创建的（§4.0.5） | `version`（3）、`mode`、`provider`，可选的 `label`，可选的 `agentProvider` \| `agentModel` \| `agentReasoningEffort` \| `persona` \| `toolFilter` |
| `compaction/start` | 一次压缩事务开启，在摘要器运行之前（§4.0.6） | `from`、`to`（该事务打算折叠的 seq 范围） |
| `compaction/end` | 事务关闭 | `ok`（布尔），可选的 `reason`（`empty` \| `error` \| `already compacted`） |
| `compaction/summary-error` | 摘要没能产出，所以历史仍然可见（§4.0.6） | `reason`（`empty` \| `error`）、`message` |
| `request/header` | 循环为它即将发出的请求命名（§4.0.7） | `reason`（`initial` \| `series` \| `change` \| `resume`）、`model`、`messages`（数量）、`tools`（名字） |
| `request/context` | 循环记录一个请求实际携带了什么（§4.0.9） | 可选的 `provider`、`model`、`messages`（数量）、`tools`（数量）、可选的 `temperature`、可选的 `maxTokens`、可选的 `stop`、`systemBytes`、`systemFingerprint` |
| `assistant/attempt` | 一次**没有**成为可见答案的 provider 尝试（§4.0.7） | `attempt`（从 0 开始）、`outcome`（`retry` \| `error` \| `cancelled`）、`retryable`（布尔），可选的 `message`，可选的 `code` |
| `agent/inbox` | 对持久输入队列的一次改动（§4.0.8） | `target`（`next turn` \| `next step`）、`start`、`removed`、`inserted`（`[{id,text}]`） |
| `agent/status` | 一个常驻循环进入或离开某个阶段（§4.0.8） | `phase`（`running` \| `maintenance` \| `idle`） |
| `file/write` | 一个人通过编辑器保存了缓冲区（§4.0.10） | `path`（工作区相对路径）、`bytes`、`sha256` |
| `file/revert` | 一个人把文件恢复到它已提交的字节（§4.0.11） | `path`（工作区相对路径）、`bytes`、`sha256` |
| `terminal/run` | 一个人在 IDE 终端里跑了一条 shell 命令（§4.0.12） | `command`、`cwd`（工作区相对路径） |
| `terminal/end` | `terminal/run` 的那条命令结束、被取消，或者根本没拿到退出码（§4.0.12） | `command`、`cwd`、`exitCode`、`durationMs`、`cancelled`、`stdoutTail`、`stderrTail`、可选 `message` |

三个 `goal/*` 载荷上可选的 `revision` 是多写者续作用来在**写入之前**判断自己是否
仍然是推进该目标的那个写者的版本号：写者读取日志里最新的 `revision`，仅当自己的
本地副本仍然匹配时才追加 `goal/continued`（携带 `revision + 1`）——否则它就是
`Stale`，什么都不写，这样两个入场者就不可能把一个目标跑两遍。**任何省略 `revision`
的 `goal/*` 事件读作 `0`**——旧日志里没有它，读者也不得因此拒绝这些日志。
`goal/created` 写 `0`，`goal/continued` 写递增后的值，`goal/ended` 写它停下的那个
版本，而 `fold` 绝不让这个值倒退。

**每个 `approvalId` 恰好一条 `approval/decision`，由做出决定的一方写。** `approval/request`
永远由 agent 循环写——只有它知道这次同意属于哪个轮次、哪一步。决定则由产出答案的那个
界面写：CLI 审批者在循环内部答复（`  approve=always`、`  approve=prompt`），所以循环
记录它们；浏览器经 HTTP 答复，所以请求处理器在回包**之前**追加决定——同意必须在发出
200 之前就在日志里，否则点击与响应之间的一次重启就会丢掉日志存在的意义所要保存的那个
答复。自己记录决定的审批者用 `tool.DecisionRecorder` 声明这一点，循环于是只写请求。
两个写者会把一次点击变成两条记录，让审计无法说清一个人被问了几次。*超时*仍然由循环
记录成带 `note` 的 `deny`：「没人答复」是循环没能拿到答案，不是任何人的决定。

`decidedBy` 指明**谁**答复了，而不是哪个二进制写了这条记录：人在 HTTP 界面答复时是
`user`（浏览器就是用户，不是进程），CLI 审批者用它自己的 `Name()`（`flag:  approve=always`、
`prompt`），而没人答复、由循环记录这次拒绝时，是 `timeout` | `cancelled` | `error`
之一。需要来源信息（「这是人还是默认值？」）的读者读这个字段；需要代码路径的读者读
该事件的邻居。

三个「没人答复」的词被区分开，因为它们是不同的事实，而循环是唯一能区分它们的写者：

| 值 | 何时 | 操作者应当得出什么结论 |
|     |     |     |
| `timeout` | 截止时刻已过——键盘前没人 | 问题本身合理，只是人不在 |
| `cancelled` | 问题还开着时轮次被取消 | 人**撤回**了请求；没有任何东西在是非上被拒绝 |
| `error` | 审批者自己坏了（I/O 错误、管道关闭） | 一个 bug 或坏掉的界面，不是策略决定 |

这种情况下写者 MUST NOT 写出审批者的名字：一个写着 `decidedBy: "web"` 而旁边是
`note: "no answer: context canceled"` 的载荷自相矛盾，而一个分不清「人说了不」与
「人走开了」的审计，已经失去了它唯一存在的理由。

**`turn/start` 故意有不止一个生产者。** 哪个界面拥有轮次入口就由谁写它：CLI 轮次由
agent 循环写，API 轮次由 HTTP 层写（它必须在请 runner 运行之前就开启轮次，这样
`POST /api/sessions/{id}/messages` 才能同步地用提示词的 `seq` 作答）。读者不得假定
某个 `turn/start` 是哪个界面写的，也不得据此推断是 CLI 还是 API。

**`session/end` 只属于终结性会话。** 可续的会话文件是**可续的**：`hearth chat   session X`
会把轮次 2 追加到同一条日志，之后另一个进程可能追加轮次 3。所以对这些会话来说，
「会话结束了」不是框架能观察到的事实——进程退出是操作者事件，不是领域事件。在进程
脱离时写 `session/end` 会是一句下一个进程必须反驳的谎。因此读者必须把缺少
`session/end` 当作正常，也不得用它判断一个可续会话是否完成。

在轮次**内部**死掉的进程会留下一个有 `turn/start` 而没有 `turn/end` 的轮次。那不是
损坏：那是一个从未结束的轮次的诚实记录，读者自己的投影看到的是一个 `running` 轮次。
下一个恢复这条日志的进程通过**追加**一条 `reason="cancelled"`、`cancelledBy="cli:recovery"`
的 `turn/end` 来关闭它（§5 禁止那种本来很诱人的重写），然后以接下来的轮次号继续。
缺口里不发明任何东西：一个没人答复的 `step/start` 保持没人答复，因为「这一步被放弃了」
本来就是「一个开着的步」的含义。

唯一的生产者是**由 `subagent` 工具创建的子会话**：没有人能恢复它（它的 id 在工具结果里
返回，而不是由用户选择），所以它按构造就是终结性的，它写一次 `session/end`，`reason`
为 `completed` 或 `aborted`，并带上它跑过的轮次数。想要「这棵子树结束了吗」的读者可以
依赖它；想要「这段对话结束了吗」的读者仍然不能。

**`session/archived` 是一份声明，不是生命周期事件。** 它说的是操作者决定这个会话不再
开始新工作；它没说这个会话结束了、不可读了或可删了。由此有三条后果，而三条都是刻意
的：

  **至多一次，且幂等。** 唯一的写者是 `hearth session archive`，它先读日志，若记录
  已经在那里就什么都不写。第二次运行报告 `already archived`，而不是追加一条重复记录，
  这样读者永远不必决定两条记录哪条赢。
  **日志缺失不等于会话已归档。** 文件不存在时 `is_archived` 回答 `false`，因为归档
  记录就在会话自己的日志里：一个已归档的会话总是有一条。回答 `true` 会让会话 id 里的
  一个笔误看起来像一次策略决定。
  **它是在门口拒绝新工作，而不是在轮次内部。** 检查在启动子 agent 时运行，也在恢复
  一个 `continuable` 子会话时运行，沿着 `session/start` 的 `parent` 链一跳一跳地走。
  已经在跑的轮次保留它的审批和它的取消令牌：中途叫停它是 `cancel`，另一个动词，
  另一条记录。

拒绝时会指名那个已归档的会话——它可能是被问及者的某个祖先——因为那是操作者唯一能
施加动作的实体。

**``turn/end.cancelledBy`` 说的是谁停下了轮次。** 它只在 `reason` 为 `cancelled` 且
写者知道原因时被写，并且它是**自由形式**的：恢复路径写 `cli:recovery`，导入的 v0 日志
带着旧实现的词，而循环写 `user`、`parent`、`timeout`、`hook`、`disposed`、`unknown`
之一。读者 MUST 把无法识别的值当作数据（法则 2）——这个成员的职责是让「操作者按了
停止」与「属主被释放」可区分，而这个职责并不要求一份封闭的词表。

`unknown` 不是沉默。一个没有说明原因就被升起的令牌故意报告 `unknown`：轮次确实被
取消了，而拒绝说出来会让那条唯一能解释该轮次的记录少一个字段。

**`turn/end.reason` 与 `step/end.reason` 是两套不同的词表，这是刻意的。** 轮次与步
回答的是不同的问题，所以它们不共享词表：

  **`turn/end.reason`——整个轮次怎么样了：** `completed` |
  `cancelled` | `error`。一个用光了 token（`max_tokens`）或因为模型索要工具
  （`tool_use`）而结束的步，属于一个**完成了**的轮次：模型做了它能做的，
  这个轮次不算失败。写者 MUST NOT 把步级别的 reason 抄进 `turn/end`。
  **`step/end.reason`——一次模型调用为什么停下：** `end_turn` | `tool_use` |
  `cancelled` | `max_tokens` | `error`。`assistant/message.stopReason` 使用
  同一套步级别的列表外加 `error`，因为一个在吐出文本之后失败的流确实会
  合法地留下部分输出（见 `partial`）。

因此，按 `turn/end.reason == "completed"` 分支的读者，会把最后一步以 `end_turn`、
`tool_use` 或 `max_tokens` 结束的轮次算作成功；它不得在 `turn/end` 里寻找 `end_turn`。
反过来说，「模型是不是因为想要工具而停下的？」是 `step/end`（或 `stopReason`）的问题，
永远不是 `turn/end` 的问题。这个不对称是承重的：把两份列表合并，正是让成功的轮次在
期待 `completed` 的读者眼里消失的原因。

`step/start`、`step/end` 与 `llm/call` 是在 M1 加入的。加类型是一次**增量**变更：
一个从没听说过 `step/start` 的 v1 读者必须跳过它并继续读（见 §6），所以它们的到来
没有抬升格式版本。`llm/call` 记录请求的*形状*——模型、消息数、工具名、采样旋钮——
并且刻意**不**记录提示词文本：提示词已经作为构成它的 `user/message` /
`assistant/message` / `tool/result` 记录在日志里，存两遍只会让日志翻倍而毫无收益。

**载荷 schema 是描述性的，尚未被强制。** 没有 v1 读者校验载荷；加成员的写者是兼容的，
读者 MUST NOT 因为某个事件的载荷出乎意料就拒绝它。这张表记录的是写者们发出什么，
好让重实现能把它们渲染出来；只有当 schema 注册表落地时它才变成规范性的。

工具输出与子进程输出是有上限的（工具结果 64 KB，exec 1 MiB），超大的载荷文本在追加
前被截断，并记录 `truncated: true`。读者不得假定载荷很小。

### 4.0.1 `user/message` 附件

`attachments` 里的一个元素是**引用**，永远不会是正文：

| 成员 | 类型 | 何时 |
|     |     |     |
| `kind` | `file` \| `image` \| `diff` | 总是 |
| `id` | 32 个十六进制字符 | 当发送方上传了字节时；指定 `<dataDir>/blobs/` 里的一个文件 |
| `mediaType` | 字符串 | 当发送方声明了它时 |
| `name` | 字符串 | 当发送方给文件起了名时 |
| `path` | 字符串 | 对 `kind:"file"`：一个工作区相对路径 |
| `bytes` | 数字 | 总是；解码后的尺寸，`kind:"file"` 时为 `0` |

正文用 `GET /api/sessions/{id}/attachments/{id}` 取回（api.md §4.11）。它不在日志里，
而这是刻意的：日志的每一个读者都要为它的尺寸付账——投影、分叉、索引，以及每一个
`GET /events` 分页（它返回整条事件行）——日志是只追加的，所以一个错误永远删不掉，
而分叉会原样复制事件，所以一张截图要花掉它字节数的两倍。需要像素的读者必须向服务器
要，并且可以假定 `id` 不存在（一个 `kind:"file"` 附件根本没有正文）。

### 4.0.2 `tool/result` 图像

用像素作答的工具不能把它们放进 `output`：在每一种 chat completions 线路上，工具消息都是
文本。所以 `read_image` 把事实写下来，而把字节留在日志之外，正如附件所做的那样：

| `meta` 的成员 | 类型 | 何时 |
|     |     |     |
| `imagePath` | 字符串 | 工具读到的一个工作区相对路径 |
| `mediaType` | 字符串 | `image/png`、`image/jpeg`、`image/gif`、`image/webp`、`image/bmp`、`image/tiff`、`image/x icon` 之一 |
| `bytes` | 数字 | 读取那一刻的文件尺寸 |

这些字节只到达模型一次，作为循环从 `Result.parts` 构建的一条后续 `user` 消息；它们
永远不在日志里。之后的轮次会重新读这个路径所指的文件，这意味着图像在本次会话余下的
时间里都可见，代价是每次读取一条很小的元数据记录——也意味着如果文件被删除或被文本
替换，图像就干脆没了。读者必须容忍一条根本没有 `meta` 的工具消息：大多数工具没有
什么结构化的话要说。

### 4.0.3 `context/compressed` 区块

`compaction/summary` 是其中一种方式：最老的那段完整历史被替换，被替换的记录再也不被
读。`context/compressed` 是另一种形状——一段模型决定不再为之付账的范围，而它仍然能
命名、恢复并搜索它。

这个事件不增加任何信封成员，也不抬升版本，所以一个从没听说过区块的 v1 读者会继续读
这条日志，只是显示未压缩的历史：这是安全的方向。它携带的是地址，不是副本。

| 成员 | 类型 | 含义 |
|     |     |     |
| `blockId` | 字符串 | 32 个小写十六进制字符；模型交回来的名字 |
| `from` | 数字 | 该区块藏起来的第一个 `seq`，含端点，至少为 1 |
| `to` | 数字 | 该区块藏起来的最后一个 `seq`，含端点，不早于 `from` |
| `summary` | 字符串 | 模型改读的东西；1 字节到 8 KiB |
| `messages` | 数字，可选 | 制块时有多少条投影消息落在这个范围里——一个诊断量，永远不是地址 |
| `surfaceOp` | 对象，可选 | 该区块如何呈现：缺省意味着 `hide`；`{"op":"replace","startSeq":N,"endSeq":M}` 用一条摘要消息代替这段范围，而不是留下一个无声的缺口 |

`surfaceOp` 是唯一一个改变模型**看到**什么、而不是改变区块如何被描述的可选成员。它在
该字段出现之前写下的每个区块里都不存在，而缺省意味着 **hide**——历史上的行为——所以
忽略它的读者读到的日志与它一向读到的一模一样。它存在时，`op` 必须是 `"replace"`（未知
的 op 是错误，而不是无声的 `hide`：写者本来想让某些东西显现出来，悄悄丢掉它比失败更
糟），并且 `startSeq`/`endSeq` 含端点，且必须落在 `[from..to]` 内——为区块并未覆盖的
范围写摘要，是账本在恢复时答不上来的一句断言。这里仍然没有 begin/summary/commit 三段：
区块就是一个事件，所以它按构造是原子的（一个写了一半的区块不可能被观察到）。

四条不变式，写者在追加之前必须逐条检查：

1. **范围是 seq 范围。** Seq 号能挺过重放、重新分叉和换一个读者；
   消息索引这三样一样都挺不过。
2. **任意两个区块不重叠。** 重叠会把一条记录藏两次，并给
   「恢复」两个答案。相接的范围（`[1..5]`、`[6..9]`）是没问题的。
3. **区块 id 在一条日志内唯一**，并且是 32 个十六进制字符，所以它是一个
   键，永远不是路径。
4. **该范围必须已经被日志关闭。** 指向未来记录的指针不是地址；写者
   检查这一点，因为只有它知道日志已经写到了哪里。

恢复一个区块是**读取**，永远不是重写：原件仍然在日志里稍早几条记录处，所以
`decompress` 从同一个文件把它们交回来。因此日志永不缩小，区块永不编辑——一个当初
就弄错的区块仍然在那里，而停止为它付账的办法是恢复它。

### 4.0.4 `hook/rewrite` 记录

钩子是一个扩展点，它可能改变 provider 收到什么，而不改变它所依据的那条日志。存在两个
改写阶段：`pre step`（即将发出的消息列表）与 `request`（完全构建好的请求，在工具挂上去
之后）。钩子返回「未改变」或一个改写后的值；循环按安装顺序解析钩子，后一个钩子看到前
面的改动。

第三个阶段 `request error` **不**改写，也**不**记录在这里：在一次请求尝试失败、而传输
策略将要放弃之后，钩子只能回答「重试还是不重试」（`HookOutcome::Retry`）。它不改变任何
持久的东西，所以没有给它的 `hook/rewrite` 记录——它促成的重试会作为下一次 `llm/call`
出现。

`hook/rewrite` 是审计记录。每一个确实改变了东西的阶段恰好写一行，指名改变了它的钩子：

| 成员 | 类型 | 含义 |
|     |     |     |
| `phase` | 字符串 | `pre step` 或 `request`。 |
| `hook` | 字符串 | 改变了东西的钩子名字，逗号分隔。 |
| `changed` | 整数 | 有几个钩子动了。 |

惰性的情况是沉默的：某一步里没有任何钩子改变任何东西，就不写 `hook/rewrite`。正是这一点
让读者可以说「日志里没有改写」并且是字面意思——循环不可能在不留一行的情况下改写 provider
看到的东西。这条记录**不**携带改写前/后的文本。记录差异会把钩子可能已经抹掉的字节原样
放回日志，从而使钩子失效。这一行断言的是改写*发生过*、*是谁*做的；内容只能通过在同一
前缀上重跑同一批钩子来复原，这与 `context/compressed` 所站的是同一块地基。

钩子不得改变模型 id（循环在调用之后会把它恢复），也不得自己追加事件——循环是唯一的写者。
失败的钩子不会被吞掉：该步以 `error` 和 `code: hook_failed` 结束，所以一个坏掉的扩展是
响亮的，而不是无声缺席。

### 4.0.5 `subagent/catalog` 与 `subagent/descriptor` 记录

子 agent 是一个带持久身份的子会话，所以之后的轮次在重启之后还能再次指名它。两行记录
这个身份，各自恰好一个写者：

  `subagent/catalog` 由循环写**在父日志上**，它启动的每个子会话一次，写于子会话的
  工具结果回来时。`subagent` 工具从不写它——它通过 `Result.meta["subagentCatalog"]`
  把载荷交给循环，这与 `compress` 为 `contextBlock` 所用的通道相同，因为循环是父日志
  唯一的写者。
  `subagent/descriptor` 由子会话自己的 runner 写**在子会话自己的日志上**，一次，在它
  第一个轮次内，紧跟 `session/start` 之后。它只在日志里：它从不进入模型的历史，并且
  能挺过压缩。

| 成员 | 类型 | 含义 |
|     |     |     |
| `version` | 整数 | `3`。不认识这个版本的读者折叠成「没有 descriptor」，而不是报错。 |
| `childId` | 字符串 | （仅 `catalog`）子会话 id。 |
| `mode` | 字符串 | `one shot` 或 `continuable`。 |
| `provider` | 字符串 | （仅 `descriptor`）跑这个子会话的后端。 |
| `label` | 字符串 | 可选，面向人。 |
| `name` | 字符串 | （仅 `catalog`）子会话持久的同级名字，例如 `agent 2`；见下文。 |
| `agentProvider` / `agentModel` / `agentReasoningEffort` / `persona` / `toolFilter` | 可选 | （仅 `descriptor`，仅 `continuable`）子会话被创建时的构成。 |

**名字。** 一个子会话的名字在它的同级之间唯一，匹配 `^[a z][a z0 9 ]{0,31}$`，且永远
不是 `lead`（根的名字）。调用方不要求名字时，runner 挑第一个空闲的 `agent N`。名字住
在 catalog 行里；工具也在 `Result.meta["name"]` 里把它回显给模型看，但日志行才是真相
来源。

**折叠。** 一份 catalog 就是它那些行的有序列表（坏行被跳过，不是致命的——未知类型是
数据）。descriptor 的折叠读日志上**第一个** `subagent/descriptor`，忽略后面的；它不
认识的版本折叠成 `None` 而不是抛错，这样一个旧读者遇到未来的 descriptor 时会降级，
而不是拒绝这条日志。

**一次性 vs 可续。** `one shot` 子会话是终结性的：它跑完时写 `session/end`，之后任何
东西都不得再向它追加。`continuable` 子会话在静息时永不写 `session/end`——它最后的
`turn/end` 就是静息状态——所以之后的一次 `Runner::resume(childId, prompt, …)` 可以把
另一个轮次追加到同一条日志，完全由持久日志驱动，而不是由任何内存里的激活状态驱动。
恢复一个一次性子会话、一个已经写过 `session/end` 的子会话，或一个形状不像子会话 id 的
id，都会在写出第一个事件之前失败。这就是让子会话挺过重启的东西：承载它的是日志，不是
活着的进程。

### 4.0.6 `compaction/start`、`compaction/end` 与 `compaction/summary-error`

一次压缩不是一次追加：摘要器在「决定范围」与「写下摘要」之间运行，而进程可能死在这个
缝里。三行增量记录把它在同一条只追加日志上变成一个可检查的事务：

  `compaction/start{from,to}` 在向摘要器要文本**之前**开启事务。它指名的 seq 范围就是
  计划的那些范围。
  `compaction/summary`（已有的那条记录）在摘要产出时把它落下来。
  `compaction/end{ok,reason?}` 关闭事务。`ok:true` 表示摘要已落地；`ok:false` 记录一轮
  什么都没折叠（`reason` 是 `empty`、`error`，或并发压缩器抢走了这段范围时的
  `already compacted`）。
  `compaction/summary-error{reason,message}` 记录一次没能产出的摘要（provider 错误，
  或空输出）。它什么都不替换。

**一次失败的压缩什么都不藏。** 摘要器报错或只返回空白时，写者记录 `summary error` +
`end{ok:false}`，并且不追加 `compaction/summary`；被覆盖的事件对模型仍然可见。这是
「多给模型一点」属于安全方向的唯一一处：否则一份空摘要会成为那些事件说了什么的永久
记录。

**孤儿是可发现的，不是无声的。** 一个后面没有 `compaction/end` 的 `compaction/start`
是一次死在半路的压缩；`open_compaction()` 报告它的 seq。相信一个写了一半的范围的读者
会把部分摘要当成全部故事，所以这个括号存在，恰恰就是为了让那个状态可见。

### 4.0.7 `request/header` 与 `assistant/attempt`

一个轮次的 provider 流量有两种读者需要、而仅有答案并不携带的形状：请求*为什么*存在，
以及*路上哪些尝试失败了*。两行增量记录把它们记下来。

  `request/header{reason,model,messages,tools}` 对循环构建的每个请求写一次，在
  `llm/call` **之前**。`reason` 对一个轮次的第一个请求是 `initial`，当循环接管一个
  HTTP 层已经记录过的轮次时是 `resume`，当工具集与上一个请求不同时是 `change`，
  对同一次运行之后每一步是 `series`。消息正文刻意缺席：它们已经在更早的行上了，
  复制它们会让这一行和请求本身一样大。
  `assistant/attempt{attempt,outcome,retryable,message?,code?}` 对每一次**没有**成为
  可见答案的 provider 尝试写一次。`outcome` 是 `retry`（循环会再试）、`error`（不会）
  或 `cancelled`。`attempt` 在步内从 0 开始。

**成功的尝试不写 `assistant/attempt` 行。** 它就是该步已经记录的那唯一一条
`assistant/message`，所以只折叠 `assistant/message` 的读者看到的，正是这些行出现之前
它看到的答案——这两个类型是诊断，不是历史。这也是为什么它们都从不被回喂给 provider。

在 `assistant/attempt` 出现之前，一个失败两次、第三次成功的轮次看起来和一次干净调用
一模一样。那正是「provider 当时没事」与「provider 第三次才没事」之间的差别，而它本来
无法从日志中恢复。

### 4.0.9 `request/context`

`request/header` 回答请求*为什么*存在。`request/context` 回答*它携带了什么*，这是当
两次对同一条日志的运行对某个答案意见不一致时你要问的问题。

  `request/context{provider?,model,messages,tools,temperature?,maxTokens?,stop?,systemBytes,systemFingerprint}`
  对每个请求写一次，紧跟 `request/header` 之后，并且在 `llm/call` 之前再写一次。

每个成员都是一个标量或一份摘要：

| 成员 | 含义 |
|     |     |
| `provider` | 请求去往的那个权威（`api.example.test:8443`）。调用方没有指名时缺席——未指名的端点被省略，永不猜测。 |
| `model` | 请求指名的模型 id。 |
| `messages` | 发出了多少条消息。 |
| `tools` | 发出了多少份工具规格（一个计数，不是名字：`request/header` 已经携带了名字）。 |
| `temperature`、`maxTokens`、`stop` | 仅当确实配置了时存在。循环施加的默认值不作为调用方的选择报告。 |
| `systemBytes` | 系统提示词的字节长度。 |
| `systemFingerprint` | 16 个小写十六进制字符：系统提示词的 FNV 1a 64。 |

**系统提示词本身永不在这里重复。** 它已经在 `session/start` 上了；逐步复制它会让这一行
成为日志里最大的一行，而新增信息最少。指纹是变更检测器，不是安全边界：能够选择提示词
的对手本来就在写日志。它的定义是冻结的（`hearth_core::digest::fingerprint`），因为这些
值会被写下来、之后被比较——一份随编译器升级而漂移的摘要会让每一份存下来的指纹变成谎话。

两行合起来，就是「答案变了」与「请求变了」之间的差别，而这正是一次两次运行之间的比较
所需要的东西。

### 4.1 分叉出的日志

一次分叉（`hearth chat   fork`、`hearth session fork`）创建一个**新**会话，它的日志是
另一个会话日志某一段前缀的拷贝。分叉出的日志是一条普通的 v1 日志：它满足 §3 的每一条
不变式，它的 `seq` 从 `1` 起稠密延伸，每条记录都带着子会话的 session id。读者不需要
任何分叉意识就能投影它——子会话的轮次就是被复制过来的那些，而下一个轮次比最后一个被
复制的轮次大一。

复制是**逐字节原样**的，每条被复制的记录恰好有两处信封改动：

1. `session` 变成子会话的 id；
2. `seq` 从 `1` 开始，按原始顺序重新编号。

`v`、`id`、`ts`、`turn`、`step`、`type` 与 `payload` 原样复制，所以读者仍能把一条
被复制的记录与父日志里的那条关联起来，而检查单条日志内唯一性的读者（§9）也仍然诚实。

**头部**记录是父会话自己的 `session/start`，以同样方式复制，并加上三个载荷成员：

| 成员 | 类型 | 含义 |
|     |     |     |
| `forkedFrom` | 字符串 | 父会话的 session id（只是直接父会话：分叉的分叉指名它自己的父会话）。 |
| `forkedAtSeq` | 整数 | 父会话被复制的最高 `seq`，也就是切点。 |
| `forkedAt` | 整数 | 拷贝发生的时刻，unix 毫秒（所有跨包的时间都是毫秒）。 |

复制父会话的 `session/start` 而不是合成一份新的，是刻意的：那条记录已经携带了被复制
的那些轮次当初所处的 workspace、model、profile、`startedBy` 与系统提示词这些事实，而
一份合成的头部会悄悄丢掉它们——以及未来生产者添加的每一个成员。往载荷里加成员是兼容
变更（§6），所以头部对一个从没听说过 `forkedFrom` 的 v1 读者仍然可读。

如果父会话**没有** `session/start`（手工写的或遗留的日志），分叉就自己前插一个头部：
一条携带 `forkedFrom`、`forkedAtSeq` 与 `forkedAt` 的 `session/start`，外加从被复制
前缀里折叠出来的 `workspace`、`model` 与 `profile`，这样子会话就不会声称自己是在一个
未知目录里被创建的。它绝不发明父会话没有的事实：折叠不出来的成员就留空。

**切点**默认是父会话里最后一个 `turn/end`，所以分叉永不从轮次中途开始，也永不继承一个
没人会关闭的轮次。显式的切点必须指名一条存在的父记录，并且不得落在一个开着的轮次内部。
整个操作期间父日志以只读方式打开：它的字节不变，父日志里的撕裂尾既不被读作事件，也不
被截断。

   

### 4.0.8 `agent/inbox` 与 `agent/status`

两行记录让一个会话携带尚未被答复的工作，也让读者看见一个处于轮次之间的循环。

`agent/inbox{target,start,removed,inserted}` 是对持久输入队列的一次改动。载荷是一次
**拼接**：`start` 与 `removed` 指认从既有队列里取出的切片，`inserted` 是替换它的东西。
一种形状覆盖每一种操作，所以读者永远不必知道写者心里想的是哪个动词：

| 操作 | 行 |
|     |     |
| 追加 | `{target, start: len, removed: 0, inserted: [{id, text}]}` |
| 认领队头 | `{target, start: 0, removed: 1, inserted: []}` |
| 清空 | `{target, start: 0, removed: len, inserted: []}` |

`target` 是 `next turn`（作为新轮次的提示词投递）或 `next step`（在 agent 下一次模型
调用之前投递）。两个队列都是从这些行里折叠出来的，而这正是要点：一个进程内的双端队列
会是真相的第二份拷贝，而被接受的消息会随进程一起死掉。信封携带 `turn = 0` 与 `step = 0`，
因为一条排队中的消息还不属于任何轮次。

一次与它所指名的队列对不上的拼接（`start` 越过末尾，或 `removed` 伸到末尾之外）会被
**跳过，而不是夹紧**。夹紧会悄悄删掉一条写者本不想删的消息，而一个会丢提示词的读者比
一个错过更新的读者更糟。未知的 `target` 出于同样的理由被跳过：把消息放进另一个队列会
改变模型何时看到它，这不是读者有权做的决定。

`agent/status{phase}` 记录一个常驻循环进入或离开某个阶段：`running`、`maintenance` 或
`idle`。它**只**为维护窗口写，不为普通轮次写。一个轮次已经把自己的一切都说清楚了；
每轮多写两行，会在每条日志里留下一个对任何读者都没有好处的诊断。维护窗口是相反的情形：
没有这些行，两个轮次之间一段无法解释的空档，与一个慢 provider 无法区分。

后台子会话通过同一批队列汇报：它落定时，它的通知作为一条 `next turn` 消息被追加，所以
一个在「委派」与「落定」之间被重启过的父会话仍然能找到答案。通知是散文，不是工具结果，
这就是为什么一次后台委派不能同时要求 schema。

两行都只在日志里。它们都不进入模型历史：队列决定一条消息*何时*到达模型，而消息本身
仍然是一条 `user/message` 行。

### 4.0.10 `file/write`

`file/write{path, bytes, sha256}` 记录**一个人**通过编辑器保存了一个缓冲区。`path` 是
工作区相对的，永远不是绝对的；`bytes` 是写进去的东西的尺寸，包括零；`sha256` 是写入
字节的摘要，小写十六进制，好让之后的读者能问这个文件是否仍然匹配当初保存的东西。

它不是 `tool/call`，而这个差别正是这个类型存在的理由。工具通道是一段对话：`tool/call`
是循环请一个工具做的事，`tool/result` 是它给模型的答案。人保存一个文件时，循环里没有
模型，而把它记成一次工具调用会让这份记录声称 agent 写了一个它从未碰过的文件——读者
无法把那种情况和 agent 真的写了它区分开。

这一行买到的是解释分歧的能力。日志说 agent 读了 `main.rs` 并看到了 X；磁盘说是 Y；没有
这一行，唯一可用的假设是 bug。有了它，读者就能说出文件是什么时候变的、被谁的手变的。
这正是法则 1 应用到工作区上的样子：重启之后，文件是唯一幸存的证据，而没有时间戳的证据
只是传言。

两条值得说明的后果：

  投影**忽略**它，模型历史永不看见它。它是关于工作区的事实，不是关于对话的；把它放到
  模型面前，会在没有任何工具结果依赖的东西上花掉 token。
  框架拒绝的一次写入（只读模式、工作区之外的路径、过期的 `expectedSha256`）**什么都不
  追加**，因为没有文件改变。一行描述一次并未发生的写入的记录，比缺一行更糟。

### 4.0.11 `file/revert`

`file/revert{path, bytes, sha256}` 记录**一个人**把一个文件恢复成 git 为它提交过的字节，
丢弃了一处工作区改动。载荷有三个与 §4.0.10 相同的成员，`sha256` 同样是此刻磁盘上字节
的摘要，所以读者在事后仍能把这一行与文件对上。

它与 `file/write` 是两个不同的类型，理由和 `file/write` 与 `tool/call` 不同的理由一样：
两行描述的是不同的行为，即使二者都在搬字节。`file/write` 说一个人写了这个缓冲区；
`file/revert` 说一个人把一处编辑扔掉了，要了已提交的版本。问「谁最后改了这个文件，
他是有意的吗」的读者，无法从一份把两种行为写成一个样子的日志里得到答案。

三条后果：

  投影**忽略**它，模型历史永不看见它，与 §4.0.10 完全一样。它是关于工作区的事实，
  不是关于对话的。
  恢复只对带已提交版本的文件成功。未跟踪的文件被拒绝并且**不被删除**，不存在的仓库被
  拒绝，而一次拒绝**什么都不**追加——恢复到一个不存在的版本，是给删除起了个让人安心
  的名字。
  它同时恢复索引与工作树，所以已暂存的编辑也会被丢弃。只做一半会让 diff 列表与磁盘
  互相矛盾。

### 4.0.12 `terminal/run` 与 `terminal/end`

一个人在 IDE 终端里敲字，那个人是人，不是模型，所以这两类不是 `tool/call` 与
`tool/result`——和 §4.0.10 给 `file/write` 的理由是同一条。一条被接受的命令写一条
`terminal/run`，一条真正开始的命令写一条 `terminal/end`。在跑起来之前就被拒的
命令——空行、跑出工作区的目录、前一条还在跑时的第二条、记不进日志的那条——两行都
不写：一条声称「跑了什么」的记录，不能描述一件没发生的事。

`terminal/run{command, cwd}` 写在命令**运行之前**，所以一个在命令中途死掉的进程，
已经说过它在跑什么；写不进这一行的命令会被拒绝，而不是照跑——一条不进日志的进程，
是这个面唯一不能做的事。`command` 是人敲的那行原文（去掉首尾空白），
`cwd` 是它运行所在的目录、相对工作区，工作区根部写作 `.`。

`terminal/end{command, cwd, exitCode, durationMs, cancelled, stdoutTail, stderrTail, message?}`
收尾，而且 socket 已经断了也照写——一条没有结尾的命令就是日志上的一个洞。
`command` 与 `cwd` 重复一遍，是为了这一行自己站得住：按序号去 join 的读者是
例外，不是常态。`exitCode` 在没拿到状态时是 `-1`——起不来，或者被杀掉却没产出
状态——这时 `message` 说明原因，所以读者不能把 `-1` 当作一次普通退出。
`cancelled` 为人按下停止、或关掉面板导致进程树被杀时为真。`stdoutTail` 与
`stderrTail` 是两条流各自的末 **4 KiB**，分开存是因为把它们交错拼回一个字符串就丢了
「是谁说的」。它们的存在是为了让人在重启之后还能回答「它到底说了什么」，并且是
刻意限长的：每个输出块写一行会把一次构建变成日志，而完整输出属于终端面板——
那是一个视图，不是真相的来源。

这两行就是这里允许存在一个终端的原因。一条能跑任意命令、又不先把它们写下来的
通路，会是这个项目唯一那份真相上的一个洞——见 `docs/14-adr-terminal.md`。

### 4.0.13 `tool/result` 的行改动

会写文件的工具——`fs_write`、`fs_edit`——在 `meta` 里说出它碰过哪几行。这是
一次工具调用的事实，不是工作区对 git 的 diff，也不能替代 `file/write`：想知道
一个文件现在长什么样，去读那个文件。

| `meta` 成员 | 类型 | 何时出现 |
|     |     |     |
| `path` | string | 这个工具写的工作区相对路径 |
| `lines` | object | 仅当新文本**与**被它替换的旧文本都读得出来、能比较时 |
| `lines.added`、`lines.removed` | number | 进出了多少行 |
| `lines.ranges` | `{from,to}` 的数组 | **新**文本里改动的连续段，从 1 起、闭区间、已合并，最多 200 段；一段都没留下时（纯删除）不出现 |
| `lines.coarse` | boolean | 文件大到没法逐行对齐，于是用一整段覆盖这次改动；这时 `ranges` 指的是剩下的整块正文 |
| `lines.cut` | boolean | 比较在它的上限处停下了，所以这些段只是真相的前缀，不是全部 |

三条规矩，第一条就是读者必须永远容忍「没有」的原因：

1. **没有强过说错。** 旧文本读不出来——新文件、二进制、不是 UTF 8 的编码——
   或者任何一边超过 20,000 行。这些情况下 `path` 可能在而 `lines` 不在，
   读者就什么都不显示，而不是显示一个猜测。
2. **一个字节都没变的写入不声称任何行。** 原样存回一次不是改动，末尾换行
   出现或消失也不是某一行挪了位。这两种都不报 `lines`。
3. **这些段说的是新文本。** 它们回答的是「现在我该去这个文件的哪里看」，
   也就是一个要在行上做标记的编辑器、一个点了要跳到某一行的列表所需要的。
   文本原来在哪儿是日志的事（`file/write` 带着完整内容）。

一条工具结果带着它发生时所处的事件信封 `turn`——不在轮内时是 `0`——所以读者
可以按轮把这些声称聚起来，回答「这一轮 agent 动了什么」。那种聚合是一个**视图**：
关于它没有任何东西被写下来，而一轮还在跑的时候，它的结尾还不成立。

## 5. 只追加，永不重写

1. 记录**只被追加**，在末尾，一行一条。既有记录永不编辑、重排、重新编号或删除。
2. 状态由日志派生；它永不被写回日志。一次纠正是**一个新事件**，不是一次
   编辑。
3. 压缩追加一条 `compaction/summary` 事件，它描述更早的一批 `seq` 值。它不移除
   它们。
4. `seq` 值在日志的整个生命期内都是稠密的。如果一条日志看起来出现了
   缺口，那就是损坏：报告它，不要重新编号。
5. **唯一**被允许的非追加写入，是在恢复期间把一个撕裂尾截回
   最后一条完整记录（§8）。那不会删掉任何完整记录。

写者 MUST 在报告成功之前 `fsync`，否则就要写明它接受的持久性窗口。`hearth` 默认每次
追加都 `fsync`，另有一个退出开关（`SyncEvery`、`NoFsync`）只被基准测试和测试使用。

### 5.1 压缩（`compaction/summary`）

压缩缩短读者渲染的内容，而不触碰日志所持有的东西。它追加**一个**事件，别的不追加：

```json
{"summary":"turns 1 2: the agent read a.txt and answered.","replaces":{"from":2,"to":13}}
```

  `summary` 是非空文本。老范围现在被说成曾经是什么，就是它。
  `replaces.from` 与 `replaces.to` 是含端点的 `seq` 值，指认摘要所代替的范围。它
  是描述性的：它**永不**授权删除或编辑那些记录，对日志的完整重放仍然能看到它们。

**什么可以被替换。** 选择范围的写者必须满足以下全部条件：

1. **轮次是原子的。** 一个轮次从 `turn/start` 跑到 `turn/end`；范围不得
   在轮次内部结束。一条从轮次中途开始的日志（一个更长会话的尾部）
   被当作「第一个指名某个轮次的事件开启了它」来读，而一个没有
   `turn/start` 的 `turn/end` 是一个单事件轮次，与投影所合成的一模一样。
2. **调用与结果原子。** 一个 `tool/call` 与它匹配的 `tool/result`
   是一个单元。如果结果落在所选切点之外，切点就移回到
   该调用之前：一个调用已被摘要掉的结果是不可用的。
3. **一个被替换过的范围永不再被替换。** 选择从最后一条
   `compaction/summary` 事件之后开始，所以摘要不嵌套，也没有任何 `seq`
   被描述两次。
4. **最近的尾部保持原样。** 最近的几个轮次（默认两个）永不
   被包含，所以模型总是完整地看到对话的活着的边缘。
5. **没有可折叠的东西不是错误。** 一条很短的日志，或一条其工具配对
   把每个早期轮次都粘在保留尾部上的日志，什么都不产出。

**写者拒绝什么。** 一个范围与更早一份摘要已经替换过的范围重叠的计划、一个指名日志里
不存在的 `seq` 的范围、或一份空摘要，都会以一个带类型的错误被拒绝，并且**不追加任何
字节**，所以修正计划之后的重试不可能写两遍。一次成功的压缩恰好追加一条记录，而稠密的
`seq` 序列不中断地继续（§3）。

**读者如何渲染它。** 可见的对话是**最后**一条 `compaction/summary` 事件加上它之后追加
的每一个事件；更早的消息不再渲染。累计计数器**不**重置：`turns`、`steps`、工具调用与
token 总数仍然描述整个会话，因为压缩隐藏消息，它并不把做过的工作取消。需要精确知道
一份摘要替换了哪个范围的读者读 `replaces`；完全忽略压缩的读者仍然加载日志，只是渲染
更多消息。

   

## 6. 前向兼容：未知类型与未知成员

一个旧读者必须挺过一个更新的写者写下的日志。因此：

1. **未知的 `type` 值 MUST 被接受**并保留。只理解目录一部分的读者把
   该事件当作一条不透明的记录保留：它仍然计入 `seq` 稠密度，仍然参与
   排序，也仍然被重放返回。它 MUST NOT 被丢掉，也 MUST NOT 变成错误。
2. **`payload` 里的未知成员 MUST 被接受**，并且 MUST 在
   decode/encode 往返中保持不变。如果一个实现重新编码一个它并未完全理解的
   载荷的事件，未知成员必须仍然以相同的值存在。（载荷以原始 JSON 携带，而不是
   通过一个更窄的结构体重新建模。）
3. **信封里的未知成员**（`v`/`seq`/…… 的兄弟）MUST NOT
   造成错误，但 MAY 在重新编码时被丢掉。对写者来说 v1 信封是一个封闭
   集合，所以一个未来的信封成员被当作版本变更（§9）处理，
   而不是一次增量微调。
4. **已知成员的未知值**（例如一个新的 `stopReason`）MUST NOT 被
   存储或读者拒绝。解释在投影层：一个无法识别的值被原样携带并以
   通用方式渲染。
5. v1 里没有任何东西校验载荷 schema，所以载荷内部的「未知字段」
   与「这个版本还不认识的字段」无法区分——而这正是要点。

两个一致性用例钉住这一点：`ok_unknown_event_type`（一个必须往返的 `future/...` 类型）
与 `ok_unknown_payload_fields`（必须幸存的额外载荷成员）。`ok_unknown_envelope_member`
只断言未知信封成员不是错误，因为丢弃它们是允许的。

   

## 7. 版本策略

  `v` 声明的是**单条记录**的信封版本，不是文件的版本。混合版本不合法：一条日志
  从头到尾都是 v1。如果写者必须改动某个 v1 读者处理不了的东西，它就抬升 `v` 并开始
  一个新的日志版本（或通过写一个新文件来迁移）。
  **增量变更不抬升 `v`：** 新的事件类型、新的载荷成员、新的载荷取值枚举，以及新的
  `reason`/`status` 字符串。
  **破坏性变更抬升 `v`：** 改变某个成员的类型或含义、移除或重命名一个成员、把一个
  可选成员变成必需、改变 `seq` 的含义、改变 `ts` 的单位或 id 格式，或增加一个新的
  信封成员（§6.3）。
  遇到 `v != 1` 的记录的读者 MUST 失败关闭：以 `unsupported event version` 拒绝该
  记录，不要猜，不要部分应用它。
  事件**类型**由它们的 `namespace` 版本化（v1 里的 `tool/call` 永远保持兼容；重新设计
  的变体是 `tool/call2`，或者藏在一次 `v` 抬升之后）。
  `hearth` 本身还在 1.0 之前（`0.0.0 dev`）；**事件格式不是**——v1 自 M0 一致性套件
  起就冻结了，此后的格式改动遵循上面的规则。

> 实现状态：**已完成**（M0）。`internal/session.Open` 会拒绝一条日志——只要它里面
> 任何位置出现了第一条不受支持版本的记录——返回
> `session.ErrUnsupportedVersion`（`"session: event log uses an unsupported
> format version"`），并且让文件逐字节保持不变——不截断，
> 因而也不会在一个外来前缀之后追加。出于同样的理由，`ReadFrom` 会在
> 每条记录上重新检查 `v`。测试：`TestRefusesUnsupportedVersion`、
> `TestRefusesVersionUpgradeMidLog`，位于 `internal/session/version_test.go`。
>
> 注意恢复用的是**廉价信封检查**（`seq`、`id`、`session`、
> `type`、`ts`），而不是上面那个完整的 `validate()`：校验每条记录的载荷
> 会在打开时重扫整个文件，而 10k 事件的预算负担不起
> （在 300 ms 预算下，39 MB / 10k 事件实测 102 ms）。因此载荷
> 的合法性是在写入时和 `ReadFrom` 时强制的，不是在恢复期间。

   

## 8. 撕裂尾保证

一次崩溃可能让最后一行不完整：没有 `\n`，或 JSON 在值中途被切断。这是预期的失败，
不是损坏，而且它是可恢复的。

**读者规则（恢复）。** 保留完整、合法记录的最长**前缀**：

  逐条记录地走这个文件，按 `\n` 切分。
  一条没有终止 `\n` 的记录是不完整的：停下，它不属于前缀。
  （即使它的字节恰好是合法 JSON。）
  一条解析失败或违反 §3 任何不变式的记录是不合法的：停下；它
  以及它之后的一切都不属于前缀。
  空行被忽略，不终止扫描。
  停止点之前的一切仍然按顺序可读，`seq` 值不变。

**写者规则（先恢复，再追加）。** 算完前缀之后，写者 MAY
把文件截到恰好是最后一条被保留记录结束处的字节偏移，然后用
`seq = lastKeptSeq + 1` 追加新记录。这是唯一被允许的
非追加写入。如果它不截断，它 MUST 忽略（并且永不回读）那段垃圾尾部，而下一次追加仍须
让文件可解析——实践中，就是截断。

一个实现必须提供的保证：

1. 一条撕裂的末记录永不使更早的记录不可读。
2. 恢复永不重新编号或删除一条完整记录。
3. 恢复之后，日志重新合法，追加从 `lastSeq + 1` 继续。
4. 一条撕裂的**中间**记录不能靠这条规则恢复（它是损坏，
   不是崩溃产物）；它之前的前缀仍然可读，而尾部
   丢失。写者 MUST NOT 悄悄把这个缺口补上。
5. 恢复是幂等的：跑两遍得到相同的前缀与相同的文件尺寸。

夹具：`tests/fixtures/events/v1/torn final line.jsonl`——六条完整记录，
跟着第七条在 JSON 中途被截断、且没有尾随换行的行。用 `session.Open` 打开它
必须得到六个事件（`seq` 1..6）、把文件截到那六条记录的前缀，并让下一次追加
产出 `seq` 7。

夹具纪律（由 runner 强制）：其余每个夹具都以一个换行结束它的最后
一条记录。尾部被丢掉的夹具必须撕裂在**值中途**（不合法 JSON），
因为真实的崩溃不会规规矩矩地停在值的边界上——一个被丢弃但完整的 JSON 尾部意味着
缺一个换行，runner 会失败，而不是悄悄丢掉那个夹具的最后
一个事件。

   

## 9. 读取日志：参考算法

```text
read_log(path)  > list<Event>:
    events = []
    offset = 0                      # byte offset of the next record
    for each chunk split by b'\n' in file(path):
        if chunk is not terminated by b'\n':      # torn tail
            break
        text = chunk without trailing '\r'
        if text is blank: offset += len(chunk)+1; continue
        rec = parse_json(text)                    # error  > stop (invalid JSON)
        if rec is not object                       # error  > stop (envelope)
            stop("does not match the v1 envelope")
        e = decode_envelope(rec)                   # missing/mistyped members
        validate(e)                                # v, id, ts, session, type
        if e.seq != len(events)+1: stop("not dense")
        if e.id in seen_ids: stop("duplicate event id")
        events.append(e); seen_ids.add(e.id)
        offset += len(chunk)+1
    return events, offset
```

`v != 1` 以 `unsupported event version` 停止读取（§7）。重复检测使用这条日志里
已经被接受的 id 集合，不是全局集合。

追加：

```text
append(store, session_id, type, payload)  > Event:
    e.seq  = store.last_seq + 1
    e.v    = 1
    e.id   = 32 random lowercase hex chars      # 128 bits, CSPRNG
    e.ts   = now() in Unix milliseconds
    e.session = session_id
    validate(e)
    write(json_dumps(e) + "\n")                # one line, LF
    fsync()                                    # per durability policy
    store.last_seq = e.seq
```

   

## 10. v1 里刻意不包含的东西

  没有签名、校验和或加密：日志是本地状态，由文件系统权限保护。
  没有逐记录压缩，没有二进制分帧，没有长度前缀。
  没有 schema 注册表或载荷校验（见 §4）。
  没有日志轮转：一个会话，一个文件，贯穿会话的生命期。
  没有跨会话排序：`seq` 是按日志算的，`ts` 是唯一
  跨会话的线索，而它在进程之间并不单调。

这些都是之后的增量：校验和可以是 `v` 抬升背后的一个新信封成员，而一份载荷 schema
注册表根本不需要任何格式变更。

   

## 11. 一致性套件

  `tests/conformance/cases/events/*.json`——每个文件一个用例。加一个用例必须
  永远不需要改 Go 代码。
  `tests/fixtures/events/v1/*.jsonl`——真实感的整条日志，包括撕裂
  尾。
  已退役的 Go 树里的 `tests/conformance/conformance_test.go`——runner（Go 写的，但它只使用
  公开的 session API；另一种语言里的移植才是预期用法）。

用例文件 schema（一个封闭 schema：未知成员是测试失败，所以一个笔误会
大声报出来）：

| 成员 | 类型 | 必需 | 含义 |
|     |     |     |     |
| `name` | 字符串 | 是 | 必须等于文件名主干。 |
| `description` | 字符串 | 是 | 这个用例为什么存在（≥ 10 个字符）。 |
| `lines` | 数组 | 是 | 输入日志。一个**对象**元素被压成一行 JSONL；一个**字符串**元素被逐字用作原始行（用于畸形输入，例如一个被截断的片段）。 |
| `expect` | `"ok"` \| `"error"` | 是 | 输入日志是否必须被接受。 |
| `errorContains` | 字符串 | 当且仅当 `expect == "error"` | 诊断必须包含的子串（§3 的表）。 |
| `canonical` | 数组 | 否 | 必需的重新编码形式，每个被接受的事件一个元素。以**解析后的 JSON** 比较，所以键序与空白无关。契约不固定重新编码形式时省略。 |

对每个 `ok` 用例，runner 还断言：每行解码成信封、`v == 1`、`seq` 从 1 起稠密、`id`
非空且唯一、`ts > 0`、`session` 与 `type` 非空；并且 decode → encode → decode 是一
个不动点，载荷字节相等（JSON 规范化之后）。对每个 `error` 用例，它断言失败消息包含
`errorContains`。

一个重实现通过的标准是：它对所有用例都复现了接收/拒绝的决定、`canonical` 字节
（作为解析后的 JSON）以及 §8 的夹具行为。