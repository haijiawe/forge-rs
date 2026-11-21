> 本文件是 [api.md](api.md) 的中文翻译。**英文原文是规范**：两者冲突时以英文为准，并回来修本文件。
> 原文里用反引号标的字段名、路径、命令、错误码一律不译，它们必须与代码逐字一致。

<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# hearth HTTP + WebSocket API 契约，协议版本 1

> **中文导读。** 这份契约管一件事：`hearth web` 对外暴露的 HTTP + WebSocket 面（协议版本 1）。
> 浏览器 UI、桌面壳、任何脚本客户端都只按这份文本工作；一旦有客户端按协议 1 发布，服务端就不能单方面改它。
>
> 怎么读：§1 是整个面的形状；§2 讲认证（loopback 之外一律拒绝、`?token=` 会换成 cookie）；
> §3 讲协议协商（`X-Hearth-Protocol: 1`，WebSocket 上等价的是子协议 `hearth.v1`）；
> §4 一条路由一节，每节给方法、请求体、成功响应、以及每种失败的状态码与错误码；
> §5 是错误信封（`{code, message, details?}`）；§6 讲哪些还能改、哪些不能。
>
> 几个术语：**token** 是启动时打印到 stderr 的凭据，`GET /?token=` 之后变成 `hearth_token` cookie；
> **Idempotency-Key** 是 `POST /messages` 的必填头，缺了返回 428 而不是默默重放；
> **lastSeq** 是这条会话日志当前的最大 `seq`，客户端拿它判断自己有没有落后；
> **「不只是 GET」** ——同一路径不同方法常常对应不同能力（`/file` 的 GET 读、PUT 存），405 是真实答案。
>
> 改动时必须同步四处：① `crates/protocol/src/protocol.rs` 的 wire 类型；
> ② `web/src/api/protocol.ts`——**没有生成器**，是手工维护的镜像，必须同一次提交改；
> ③ `crates/server/src/server.rs` 的 handler；
> ④ `crates/server/src/routes.rs` 的 `ROUTES` 表——`crates/server/tests/routes_table.rs` 会逐行发真请求，
> 声明了却服务不了会红，服务得了却没声明没人发现。再加一条 `tests/conformance/cases/http/` 用例。

> **已实现。** 协议 1 在 M3 发布，今天由 `hearth web`（`docs/contracts/cli.md` §2.7）提供服务，
> 它同时从同一个源提供浏览器 UI。`internal/server` 是实现；`web/` 是协议 1 的客户端，
> 而 `internal/protocol` + `web/src/api/protocol.ts`（由它生成）是共享的 wire 类型。
>
> 这份文档写在代码之前，它同时仍然是兼容性承诺：一旦有客户端按协议 1 发布，服务端就不能
> 单方面改它。在已发布的服务器与这段文本仍然不一致的地方，那是缺陷，并且它已经在与其相关的
> 那一节里就地标出——那是一份待办清单，不是可以弯曲线格式的许可。
>
> 底下的事件格式是冻结的：`docs/contracts/events-v1.md`。这套 API 是那些事件的传输层，
> 它绝不能暴露无法从日志派生的状态。

版本：**1**。通过 `X-Hearth-Protocol` 头协商（§3）。

---

## 1. 面的形状

- 传输：HTTP/1.1（接受 HTTP/2），**仅限回环地址**，外加一个用于流式传输的 WebSocket 升级。
- 基地址：`http://127.0.0.1:{port}`。服务器绑定 `127.0.0.1`，绝不绑定 `0.0.0.0`；非回环地址的
  对端在路由之前就被 `403` 拒绝（§2）。
- 内容类型：每个请求体与响应体都是 `application/json; charset=utf-8`。请求体内容类型不是
  它的请求是 `415 unsupported_media_type`。
- 请求体是 UTF-8 JSON 对象。未知的请求成员必须被忽略（前向兼容与事件格式同理）。
- 每个响应都携带 `X-Hearth-Protocol: 1`。
- 服务器是单用户、本机的：没有多租户概念，没有用户 id。
- 所有状态都从事件日志派生。API 可以缓存投影，但重启后必须仅凭日志重现完全相同的响应。

### 大小写与编码规则

- JSON 成员名是 `camelCase`。
- 时间戳是**整数 Unix 纪元毫秒**（`ts`、`createdAt`、`updatedAt`），绝不是 ISO 字符串，
  绝不是浮点数。
- `seq` 是 JSON 整数；用 IEEE-754 双精度浮点（JavaScript）的客户端在 `2^53` 之上必须按
  `BigInt` 处理它。
- id 是不透明字符串。客户端不得解析它们。
- 缺席的可选成员是省略，而不是 `null`，除非下面的某个 schema 说明某个成员可为 null。

---

## 2. 认证与仅限本机访问

- 首次启动时，服务器生成一个 **32 字节的随机令牌（token）**（256 位，无填充的 base64url），
  并存进 `<dataDir>/token`，权限为仅所有者可访问（`0600`）；之后再启动会复用那个文件。
  服务器本身什么都不打印：`hearth web` 把 UI 的 URL 打到 stderr，令牌作为它的 `token` 查询
  参数，每次启动打一次，而那个 URL 就是一份凭据（`docs/contracts/cli.md` §2.7）。
- 每个请求都必须出示令牌：或者用 `Authorization: Bearer {token}`，或者（对浏览器客户端）
  用 `HttpOnly` cookie `hearth_token`。缺失或错误的令牌 → `401 unauthorized`。令牌以
  恒定时间比较。
  - 浏览器在 `GET /` 导航时无法设置 `Authorization`，所以由服务器自己设置 cookie：当它为一个
    `token` 查询参数匹配的请求提供外壳时，响应携带
    `Set-Cookie: hearth_token={token}; HttpOnly; SameSite=Strict; Path=/`。`token` 参数错误
    或缺席时仍然提供外壳，只是不带 cookie（操作者也可以带外设置它；协议 1 里没有认证路由）。
    正是这一点让 `hearth web` 打印的那个 URL 端到端可用：包括页面无法附带请求头的
    WebSocket。
- 对端地址不是回环地址的请求，无论令牌如何都以 `403 forbidden` 拒绝。协议 1 里没有
  “绑定到网络”这种模式。
- 改变状态的请求（`POST`）必须携带值为 `http://127.0.0.1:{port}` 或
  `http://localhost:{port}` 的 `Origin` 头；`Origin` 缺失或不匹配 → `403 forbidden`。
  这挡住了浏览器标签页发起的跨站请求伪造。
- WebSocket 升级套用同样的令牌与 `Origin` 检查；被拒绝的升级返回的是升级之前的 HTTP 状态，
  而不是一个 WS 关闭帧。

---

## 3. 协议协商

| 头 | 方向 | 必需 | 值 |
| --- | --- | --- | --- |
| `X-Hearth-Protocol` | 请求 | 是 | `1` |
| `X-Hearth-Protocol` | 响应 | 总是 | `1` |

- `/api/*` 请求缺少这个头 → `400`，错误码 `protocol_required`。
- 服务器不讲的版本（例如 `2`）→ `400`，错误码 `protocol_unsupported`。服务器不会回落到
  另一个版本。
- 检查的顺序是可观察的，也是契约的一部分：非回环地址的对端先得到 `403`，然后检查令牌
  （`401`），最后才检查协议头（`400`）。因此，一个既无令牌也无协议头的请求得到的是
  `401 unauthorized`，而不是 `400 protocol_required`——服务器不会告诉一个未认证的调用方
  它讲的是哪个协议。
