---
dir: crates/plugin
layer: L2
role: plugin host — child processes speaking JSON-RPC 2.0 over stdio, contributing tools, prompts and hooks
public:
  - Host
  - Loaded
  - Manifest
  - Runtime
  - Connection
  - Stopped
  - Error
  - Kind
  - Result
  - Handshake
  - Capabilities
  - ToolSpec
  - ToolOutput
  - PromptSpec
  - Request
  - Message
  - is_valid_name
  - is_hook_phase
  - PROTOCOL_VERSION
  - DEFAULT_MAX_BYTES
  - DEFAULT_TIMEOUT_MS
  - HOOK_PHASES
  - CLOSE_GRACE
may_import: [forge-core, forge-mcp]
must_not:
  - spawn anything from Host::start or discover — a listing is not consent to execute
  - accept a reply whose id is not the id of the call in flight
  - leave a child running after a timeout, an initialize failure, or a drop
  - read a command line from plugin.json instead of deriving it from runtime plus entry
  - register plugin tools directly into forge-tool — the CLI wires names and schemas, calls come back here
gate: cargo test -p forge-plugin --offline
---

## 做什么

把一个外部进程变成 harness 的能力来源。插件在自己选的语言里实现 `tools/list`、
`tools/call`、`prompts/list`、`prompts/get`、`hooks/run` 和 `shutdown`，宿主只讲
JSON-RPC 2.0，一行一个对象，UTF-8，`\n` 结尾。协议写在 `PROTOCOL.md`，那份文档是
契约：照它写的插件必须能跑，参考实现 `tests/reference-node/index.js` 就是照它写的。

插件不是 MCP 服务器。MCP 只暴露工具，插件还能交 prompt（系统提示片段）、hook
（回合前后跑，可以改 payload）和工具装饰器；生命周期也由宿主驱动（`load`/`unload`），
不是一个自己要加入的会话。差别在能力，不在线上格式。

`Host::start` 只记下根目录，`discover` 只读 `plugin.json`。两者都不 spawn：列目录不是
执行授权，和 `forge-skill` 只读 `SKILL.md` 是同一条纪律，而这里的产物是程序。`load`
才起子进程并做 `initialize`，`unload` 发 `shutdown`、关 stdin、等两秒、再杀。

子进程死了不靠看门狗发现：下一次调用在写之前 `try_wait`，管道断了就报
`child_exited`，并把最后一行 stderr 附在消息里。没人调用的插件不需要被盯着，而为
「发现故障」存在的线程本身也会故障。

## 公开面

`Host`：`start`/`discover`/`load`/`unload`/`unload_all`/`loaded_names`/`is_loaded`/
`handshake`/`capabilities`/`tools`/`call_tool`/`prompts`/`prompt_text`/`run_hook`/
`all_tools`/`command_line`，以及 `with_logger`/`set_timeout`/`set_max_bytes`/
`set_protocol_version` 和三个对应的读取器。

`Manifest`+`Runtime`（`read`、`command_line`、`is_valid_name`）、`Connection`+`Stopped`
（自己搭测试用）、`Handshake`/`Capabilities`/`ToolSpec`/`ToolOutput`/`PromptSpec`/
`Request`/`Message`、`Error`+`Kind`（`Kind::code()` 是 CLI 打印的稳定字符串）、
`is_hook_phase`、`PROTOCOL_VERSION`/`DEFAULT_MAX_BYTES`/`DEFAULT_TIMEOUT_MS`/
`HOOK_PHASES`/`CLOSE_GRACE`。

失败是一个封闭词表，每个码指向不同的人：`unknown_plugin`、`bad_manifest`、
`unsupported_runtime`、`spawn_failed`、`not_initialized`、`protocol_error`、`timeout`、
`child_exited`、`too_large`、`bad_response`，外加 `io`。加一个码是一次带测试的文档
变更，不是调用点随手写的字符串。

## 依赖与边界

只依赖 `forge-mcp`（`Logger`/`NoopLogger`，宿主必须接受调用方给的日志汇）和
`serde_json`（线上格式）。层位置 L2，向下依赖 L1 的 `forge-mcp` 是合法的。

`forge-mcp` 的传输（`stdio`/`jsonrpc`/`Conn`）是 `pub(crate)`，公开面只有 MCP 形状的
`dial`/`Client`（`initialize`/`tools`/`call`/`close`）和 MCP 的版本串。要复用它，要么把
它的隐私面撕开，要么把插件协议掰成 MCP 客户端——后者等于让 L1 带上插件知识，两边都
比在这里重写一份 400 行的 stdio 传输糟。所以：日志汇复用，传输自建，结构照抄它已经
证明过的那套（读线程 + stderr 泵 + 退出回收，全部可 join）。

不依赖 `forge-core`：它的 `ErrorKind` 是事件日志和 HTTP 信封已经在说的封闭词表，插件
失败在里面没有位置（没有哪个事件类型承载「插件超时」）。等真需要共享 id 类型再接。

**不要做**：不要把插件工具直接注册进 `forge-tool`——那会让 `forge-tool` 依赖宿主
生命周期。CLI 的接线只把 `<plugin>.<tool>` 名字和 schema 交给注册表，调用时回到
`Host`，见 `D:\forge-rs-target\p3-needs-forge-plugin.md`。

## 不要做

- 不要在 `discover`/`start` 里 spawn：列目录不是执行授权，和 `forge-skill` 只读
  `SKILL.md` 是同一条纪律，而这里的产物是程序，更甚。
- 不要接受 id 不匹配的回复。插件若在同一根管道上并发发多个回复，接受「差不多的
  id」等于把一次调用的结果交给另一次调用。
- 不要在超时后等待：杀掉并报 `timeout`。一个忽略过 deadline 的子进程会再忽略一次，
  而调用方的线程被卡住比丢掉它在飞的中间状态更糟。
- 不要把 `plugin.json` 里的字符串当命令行用。`runtime` 是封闭集合，命令由它推导，
  否则「改一个插件」等于「能执行一条命令」。
- 不要为了「省一次往返」去改协议或改边界。§2.3 测到的是空结果 80.6 µs、4 KB
  118.3 µs、64 KB 343.7 µs，相对一次 1,203 ms 的模型调用是 0.007%——一次往返从来不是
  瓶颈，让插件变难写才是代价。反过来也别把它当免费：一千次就是 80 ms，而且 §2.3 明确
  说了热循环里几千次小调用该批量化或改用 Rust 写，不是搬动宿主。
- 不要在 `Drop` 之外留下子进程；也不要开无法 join 的线程。