- `GET /healthz` 免于版本协商与认证（§4.1）。
- WebSocket 客户端在升级请求上发送这个头；每个服务器帧也携带 `"protocol": 1`，这样剥离
  头的代理无法悄悄降级一条流。
  - 浏览器根本无法在 WebSocket 握手上发送请求头。因此这类客户端改为在
    `Sec-WebSocket-Protocol` 里提供子协议 `hearth.v1`，而不是发送 `X-Hearth-Protocol`；
    服务器接受提供了它的握手，并在 `101` 里回显
    `Sec-WebSocket-Protocol: hearth.v1`（RFC 6455 §4.2.2 要求把选中的子协议点名，否则浏览器
    会判定连接失败）。两种信号任一都满足协商；一个都不带的握手仍然是
    `400 protocol_required`。

---

## 4. 路由

所有路径都在 `{base}` 之下。`{id}` 是一个会话 id；未知的 id 是 `404 not_found`，绝不是
空的 `200`。

| # | 方法 | 路径 | 用途 | 成功 |
| --- | --- | --- | --- | --- |
| 1 | `GET` | `/healthz` | 存活探测，无认证，无版本头 | `200` |
| 2 | `GET` | `/api/sessions` | 列出会话 | `200` |
| 3 | `POST` | `/api/sessions` | 创建一个会话 | `201` |
| 4 | `GET` | `/api/sessions/{id}` | 一个会话的元数据/投影 | `200` |
| 5 | `GET` | `/api/sessions/{id}/events` | 一页事件 | `200` |
| 6 | `POST` | `/api/sessions/{id}/messages` | 提交一条用户消息 | `202` |
| 7 | `POST` | `/api/sessions/{id}/approvals/{approvalId}` | 回答一个审批 | `200` |
| 8 | `GET` | `/api/sessions/{id}/stream` | WebSocket：快照，然后是实时事件 | `101` |
| 9 | `POST` | `/api/sessions/{id}/cancel` | 停止正在运行的那一轮 | `202` |
| 10 | `POST` | `/api/sessions/{id}/steer` | 把文本注入正在运行的那一轮 | `202` |
| 11 | `GET` | `/api/sessions/{id}/attachments/{attachmentId}` | 一个附件的字节 | `200` |
| 12 | `GET` | `/api/capabilities` | 这个进程能提供什么，与会话无关 | `200` |
| 13 | `GET` | `/api/teams`, `/api/teams/{board}` | 这台机器跑过的团队看板 | `200` |
| 14 | `GET` | `/api/sessions/{id}/goal` | 这个会话仍在努力达成的目标 | `200` |
| 15 | `GET` | `/api/presents` | 模型交给操作者的文件，最新的在前 | `200` |
| 16 | `GET` | `/api/sessions/{id}/subagents` | 这个会话委派出去的子代理 | `200` |
| 17 | `GET` | `/api/sessions/{id}/files` | 会话工作区里的一个目录 | `200` |
| 18 | `GET` | `/api/sessions/{id}/file` | 那个工作区里的一个文件，以文本形式 | `200` |
| 19 | `GET` | `/api/sessions/{id}/diff` | 那个工作区里 git 知道的改动；`?path=` 把它收窄到一个文件的各行 | `200` |
| 20 | `PUT` | `/api/sessions/{id}/file` | 把编辑过的缓冲区保存进那个工作区 | `200` |

### 4.1 `GET /healthz`

无认证，无 `X-Hearth-Protocol`，无请求体。`200`，内容为：

```json
{"status":"ok","protocol":1,"version":"0.0.0-dev","uptimeMs":123456}
```

供守护进程使用。它不得触碰文件系统或日志，并且必须在 100 ms 内回答。它对模型提供方什么
都不说：一个没有网络的健康进程仍然报告 `ok`。

### 4.2 `GET /api/sessions`

查询参数：`limit`（默认 50，最大 200）、`cursor`（不透明，来自上一页）、`status`（可选
过滤：`idle|running|awaiting_approval|ended|aborted`）。

```json
{
  "sessions": [
    {
      "id": "s_01jya7c9q3r5t7v9w1x3y5z7ab",
      "createdAt": 1794787200000,
      "updatedAt": 1794787222110,
      "status": "ended",
      "lastSeq": 15,
      "turns": 2,
      "workspace": "D:/work/demo",
      "model": "deepseek-chat"
    }
  ],
  "nextCursor": null
}
```

`status` 由最近的相关事件派生（`session/start`、`turn/start`、没有决定结果的
`approval/request`、`session/end`、`turn/end`）。最后一页上 `nextCursor` 是 `null`；排序按
`updatedAt` 降序，并列时按 `id` 决出，所以翻页是稳定的。

### 4.3 `POST /api/sessions`

```json
{"workspace":"D:/work/demo","model":"deepseek-chat","profile":"default"}
```

`workspace` 必填，且必须是绝对路径；不存在的目录会被创建（`0755`），而存在但不是目录的
路径会被拒绝。`model` 与 `profile` 可选，缺省时回落到配置。服务器追加一个 `session/start`
事件（因此它带 `seq: 1`）并响应 `201`：

```json
{"session":{"id":"s_01jya7c9q3r5t7v9w1x3y5z7ab","createdAt":1794787200000,"updatedAt":1794787200000,"status":"idle","lastSeq":1,"turns":0,"workspace":"D:/work/demo","model":"deepseek-chat"},"lastSeq":1}
```

错误：`400 bad_request`（缺少 `workspace`、路径不是绝对路径、路径存在但不是目录、或者
未知的 `profile`）、`409 conflict`（同一工作区已经有一个会话处于 `running` 或
`awaiting_approval`）。

> **实现状态。** 这个构建里没有 profile 注册表，只有一个已知取值 `"default"`（空 profile
> 也被接受，含义相同）。其他任何值都以 `400 bad_request` 拒绝，而不是被记录：日志不得
> 声称一个没有任何代码能兑现的预设。等真正的注册表落地时，`internal/server/server.go` 里的
> 白名单（`defaultProfile`）就是唯一会增长的地方。

### 4.4 `GET /api/sessions/{id}`

与 §4.2 中一个元素相同的对象，外加派生细节：

```json
{
  "id": "s_01jya7c9q3r5t7v9w1x3y5z7ab",
  "createdAt": 1794787200000,
  "updatedAt": 1794787222110,
  "status": "awaiting_approval",
  "lastSeq": 15,
  "turns": 2,
  "workspace": "D:/work/demo",
  "model": "deepseek-chat",
  "pendingApproval": {
    "approvalId": "apr_01",
    "callId": "call_01",
    "tool": "shell",
    "args": {"command": "rm -rf build"},
    "reason": "destructive command",
    "requestedAt": 1794787301300,
    "timeoutMs": 120000
  }
}
```

除非有一个 `approval/request` 没有匹配的 `approval/decision`，否则省略 `pendingApproval`。
对一个 `running` 会话，在日志追赶期间，同一个请求也可能由缓存回答；响应里的 `seq` 值绝不能
跑在日志前面。

### 4.5 `GET /api/sessions/{id}/events`

查询参数：`after`（排他的 `seq`，默认 `0`）、`limit`（默认 100，最大 1000）。

```json
{
  "sessionId": "s_01jya7c9q3r5t7v9w1x3y5z7ab",
  "from": 4,
  "to": 8,
  "lastSeq": 15,
  "events": [ { "v": 1, "seq": 4, "id": "…", "ts": 1794787202098, "session": "…", "turn": 1, "step": 1, "type": "assistant/message", "payload": {"text":"…"} } ],
  "nextAfter": 8
}
```

- `events` 是日志里的**原样事件对象**，按 `seq` 顺序，绝不重新序列化成别的形状
  （`docs/contracts/events-v1.md` §2）。
- 当 `after >= lastSeq` 时 `events` 可以是空的；那是 `200`，不是 `404`。
- `from` 是返回的第一个 `seq`，`to` 是最后一个；页为空时两者都是 `0`。`nextAfter` 是客户端
  继续翻页时要作为 `after` 传的值；它等于 `to`（空页时则是不变的 `after`）。
- 超过上限的 `limit` 被截断到上限，而不是被拒绝。不是正整数的 `limit`，或不是非负整数的
  `after`，是 `400 bad_request`（`"limit must be a positive integer"` /
  `"after must be a non-negative integer"`）。
- 未知类型的事件原样返回（前向兼容）。

### 4.6 `POST /api/sessions/{id}/messages`

提交一条用户消息，并开始（或继续）一轮。

接受一条消息会在这一轮运行之前先追加 `turn/start`，再追加 `user/message`，两者都携带轮号。
响应里的 `seq` 是 `user/message` 的 seq，不是 `turn/start` 的。

必需的头：`Idempotency-Key: {uuid}`。同一个键重放会返回同样的结果，而不是追加第二条
`user/message`。

发往同一个会话的并发提交是串行化的：每条被接受的消息得到自己的轮号和恰好一对
`turn/start`/`user/message`，所以两个请求永远不会共用一轮。响应在这一轮运行之前就返回了，
所以在上一轮仍在运行时，第二个 POST 会被接受。

如果日志收下了 `turn/start`，但随后的 `user/message` 追加失败，服务器会为那一轮追加带
`"reason":"error"` 的 `turn/end`——日志是只追加的，所以这一轮无法被“取消开启”——并回答
`500`。因此会话保持 `idle`，而不是报告一个没有提示词却在运行的轮；用同一个
`Idempotency-Key` 重试*不会*被去重：失败那次尝试的提示词没有落地，所以重试会开启下一轮
并运行。

```json
{"text":"Run the package tests.","attachments":[{"kind":"file","path":"notes/todo.md"}]}
```

`text` 必填且非空（最大 256 KiB）；`attachments` 可选，最多 10 项，每项是
`{kind:"file"|"image"|"diff", path?, data?, mediaType?, name?}`。

附件的正文**不**是这一轮事件的一部分。`data`（标准 base64，每个最大 1 MiB，全部加起来
最大 8 MiB）被解码并写到 `<dataDir>/blobs/` 下的一个文件里，而 `user/message` 事件只记录
`{kind, id, mediaType, name, bytes, path}`。三个理由，都是量出来的而不是假设的：日志的每个
读者都要为这段 base64 付账（投影、fork、索引，以及每一个返回整条事件行的 `GET /events`
页）；日志是只追加的，所以上传的正文永远无法被删除；而 fork 逐字复制事件，所以一张 6 MiB
的截图会变成 12 MiB。

* `kind:"file"` 指认一个工作区路径，不携带正文。`path` 必填。
* `kind:"image"` 与 `kind:"diff"` 必须有 `data`。

正文的字节用 `GET /api/sessions/{id}/attachments/{id}`（§4.11）取回。

响应 `202`：

```json
{"seq":16,"accepted":true,"deduplicated":false}
```

同一个键的重放返回 `200`（不是 `202`），带 `"deduplicated":true` 和原来的 `seq`。错误：
会话已结束或处于 `awaiting_approval` 时是 `409 conflict`；输入格式良好但无法使用（例如
附件太大）时是 `422 unprocessable_entity`；请求体格式错误是 `400 bad_request`；缺少
`Idempotency-Key` 时是 `428 precondition_required`。

### 4.7 `POST /api/sessions/{id}/approvals/{approvalId}`

```json
{"decision":"allow","note":"generated output, safe to remove"}
```

`decision` 是 `allow` 或 `deny`；`note` 可选（最大 4 KiB）。追加一个 `approval/decision`
事件，并在 `allow` 时恢复被阻塞的工具调用。

响应 `200`：

```json
{"approvalId":"apr_01","decision":"allow","seq":5}
```

错误：`404 not_found`（没有这个审批，或它不在待决状态）、`409 conflict`（它已经被回答
过——一次双击不得追加两个决定）、未知 `decision` 取值是 `400`。这个路由是释放一个被阻塞的
工具调用的唯一途径；超时本身就是服务器追加的一个 `deny` 决定，带 `decidedBy: "timeout"`。

### 4.8 `GET /api/sessions/{id}/stream` (WebSocket)

在同一个基地址上升级。查询参数 `after`（默认 `0`）用于续传：服务器先发一个快照，然后发
每个 `seq > after` 的事件，然后发实时事件，无空洞、无重复。

关闭码：`1000` 正常，`1002` 协议错误（格式错误、二进制或不受支持的客户端帧；不支持的
opcode），`1011` 服务器失败（包括“客户端太慢”）。服务器每 30 s ping 一次；错过两次 ping
的客户端会以 `1000` 被断开。

策略失败**不是**关闭码：令牌错误或缺失、`Origin` 外来或缺失、非回环地址的对端，全都在升级
*之前*被拒绝，所以客户端看到的是普通的 HTTP 状态——`401 unauthorized`、`403 forbidden`，
或者（`X-Hearth-Protocol` 缺失/不匹配时）`400 protocol_required` /
`protocol_unsupported`（§2、§3）。协议 1 没有 `1008` 帧：等到套接字打开时，每一项策略检查
都已经通过了。

#### 帧

每一帧都是单个 UTF-8 JSON 对象，带一个 `kind` 判别器。所有服务器帧都携带
`"protocol": 1`。

**`snapshot`** ——永远是一条新连接上的第一帧：

```json
{
  "kind": "snapshot",
  "protocol": 1,
  "lastSeq": 15,
  "session": { "id": "s_01jya7c9q3r5t7v9w1x3y5z7ab", "status": "running", "turns": 2, "workspace": "D:/work/demo", "model": "deepseek-chat", "updatedAt": 1794787222110 }
}
```

**`event`** ——一条被追加的事件，原样，按 `seq` 顺序：

```json
{"kind":"event","protocol":1,"event":{"v":1,"seq":16,"id":"…","ts":1794787300000,"session":"…","turn":2,"type":"tool/call","payload":{"callId":"call_03","tool":"shell","args":{"command":"go test ./..."}}}}
```

**`delta`** ——流式助手文本，成批发送。token（词元）按 `(turn, step)` 聚合，最多每 **50 ms**
冲刷一次（以及在一步结束时），所以客户端永远不会每 token 收到一帧：

```json
{"kind":"delta","protocol":1,"turn":2,"step":2,"seq":15,"text":"One test fails: the loop "}
```

`seq` 是这个 delta 所属助手消息的 `seq`；某一步的 delta 会在那条 `assistant/message` 事件
之前到达。delta 只是提示性的：权威文本是 `assistant/message` 的载荷。重连的客户端必须能
仅凭 `event` 帧重建对话记录。

**`error`** ——一个可恢复的流级问题；除非 `fatal` 为 true，连接保持打开：

```json
{"kind":"error","protocol":1,"fatal":false,"error":{"code":"log_read_failed","message":"could not read events.jsonl","details":{"path":"…"}}}
```

客户端 → 服务器的帧仅限于存活信号；一切变更都走 HTTP：

```json
{"kind":"ping"}
{"kind":"pong","protocol":1}
```

其他任何客户端帧都是协议错误（`1002`）。顺序保证：对单个会话，`event` 帧按严格递增的
`seq` 到达，没有空洞。出现空洞就是服务器的 bug；客户端应带 `after` 重连并重新读取
（`GET …/events`）来重新同步。

### 4.9 `POST /api/sessions/{id}/cancel`

取消当前正在运行的那一轮。无请求体；没有请求体的 POST 不需要 `Content-Type`。

```json
{"cancelled":true,"seq":16}
```

- `202` ——一个正在运行的轮被要求停止。`seq` 是调用时刻日志的最后一个 `seq`。
- `409 conflict` ——没有轮在运行。
- 未知会话是 `404 not_found`；没有令牌是 `401`。

这一轮的结束**不是**由这个路由写的。取消 runner 启动时所用的 context 会让循环停止，而循环
自己追加 `turn/end{reason:"cancelled"}`：它拥有“这一轮怎么结束”的记录，两个写者会为一轮
产生两条记录。客户端会看到结束以 `event` 帧的形式到达（§4.8），永远不会出现在这个响应里。

在日志里这一轮仍然打开期间，取消是幂等的：第二次调用还是 `202`，因为日志——唯一的真相
——仍然显示这一轮在运行，而 context 取消是幂等的。一旦 `turn/end` 被追加，答案就是 `409`：
没有什么可取消的了。

`/interrupt` 是刻意不实现的：在一次性步骤的模型下，取消一步*就是*取消这一轮，一个单独的
动词会承诺一个循环并不具备的粒度。

### 4.10 `POST /api/sessions/{id}/steer`

把文本注入已经在运行的那一轮，而不是开启下一轮。

头：`Idempotency-Key` 必填，规则同 §4.6。

```json
{"text":"also check the retry path"}
```

`202` 回答时给出被追加的 `user/message` 的 seq：

```json
{"seq":17,"accepted":true,"deduplicated":false}
```

- 事件在响应**之前**就被追加，形如 `{"text":…,"steering":true,"idempotencyKey":…}`，所以
  即使回答之后立刻崩溃，文本也能存活。steer 永远不开启一轮：它属于已经打开的那一轮。
- 同一个 `Idempotency-Key` 的重放是 `200`，带 `deduplicated:true` 和原来的 `seq`；事件只被
  追加一次。
- `409 conflict` ——没有轮在运行。往虚空里 steer 是一个错误，不是留给下一轮的消息。
- `422 unprocessable_entity` ——`text` 为空或只有空白。
- `428 precondition_required` ——缺少 `Idempotency-Key`；`text` 超过 256 KiB 是 `400`；
  未知会话是 `404`。
- 投递是每轮至多一次：文本进入队列，循环在两步之间取走它。轮结束时仍在队列里的文本会从
  队列中丢弃（它的事件仍留在日志里）；它不会被重放进下一轮。

### 4.11 `GET /api/sessions/{id}/attachments/{attachmentId}`

返回一个附件的正文，与上传时逐字节相同。

`Content-Type` 是发送方声明的 `mediaType`，或 `application/octet-stream`。响应永远是
`Content-Disposition: attachment` 加上 `X-Content-Type-Options: nosniff`：这些字节来自
客户端，浏览器不得在持有令牌的那个源里把它们当文档渲染。`Cache-Control` 是
`private, max-age=31536000, immutable`——一个名字永远指同一个正文。

- `attachmentId` 必须是 32 个十六进制字符；其他任何值都在触碰文件系统之前就是 `404`，
  所以名字不可能变成路径。
- 这个 id 必须出现在**本**会话的日志里。通过另一个会话的 URL 取到二进制块是 `404`，
  不是命中：“不在本会话里”与“从不存在”之间的差别描述的是别人的数据。
- `404 not_found` ——没有这个会话、没有这个引用、或没有这个文件。

### 4.12 `GET /api/capabilities`

描述的是**这个进程**，而不是某一个会话，所以它在任何会话存在之前就能回答。它是进程启动时
拍下的快照，不是一次查询：插件在监听器绑定之前只加载一次，所以模型能触及的东西在进程运行
期间不会改变。

```json
{
  "mode": "workspace-write",
  "workspace": "C:/work",
  "model": "deepseek-v4.1-flash",
  "plugins": [{ "name": "counter", "detail": "1.0.0 (node runtime)", "active": false }],
  "experts": [],
  "teams": [],
  "bundles": [],
  "skills": [],
  "mcp": [],
  "tools": [{ "name": "fs_read", "detail": "reads a file in the workspace", "active": true }],
  "events": ["session/start", "session/end", "turn/start"],
  "routes": [{ "name": "GET /api/capabilities", "detail": "…", "active": true }]
}
```

每一组总是存在，空数组意味着“没有配置”——绝不是“不支持”。`active` 说明这个构建此刻是否
把该条目提供给模型：一个已安装但被禁用的插件，或一个标记为 `enabled: false` 的名册条目，
会以 `active: false` 出现。那一行正是这个路由存在的理由；描述它才是重点，所以它永远不会
被省略。

最后三组是**词汇表，不是配置**。它们回答“这个构建知道什么”，而且在一台什么都没装的机器上
它们也是真的：

- `tools` 列出无条件注册的工具。只在插件或 MCP 服务器加载之后才出现的工具*不*在其中——
  启动时拍下的快照不可能知道它们。实际到达模型的东西在会话的 `llm/call` 事件里，那才是
  这个问题唯一的权威。
- `events` 列出写入方构建目录里的每一个事件类型，按代码声明它们的顺序。遇到不认识的类型的
  读者可以跳过它；这张列表就是读者发现自己看到的东西比自己更新的一种方式。
- `routes` 列出 HTTP 面：每个路由一个 `"<METHOD> <path>"` 字符串，路径用 `{placeholders}`
  写。加一条路由而不加一行没有代价；一行指向不存在的路由则是一个红的测试，因为一个比它的
  路由活得更久的声明，会让 agent 调用一个 URL，并把由此得到的 `404` 读成自己的错误。

`events` 与 `routes` 由服务器从构建本身填充，所以即使这个进程什么都没接，它们也是正确的。
`tools` 由接工具的那个人填充，因为枚举它们意味着构建它们。

这里没有任何东西在请求路径上启动进程或遍历磁盘：服务器携带的是 CLI 在启动时发现的数据，
因为能发现它的那些 crate 不是这一层可以依赖的。

- 仅 `GET`；其他任何方法都是 `405 method_not_allowed`。
- `detail` 字符串是给人看的，不是稳定的机器接口。机器可读的字段是 `name` 与 `active`；
  其他一切都可能变化。

### 4.13 `GET /api/teams` 与 `GET /api/teams/{board}`

描述的是**这台机器**上的团队看板，而不是某一个会话。`hearth team run` 把一次运行写进
`<dataDir>/teams/<slug>-<YYYYMMDD-HHMMSS>/`，这个路由是浏览器看它的窗口。它只读文件，
什么都不启动：没有 worker 进程、没有看板修复，请求路径上也不会创建目录。

```json
{
  "boards": [
    {
      "board": "release-squad-20261118-002223",
      "team": "release-squad",
      "parallel": 2,
      "createdAt": "2026-11-18T00:22:23Z",
      "tasks": [
        {
          "id": "t1",
          "subject": "Add the teams route",
          "status": "completed",
          "owner": "api-reviewer"
        }
      ],
      "workers": [
        {
          "name": "api-reviewer",
          "ok": true,
          "note": "1 turn(s), session s_abc",
          "events": 2
        }
      ]
    }
  ]
}
```

`GET /api/teams/{board}` 返回其中一个对象，不带外层包装：同样的字段，没有 `boards` 数组。

- `board` 是目录名，也是客户端再次发问时用的键。`team` 与 `createdAt` 是从那个名字里读出来
  的，因为 `board.json` 两者都不带：目录名是 `<slug>-<YYYYMMDD-HHMMSS>`，而 `createdAt`
  是那个时间戳按 UTC 渲染成 RFC 3339 的结果。名字不符合这个形式的目录（`TeamTool` 的桥接
  会按它的 call id 命名）报告自己的修改时间，并在 `diagnostics` 里说明这一点。
- `parallel` 是 `hearth team run` 一次允许的 worker 数量。没有任何东西把它持久化——
  `--max-parallel`、`team.max_parallel` 和默认值在 CLI 里解析完就被丢弃——所以这个构建写
  下的看板报告 `null`，而不是一个服务器不得不虚构出来的数字。客户端必须把 `null` 渲染成
  未知。
- `tasks` 与 `workers` 是 `board.json` 和 `workers/<name>.jsonl` 里持有的内容，被裁剪成看板
  视图所显示的东西。`tasks[].status` 按写下时的原样透传（`pending`、`in_progress`、
  `completed`）；未分配的任务 `tasks[].owner` 是 `null`。
- `workers[].ok` 与 `workers[].note` 来自那份记录的 `worker/end` 行，`events` 数的是其中的
  JSON 行数：已经结束的 worker 是两行，仍在运行的 worker 是一行。没有 end 行的记录报告
  `ok: null`——**不是** `false`——并在 `diagnostics` 里说明原因：一个还没结束的 worker 并没有
  失败，而一个在运行中途被读的看板不得让每个 worker 看起来都坏了。因此 `ok: null` 的意思是
  “这个构建说不出来”，这是与上面 `parallel` 相同的纪律。
- `diagnostics` 只在出了问题时出现：写了一半的看板、无法解析的 `board.json`、这个构建不读
  的看板版本、缺失的 `workers/` 目录、一行不是 JSON 的内容。健康的看板没有这个键。这些
  字符串是给人看的，不是稳定的机器接口，正如 §4.12 对待 `detail` 那样。
- `boards` 最新的在前，先按 `createdAt`、再按名字排序，所以同一秒启动的两次运行仍然有稳定
  的顺序。
- 从没跑过团队的机器没有 `<dataDir>/teams/` 目录，答案是 `{"boards":[]}`。路由不会创建它：
  一次读取如果创建了目录，就会让“这里跑过团队吗”变得无法回答。
- 一个垃圾看板永远不会让整个路由失败。写了一半或读不了的目录会连同已知的信息和一条诊断
  一起报告，所以单个损坏的看板无法让操作者看不见另外九个。
- `{board}` 必须是一个普通的路径段：不能有 `.`，不能有 `..`，不能有 `/` 或 `\`，不能以点
  开头，不能有 `:`。其他任何值都在构造路径**之前**就是 `404 not_found`，这与 §4.11 对附件
  id 施加的是同一条纪律，所以一个名字无法变成指向隔壁那个令牌文件的路径。
- `404 not_found` ——没有这个看板，或名字不是普通的目录名。
- 仅 `GET`；其他任何方法都是 `405 method_not_allowed`。
- `500 log_read_failed` ——`<dataDir>/teams/` 存在但完全读不了。一个坏看板永远不会是这个。

### 4.14 `GET /api/sessions/{id}/goal`

这个会话正在努力达成的目标，由它日志里的 `goal/created`、`goal/continued` 和 `goal/ended`
记录折叠而成——与 `hearth goal status` 打印的是同一次折叠。它是对日志的一次读取，别的什么
都不是：一个活在进程内存里的目标，就是一个会被重启丢掉的目标，而“扛过重启”正是目标存在的
理由。

一个从来没有目标的会话：

```json
{"goal": null}
```

一个有目标的会话：

```json
{
  "goal": {
    "id": "g_01jya7c9q3r5t7v9w1x3y5z7",
    "text": "finish the goal route",
    "maxRounds": 10,
    "rounds": 2,
    "status": "active",
    "nextRound": 4
  }
}
```

- `rounds` 数的是**已花掉的续做次数**，不是轮数。第一轮是随 `goal/created` 一起来的，所以
  一个续做过两次的目标处在它的第三轮，并报告 `rounds: 2`。这是日志持有的数字，不是它的人话
  渲染。
- `maxRounds` 是**轮**的预算，不是续做次数的预算：`maxRounds: 2` 买到创建那一轮加上恰好一次
  续做。`Goal::next_action` 执行这条边界（`rounds + 1 >= max_rounds` 意味着停止），而这个
  路由报告它的决定，而不是重算一个。
- `nextRound` 是循环接下来真正会开启的轮次，只有当 `next_action` 说 `Continue` 时才存在。
  对每一个不活跃的目标——`complete`、`blocked`、`cancelled`、`exhausted`——它都是**缺席**的，
  因为那里的一个轮次数字会承诺一个没人会跑的轮。
- `nextRound` 是一个**轮次编号**，与 `maxRounds`、`rounds` 和 `hearth goal status` 用的是同一
  套编号，其中 `goal/created` 已经花掉了第 1 轮。所以上面的例子（处在第三轮）报告
  `nextRound: 4`，而 `rounds: 0` 的目标报告 `nextRound: 2`。`Goal::next_action` 交回的却是
  一个*续做*索引——小一——而这个路由把那个一加回来。镜像这个字段的客户端不得再加一次：
  `rounds + 2` 是轮次编号，`rounds + 1` 是续做索引，把后者当轮次报告，就是那个让浏览器与
  `hearth goal status` 对同一份日志产生分歧的差一错误。
- 对一个预算已经花光、但仍是 `active` 的目标，它也**缺席**：`fold` 仍然说 `active`，因为还
  没有 `goal/ended` 被写出来，而 `next_action` 已经说 `Stop { Exhausted }`。
  `status: "active"` 而没有 `nextRound` 是一个真实且预期的答案；它的意思是“预算没了，收尾
  还没写”。客户端必须把它渲染成没有下一轮。不要通过计算 `rounds + 1` 来“修”这一点：在
  `maxRounds: 2` 且已经花掉一次续做的情况下，`rounds: 1` 而没有 `nextRound` 才是真相，而
  `nextRound: 2` 恰恰是这个缺席成员所避开的那个谎。
- `status` 是 `active`、`complete`、`blocked`、`cancelled`、`exhausted` 之一，拼写与
  `hearth_goal::Status::as_str()` 拼的一致。
- 最近的 `goal/created` 说了算。一个结束了一个目标又开启了另一个目标的会话，报告活着的那个；
  而一条带着更旧 `goalId` 的尾部 `goal/ended` 不会关掉新目标。
- 部分不可读的日志仍然回答 `200`。`fold` 跳过每一条它无法投影的目标记录——没有载荷、载荷
  不是对象、没有 `goalId`、理由它不认识的 `goal/ended`——而这个路由跳过每一行完全不是 v1
  信封的内容，比如一条残缺的尾部或一条外来的记录。答案是那些可读记录折叠出的目标，如果
  它们都没指认一个目标，就是 `{"goal": null}`。它永远不是 `500`：一个坏字节不得让操作者
  看不见一个确实在那里的目标。
- 日志存在但不含任何可读记录的会话是 `404 not_found`，与 `/api/sessions/{id}` 给它的答案
  相同。文件缺失、目录为空、日志空白不是三个不同的答案。
- 路由什么都不创建：没有日志、没有目录、没有锁、没有 store 句柄。读取一个不存在的会话
  不得把它带入存在。
- `404 not_found` ——没有这个会话。`405 method_not_allowed` ——`GET` 以外的任何方法。
  `500 log_read_failed` ——日志存在但它的字节读不了；那不是“没有这个会话”，因为这个会话
  可能完好无损。
- `hearth goal status --json` 折叠同样的记录，但把 id 叫作 `goalId`，且不带 `nextRound`。
  这个路由是 HTTP 形状；在两者之间移植的客户端要重命名字段，而不是假设它们逐字节相同。

### 4.15 `GET /api/presents`

模型用 `present` 工具交给操作者的文件。一次交接是一个指针，不是一次传输：工具什么都不复制，
也不往对话里放任何东西，它往 `<dataDir>/presented.jsonl` 追加一行 JSON。这个路由是那份清单
的读侧，也是它唯一的读者。

```json
{
  "presents": [
    {
      "ts": 1732000000000,
      "path": "reports/q3.xlsx",
      "size": 20480,
      "description": "the numbers you asked for"
    }
  ],
  "skipped": 0,
  "truncated": false
}
```

- **最新的在前。** 清单是只追加的，所以最新的交接是它的最后一行，也是 `presents` 的第一个
  元素。路由把文件倒过来，而不是按写入顺序返回：打开这份列表的操作者想看到模型刚产出的那个
  文件在最上面，而不是在 200 项深处。
- 一个条目携带清单自己的成员，其形状由工具的写者定义：`ts`（unix 毫秒）、`path`（绝对
  路径）、`size`（交接那一刻的字节数）、`description`（一行，可能为空）。
- 缺失的 `ts`、`size` 或 `description` 是一个默认值，不是损坏：`0`、`0`、`""`。同样的默认值
  也适用于类型错误的成员和负数，而条目仍然被返回：一个操作者能打开的交接，比一个严格的
  读者更有价值。`size` 是文件在被呈现时量出的值，不是它现在的值，所以一个此后被删除的文件
  仍然会出现。
- 一行不是 JSON 对象、或它的 `path` 缺失或为空，会被跳过并计入 `skipped`；每一条可读的行
  仍然被返回。一条坏行永远不是 `500`：清单是被追加的，所以写入中途崩溃可能留下一条残缺的
  行，而一个残缺的字节不得让操作者看不见其他交接。
- 最多返回 200 个条目，最多读取 8 MiB 的清单。任一上限把答案截短时，`truncated` 是 `true`，
  答案仍然是 `200`：一份不完整的列表加一个标记，胜过一个什么都不显示的错误。8 MiB 的窗口
  是文件的**尾部**，最新的行就在那里，而在窗口任一边缘被切成两半的行是被丢弃而不是计入
  `skipped`，因为那个切是读者造成的，而 `truncated` 已经说明了它。
- `note` 只在仅凭 `truncated` 无法解释这个答案时出现：清单读不了、不是普通文件、或者是被
  字节上限而不是条目数截断的。其他情况下它缺席，所以平常的答案正是上面的形状。
- **路由什么都不创建。** 为了读取而打开清单，不得创建文件或数据目录。清单缺失时是
  `{"presents":[],"skipped":0,"truncated":false}`——“什么都没被呈现过”是一个事实，不是
  错误，也不是 `404`。一个能凭空造出文件的 `GET` 会回答它自己的问题。
- 路由只从配置的数据目录回答。没有配置数据目录时 `present` 使用的回落路径
  `<workspace>/.hearth/presented.jsonl` 是另一个路径，在这里提供它会报告这台主机的数据目录
  从未记录过的交接。
- 仅 `GET`；其他任何方法都是 `405 method_not_allowed`。

---

### 4.16 `GET /api/sessions/{id}/subagents`

这个会话委派出去的子代理，附带每个子代理自己的日志所记录的时序。两次读取被连接起来：父会话
的 `subagent/catalog` 行给出形状，每个子代理的日志给出 `settledMs`、`active`、`turns` 和
`lastTurnCompleted`。路由两者都不重算——同一个数字的第二个主人，就是一个以后会不一致的
数字。

```
200 OK
{
  "session": "20261120-143012-ab12cd34",
  "count": 2,
  "subagents": [
    {
      "sessionId": "20261120-143105-0a1b2c3d",
      "name": "agent-1",
      "mode": "one-shot",
      "path": "lead/agent-1",
      "depth": 1,
      "timing": { "settledMs": 4210, "active": false, "turns": 2, "lastTurnCompleted": 1763643003300 },
      "children": []
    },
    {
      "sessionId": "20261120-143140-9f8e7d6c",
      "name": "agent-2",
      "mode": "continuable",
      "path": "lead/agent-2",
      "depth": 1,
      "diagnostic": { "kind": "unavailable", "detail": "…: no session log for this child" },
      "children": []
    }
  ]
}
```

- `count` 是整棵树里的行数，不是直接子代理的数量：“这个会话一共启动了多少子代理”才是读者
  真正在问的问题。
- `path` 相对于根，且总是以 `lead` 开头；`depth` 从根的直接子代理算起，它们深度为 `1`。
- 对于日志无法折叠的子代理，`timing` 缺席。缺席不是零：零会声称这个子代理瞬间结算了，而
  一个日志里还没有已结束轮的子代理根本没有结算。
- 当子代理存在于父会话的目录上、但它的日志无法被读取或分类时，`diagnostic` 出现：
  `corrupt`（坏 JSON、坏信封、不密集的序列、重复的 id）、`unsupported`（没有这个构建能读的
  版本的 `subagent/descriptor`）、`unavailable`（根本没有日志文件）。**这一行留着**：丢掉它
  会让一个坏掉的子代理看起来像一个从不存在的子代理，而这是操作者必须能分辨的那一件事。
- 父会话从未赋予 `name` 与 `label` 时，它们被省略。
- 有日志但没有委派的会话以 `200` 回答 `{"session":…,"count":0,"subagents":[]}`。**完全没有
  日志**的会话是 `404 no such session`，因为“这个会话不存在”和“这个会话没有子代理”是不同
  的答案，而只有调用方知道自己问的是哪一个。
- **路由什么都不创建、什么都不启动**：没有子进程、没有日志、没有目录。它是对一次运行已经
  写下的文件的读取。
- 仅 `GET`；其他任何方法都是 `405 method_not_allowed`。

---

### 4.17 `GET /api/sessions/{id}/files`

会话启动时所在工作区的一个目录。这是编辑器面板读取用来画文件树的东西；在它存在之前，从
浏览器里列一个目录的唯一办法是让模型跑一个工具，那要花掉一轮，并在日志里写下一条
`tool/call`，只为回答一个 UI 自己就能回答的问题。

工作区取自会话自己的 `session/start` 记录（`workspace`，否则 `cwd`），不从别处取：UI 必须
看到与 agent 被告知可以触碰的那棵树完全一致的东西。

```
200 OK
{ "path": "", "entries": [ { "name": "src", "kind": "dir" }, { "name": "README.md", "kind": "file", "size": 1284 } ] }
```

- 查询里的 `path` **相对于工作区**；缺席或为空意味着根。同一个值会以规范化后的形式返回
  （`"src/main.rs"`，绝不是 `"./src"`），所以客户端可以拿它做缓存的键。
- `kind` 是 `"dir"` 或 `"file"`。两者都不是的条目（套接字、管道、设备节点）被省略而不是被
  贴错标签：一个打不开的条目不是文件。
- 目录排在文件之前，然后按名字不区分大小写排序。`.git` 被省略。
- `size` 只对文件存在。
- 路径规则（§4.17–4.20 共用）：不能以 `/` 或 `\` 开头，不能有盘符或 `:`，不能有 `..` 片段，
  并且——在路径存在之后——它必须规范化到工作区内部，所以一个指到树外的符号链接也会被拒绝。
  被拒绝的路径是 `400 bad_request`。格式良好但缺席的路径是 `404 not_found`；把一个文件当
  目录来问是 `400`。
- **这里什么都不写。** 不创建目录、不触碰文件、不启动进程。编辑走 agent，因为来自 UI 的写入
  会绕过策略、审批提示和 `tool/call` 记录——这三样东西正是让一个会话可审阅的东西。
- 完全没有日志的会话是 `404 no such session`；一个已不在磁盘上的 `workspace` 是
  `404 not_found`（树被搬走了，说出来比编一棵更好）。
- 仅 `GET`；其他任何方法都是 `405 method_not_allowed`。

### 4.18 `GET /api/sessions/{id}/file`

那个工作区里的一个文件，以文本形式。

```
200 OK
{ "path": "src/main.rs", "size": 13, "text": "fn main() {}\n", "truncated": false, "binary": false, "sha256": "…64 hex…" }
```

- `?path=` 必填；缺失或为空是 `400 bad_request`。
- 最多返回 1 MiB 的文件内容。更长的文件会被截断，`"truncated": true` 会说明这一点——答案
  永远不会悄悄把文件的一部分显示得像它的全部。
- 不是 UTF-8 的文件，或含有 NUL 字节的文件，以真实的 `size` 回答
  `{"text": "", "binary": true}` 而不是报错：文件存在，UI 有权被告知这一点，而不是被告知
  它的路径错了。
- `size` 是磁盘上的大小，不是 `text` 的大小。
- `sha256` 是 `text` 里那些字节的摘要，它也是 §4.20 在保存时要回来的令牌。当答案不是整个
  文件时（`truncated` 或 `binary`），它是**空的**：一个只描述第一个兆字节的令牌，会授权一次
  替换掉没人见过的文件其余部分的保存。
- 与 §4.17 相同的路径规则和相同的 404 形状。把一个目录当文件来问是 `400 bad_request`。

### 4.19 `GET /api/sessions/{id}/diff`

那个工作区里 git 知道的改动。不带查询时，它为每个路径回答一行，带上审阅面板决定打开哪个所
需的行数统计；带 `?path=` 时，它回答那一个文件的统一 diff，因为“改了什么”这个问题还有一层
细节，把它拆成两条路由会让列表与细节在“哪些文件变了”上产生分歧。

```
200 OK
{ "files": [ { "path": "src/main.rs", "status": "modified", "additions": 12, "deletions": 3 } ] }
```

```
200 OK   (GET .../diff?path=src/main.rs)
{ "path": "src/main.rs", "status": "modified", "text": "--- a/src/main.rs\n+++ b/src/main.rs\n@@ …", "truncated": false }
```

- `status` 是 git 的词，不是这份契约发明的词：`modified`、`added`、`deleted`、`renamed`、
  `untracked`、`conflicted`。单文件形式多一个：`unchanged`，用于字节与已提交内容一致的路径。
- `additions` 与 `deletions` 来自针对 `HEAD` 的 `git diff --numstat`，对一个没有提交的仓库
  回落到工作树、然后是索引。git 数不了的文件（`untracked`）报告 `0` 和 `0`。
- `text` 是针对 `HEAD` 的 `git diff -U3 --no-color --no-ext-diff`，回落到 `--cached`。
  三个细节是这份契约自己的，它们存在是因为每一种都是“显而易见的答案”错了的情形：
  - 一个**未跟踪**的文件根本没有 `git diff` 输出，所以答案是从文件自己的字节合成出来的，
    每一行加上 `+` 前缀。没有这一点，一个全新文件的审阅者会看到一个空面板，并得出结论说
    没有任何改动；
  - 一个**二进制**文件是被描述（`Binary file <path> (<n> bytes)`）而不是被渲染，因为一张
    PNG 的 diff 是噪声的 diff；
  - `text` 上限 1 MiB，并带 `truncated: true`，与 §4.18 相同的上限和相同的标记。
- 不是 git 仓库的工作区，或一台没有 `git` 的机器，以 `200` 回答 `{"files": []}`：“这里没有
  任何东西被版本化”是关于这棵树的一个事实，不是这次请求的失败。带 `?path=` 时，同一个工作区
  对一个存在的文件回答 `{"status": "unchanged", "text": ""}`。
- 行按路径排序。hunk 是 `git diff` 自己的格式，这份契约只携带那些字节；它不重新解释它们。
- 路径规则是 §4.17 的。与 §4.17 相同的 404 形状。仅 `GET`。

### 4.20 `PUT /api/sessions/{id}/file`

保存一个编辑过的缓冲区。这是这份契约里唯一一条会改变工作区的路由，而且它是**人**的行为，
不是模型的行为：不开启轮、不升起审批提示、不记录 `tool/call`。

```
PUT /api/sessions/abc/file
{ "path": "src/main.rs", "text": "fn main() {}\n", "expectedSha256": "…64 hex…" }

200 OK
{ "path": "src/main.rs", "bytes": 13, "sha256": "…64 hex…" }
```

- `expectedSha256` 是 §4.18 随正文交回的摘要。当它存在时，文件必须仍然哈希到它。不匹配，
  或一个此后被删除的文件，是 `409 conflict`，什么都不写：一个十分钟前打开文件的编辑器，
  不得悄悄撤销一个重写了它的 agent。要创建新文件就省略它。
- `path` 与 `text` 必填；不是 JSON 的请求体，或缺了其中任一个的请求体，是
  `400 bad_request`。路径遵循 §4.17 的规则，每一种走出工作区的方式都是 `400 bad_request`。
- 服务器启动时没有编辑工作区的权限时是 403。规则是：一个以 `read-only` 启动的会话在这里
  拒绝，理由和它的工具拒绝一样：一个能写工具写不了的东西的 UI，会是一个更安静、更宽的沙箱。
- 422 用于大于 1 MiB 的 `text`，以及覆盖一个大于 1 MiB 的既有文件的保存——读者只拿到过后
  者的一段前缀，所以它对其余部分没有权威。
- 409 用于会替换一个首块含 NUL 字节的文件的保存。一个编辑器无法渲染的缓冲区不得替换没人
  见过的字节。
- 404 用于父目录缺失：因为手误创建一棵树不是“保存”的意思。
- 成功时，字节经由同一目录里的一个临时文件写入，然后改名就位，所以读者永远不会观察到写了
  一半的文件。
- **被拒绝的保存什么都不追加。** 只有成功的保存才写一个 `file/write` 事件（见
  `events-v1.md` §4.0.10），而且它是在字节落地*之后*追加的：一行描述从未落地的字节的内容，
  比一次失败的保存更糟。

### 4.21 `POST /api/sessions/{id}/revert`

把一个文件放回 git 为它提交的字节，丢弃工作树里的改动。与 §4.20 一样，这是**人**的行为：
没有轮、没有审批提示、没有 `tool/call`。

```
POST /api/sessions/abc/revert
{ "path": "src/main.rs" }

200 OK
{ "path": "src/main.rs", "bytes": 13, "sha256": "…64 hex…" }
```

- 它不接受 `text`，也不接受 `expectedSha256`。字节来自 git，所以调用方没有什么要送的，也
  没有什么可以声称知道的。
- 服务器启动时没有编辑工作区的权限时是 403，理由同 §4.20。
- 没有已提交的版本可回时是 409 `conflict`：未跟踪的文件、已加入索引但从未提交的文件，或
  不是仓库的工作区。**未跟踪的文件是被拒绝，永远不会被删除**——删掉一个人创建的文件不是
  “回到过去”，而是带着一个令人安心的名字的数据丢失。
- git 拒绝时是 500 `internal`，携带 git 自己的原话（例如 “local changes would be
  overwritten”）。那个理由是能告诉操作者的唯一有用的东西，所以它没有被丢掉。
- `path` 缺失或空白是 400，每一种走出工作区的方式（§4.17 的规则）也是 400。这个路径上的
  `GET` 是 `405`。
- 它同时恢复索引和工作树；只还原一半会让 diff 列表与磁盘产生分歧。
- **被拒绝的还原什么都不追加。** 成功的还原在字节落地*之后*写一个 `file/revert` 事件
  （`events-v1.md` §4.0.11）。它不是 `file/write`：两者都移动字节，但“一个人打了这段字”
  和“一个人把他的编辑扔掉了”是不同的两件事，问“到底发生了哪个”的读者不应被迫去猜。

---

## 5. 错误信封

每一个带正文的非 2xx 响应都恰好使用这个形状：

```json
{
  "error": {
    "code": "not_found",
    "message": "session s_01jya7c9q3r5t7v9w1x3y5z7ab does not exist",
    "details": {"sessionId": "s_01jya7c9q3r5t7v9w1x3y5z7ab"}
  }
}
```

| 成员 | 类型 | 出现 | 含义 |
| --- | --- | --- | --- |
| `error.code` | 字符串枚举 | 总是 | 稳定的机器可读错误码，`snake_case`。 |
| `error.message` | 字符串 | 总是 | 给人看的，单行，英文，无堆栈跟踪。 |
| `error.details` | 对象 | 为空时省略 | 结构化上下文；形状因错误码而异，且可能增长。 |

错误码及其 HTTP 状态：

| `code` | 状态 | 何时 |
| --- | --- | --- |
| `bad_request` | 400 | JSON 格式错误、成员缺失/无效、查询值非法。 |
| `protocol_required` | 400 | `/api/*` 请求缺少 `X-Hearth-Protocol`。 |
| `protocol_unsupported` | 400 | 头存在，但不是服务器讲的版本。 |
| `unauthorized` | 401 | 令牌缺失/错误。 |
| `forbidden` | 403 | 非回环地址的对端，或 POST 上坏的 `Origin`。 |
| `not_found` | 404 | 未知的会话、审批或路径。 |
| `method_not_allowed` | 405 | 已知路径、方法不对；响应携带 `Allow`。 |
| `conflict` | 409 | 状态不允许该动作（已结束的会话、已回答的审批）。 |
| `unsupported_media_type` | 415 | 请求体不是 UTF-8 JSON。 |
| `unprocessable_entity` | 422 | 合法 JSON，但语义上不可用。 |
| `precondition_required` | 428 | 缺少 `Idempotency-Key`。 |
| `rate_limited` | 429 | 请求太多；`Retry-After` 以秒计。 |
| `internal` | 500 | 意外失败。永不泄漏内部信息。 |
| `log_read_failed` | 500 | 日志读不了（损坏或不可读的会话文件）；`details` 指出哪里失败了。 |
| `unavailable` | 503 | 正在启动或正在关闭。 |

> **实现状态——一处未了的偏差。**
>
> 1. `internal`：实际发布的取值是 **`internal_error`**（`protocol::code::INTERNAL`），而
>    JSON 编码失败的兜底也使用字面量 `"internal_error"`。上面的契约文本说的是 `internal`；
>    代码说的是 `internal_error`，而改名会打坏每一个按已发布行为写的客户端，所以表里的
>    取值才是客户端实际会收到的。
> 2. `rate_limited` / `429`：**在 W6 里实现了。** 只有
>    `POST /api/sessions/{id}/messages` 受限。预算是按*会话*算的（不是按对端：监听器只绑
>    回环地址，而一个浏览器标签页就是一个会话），默认是一个固定窗口 **120 轮每 60 s**
>    （`hearth_server::RateLimit::default()`）。响应携带 `Retry-After: <seconds>`，向上取整
>    且永不低于 `1`，外加 `details: {sessionId, limit, windowSeconds, retryAfterSeconds}`。
>    被拒绝的请求**什么都不**追加——计数发生在状态检查之后、第一个事件之前，所以客户端可以
>    靠读日志区分“被拒绝”与“被接受”。因其他理由（`409`、`503`）被拒绝的请求不消耗预算，
>    所以重试一个坏请求仍然会显示它的错误。`RateLimit::disabled()` 是给那些自己驱动服务器
>    的调用方（e2e 脚手架、基准测试）用的，它们知道自己这边的速率。
>
> `method_not_allowed` 曾经是第 1 号偏差：`405` 的回答曾经发的是 `bad_request`。在 M7 修好
> 了——`hearth_protocol::code` 现在有这个常量，三个地方都在用它。
>
> §2 与 §3 很晚才补上两个浏览器机制：外壳为打印出来的 `?token=` URL 设置 `hearth_token`
> cookie，而 WebSocket 握手可以用 `hearth.v1` 子协议而不是 `X-Hearth-Protocol` 来表明它的
> 版本。两者都不是线协议偏差——两个都是增量的，而不带任一信号的握手仍然是 `400`。它们存在
> 是因为更早的文本描述了一个任何浏览器都做不出来的浏览器流程：在这些落地之前，UI 的流在
> 真实的 Edge 窗口上对着 `401` 永远重试。
>
> 上面的 `log_read_failed` 直到 M7 才出现在这张表里；服务器在 `500` 时发出它，而契约当时
> 没有给它名字。

客户端必须按 `error.code` 分支，绝不按 `error.message`。可以新增错误码；未知的错误码必须按
它们的 HTTP 状态类别处理。

---

## 6. 兼容性与版本管理

- 协议 **1** 只做增量。新的路由、新的响应成员、新的事件类型和新的 `error.code` 取值都是
  兼容的，不提升版本号。
- 破坏性改动（删除/重命名成员、改变类型或语义、改变状态码、丢掉一个路由）需要
  `X-Hearth-Protocol: 2`，以及一个有文档记载的重叠窗口，在窗口里服务器同时接受两个头值。
- 响应可以随时增加成员；客户端必须忽略未知成员。客户端不得因为未知的事件 `type` 而失败
  （它是透传的）。
- `/healthz` 里的 `version` 字段是构建版本（`0.0.0-dev` 那种）而**不是**协议版本；不要用
  它来开关特性。
- 规范在它明确的地方是权威，但最终的实现加上它的一致性测试才是真相（`AGENTS.md`）：当
  它们不一致时，在修代码的同一次改动里修这份文档。

### 这份契约怎么被验证

事件格式有这份文档要求的那套数据驱动用例（`tests/conformance/cases/events/*.json`）；HTTP
面没有。今天存在的东西，以及它们在哪：

1. `internal/server/*_test.go` ——每个路由用 `httptest` 驱动的请求/响应测试（`api_test.go`、
   `sessions_test.go`、`messages_test.go`、`approvals_test.go`、`turncontrol_test.go`、
   `static_test.go`、`intake_test.go`），包括 `405` + `Allow` 的用例和幂等重放的用例。
2. `internal/server/wsclient_test.go` ——一个真实的 WebSocket 客户端跑在手写的升级之上，
   断言帧顺序（snapshot → events → deltas）。
3. 经由 API 的 fixture 重放是被间接覆盖的：events 路由返回原始的日志行（该行的
   `json.RawMessage`），而 `internal/session` 的测试已经钉住了那些字节。
4. 幂等测试是存在的：带同一个 `Idempotency-Key` 的第二次 `POST …/messages` 什么都不追加，
   并回答 `deduplicated: true`（`messages_test.go`）。

相对于上面的计划仍然缺什么：`tests/conformance/cases/http/` 不存在，所以没有语言中立的 HTTP
用例语料，也没有 golden 的 WebSocket 记录 fixture 文件。`internal/server/README.md` 记录了
这份契约留待决定的实现决策。