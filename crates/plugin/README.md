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
may_import: [hearth-core, hearth-mcp]
must_not:
  - spawn anything from Host::start or discover — a listing is not consent to execute
  - accept a reply whose id is not the id of the call in flight
  - leave a child running after a timeout, an initialize failure, or a drop
  - read a command line from plugin.json instead of deriving it from runtime plus entry
  - register plugin tools directly into hearth-tool — the CLI wires names and schemas, calls come back here
gate: cargo test -p hearth-plugin --offline
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
执行授权，和 `hearth-skill` 只读 `SKILL.md` 是同一条纪律，而这里的产物是程序。`load`
才起子进程并做 `initialize`，`unload` 发 `shutdown`、关 stdin、等两秒、再杀。

一个宿主可以读**多个根**（`add_root`）：插件要么被手工放进 `<dataDir>/plugins`，要么被
`hearth market install` 解到 `<dataDir>/market/bundles`，两条路装出来的都是插件，宿主必须
都看得见，否则 market 那句"install plugin bundles"就止于一个没人加载的目录。CLI 的
`host_of` 与 chat/web 接线都把 market 的 bundles 目录加成第二个根。**同名出现在两个根里
会被拒绝而不是按顺序挑一个**：两份目录可能是不同的代码，跑错那份比起不来更糟——discover
把两份都撤下、报一条点名两条路径的冲突，`load` 随之拒绝。缺失的第二个根不是错误（多数机器
只走其中一条路）。

子进程死了不靠看门狗发现：下一次调用在写之前 `try_wait`，管道断了就报
`child_exited`，并把最后一行 stderr 附在消息里。没人调用的插件不需要被盯着，而为
「发现故障」存在的线程本身也会故障。

## 公开面

`Host`：`start`/`add_root`/`root`/`roots`/`discover`/`load`/`unload`/`unload_all`/`loaded_names`/`is_loaded`/
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

### 复跑那次测量

`examples/ipc_cost.rs` 是 §2.3 那组数字的来源，留在仓库里是为了让「跨进程很便宜」
这句话可以被推翻而不只是被引用：

```text
cargo run --release -p hearth-plugin --example ipc_cost --offline
```

它起一个临时 Node 插件，每档载荷做 2,000 次 `tools/call`（丢前 100 次预热），
同时打印一行**进程内对照**——同样的 `serde_json` 编解码但不过管道。两行相减才是
边界的价钱。它不写基线、不让构建失败：这是给人看的测量，不是 `tools/bench` 那六项
门禁，动它会连带动冻结的基线格式。

先 `cargo build --release -p hearth-plugin --bin hearth-plugin-native` 的话，它还会
追加一段**原生插件对照**（`runtime: "exec"`，20 次 spawn 采样）。实测原生启动
p50 约 8.5–9.8 ms、Node 约 43–46 ms，约 5 倍；per-call 地板约减半。两者都不到一次
模型调用的 4%，所以语言不是按性能选的——见 ADR-0003 §2.6。

### 内存是另一根轴，别拿延迟的数字回答它

`examples/memory_cost.rs` 是 §2.7 那组数字的来源。上面全是**时间**，而内存是这次
重写真正的卖点，所以它必须单独量：

```text
cargo build --release -p hearth-plugin --bins --offline
cargo run --release -p hearth-plugin --example memory_cost --offline -- 120 3
```

第一个参数是持有秒数，第二个是同时挂几个 Node 插件（用来量**边际成本**）。它打印
自己的 pid 并挂住子进程，你在另一个 shell 里采样它和它的孩子——三个 runtime 用
**同一种方法**量，比较才公平。工作集含共享的映像与 DLL 页，**私有字节才是真实增量
成本**，两个都记。

实测空插件：Node **55.9 MB 工作集 / 19.1 MB 私有**、Python 12.6 / 7.5、原生
4.4 / **0.7**，而宿主自己 6.1 / 1.8。Node 自报 `heapTotal` 只有 6 MB——**另外 50 MB
是「作为 Node」的地板价**，跟作者写了什么无关。三个 Node 插件是 167.7 / 57.5，
**边际成本等于首次成本，一点不共享**。

所以「多插件共用一个解释器」值得量而不是推断：一个 Node 进程装 3 个实例实测
55.6 / 19.7，省 112 MB 工作集 / 37.8 MB 私有。**但共用的代价是崩溃与卡死不再隔离**，
且协议要加 plugin id——那是设计决定，不是顺手能做的优化。

### 三种 runtime 都被真跑过

`node`/`python` 由 `tests/reference-node/` 与十行 Python 冒烟覆盖；`exec` 由
`src/bin/hearth-plugin-native.rs` 覆盖——那是一个能跑的最小原生插件（echo/fail/fill），
`tests/host_it.rs` 里三个用例走它验证握手、两种失败形状、以及 discover 不 spawn。
在它存在之前 `exec` 只被测过「字符串能解析」，argv 少一段也不会有测试发现。

`node` 还有第四个用例 `a_typescript_entry_runs_with_no_build_step`：`.ts` 入口零构建
跑通（Node ≥ 23.6 擦类型），Node 缺失或过老时走 `skip()` 而不是假 OK。

## 依赖与边界

只依赖 `hearth-mcp`（`Logger`/`NoopLogger`，宿主必须接受调用方给的日志汇）和
`serde_json`（线上格式）。层位置 L2，向下依赖 L1 的 `hearth-mcp` 是合法的。

`hearth-mcp` 的传输（`stdio`/`jsonrpc`/`Conn`）是 `pub(crate)`，公开面只有 MCP 形状的
`dial`/`Client`（`initialize`/`tools`/`call`/`close`）和 MCP 的版本串。要复用它，要么把
它的隐私面撕开，要么把插件协议掰成 MCP 客户端——后者等于让 L1 带上插件知识，两边都
比在这里重写一份 400 行的 stdio 传输糟。所以：日志汇复用，传输自建，结构照抄它已经
证明过的那套（读线程 + stderr 泵 + 退出回收，全部可 join）。

不依赖 `hearth-core`：它的 `ErrorKind` 是事件日志和 HTTP 信封已经在说的封闭词表，插件
失败在里面没有位置（没有哪个事件类型承载「插件超时」）。等真需要共享 id 类型再接。

**不要做**：不要把插件工具直接注册进 `hearth-tool`——那会让 `hearth-tool` 依赖宿主
生命周期。CLI 的接线只把 `<plugin>.<tool>` 名字和 schema 交给注册表，调用时回到
`Host`，见 `crates/cli/src/bridges.rs` 的 `PluginTool`。

## 不要做

- 不要在 `discover`/`start` 里 spawn：列目录不是执行授权，和 `hearth-skill` 只读
  `SKILL.md` 是同一条纪律，而这里的产物是程序，更甚。
- 不要接受 id 不匹配的回复。插件若在同一根管道上并发发多个回复，接受「差不多的
  id」等于把一次调用的结果交给另一次调用。
- 不要在超时后等待：杀掉并报 `timeout`。一个忽略过 deadline 的子进程会再忽略一次，
  而调用方的线程被卡住比丢掉它在飞的中间状态更糟。
- 不要把 `plugin.json` 里的字符串当命令行用。`runtime` 是封闭集合，命令由它推导，
  否则「改一个插件」等于「能执行一条命令」。
- 不要在 `plugin doctor` 里硬编探测名单。它曾经无条件探 `node` 与 `python`：只装了
  原生插件的机器上会打印两个谁也不需要的 runtime，而在没装解释器的机器上会报两行
  missing，同时对真正被依赖的那个二进制一个字都不说。探测集合必须从 manifest 推导，
  `exec` 查 entry 文件在不在——**不能拿 `--version` 去探插件**，那不在协议里。
- 不要为了「省一次往返」去改协议或改边界。§2.3 在真栈上测到的是空结果 37 µs、4 KiB
  56 µs、64 KiB 231 µs，相对一次 1,203 ms 的模型调用是 0.003%——一次往返从来不是
  瓶颈，让插件变难写才是代价。反过来也别把它当免费：跟进程内调用比它贵 10–30 倍，
  而且 §2.3 明确说了热循环里几千次小调用该批量化或改用 Rust 写，不是搬动宿主。
- 不要在 `Drop` 之外留下子进程；也不要开无法 join 的线程。
- 不要按调用 spawn。真正贵的是启动：spawn + handshake 实测 36–45 ms，约单次调用的
  1,000 倍。`chat` 整轮持一个 `Host`、只有 `load` 起进程，这条纪律才是保住性能的那条；
  传输方式不是。
- 不要拿延迟的数字回答内存问题。它们是两根轴：一次调用 37 µs 可以忽略，而一个 Node
  插件进程实测 19.1 MB 私有、**第二个又是 19.1 MB**（§2.7）。宿主自己 1.8 MB，所以
  「hearth 只用 6 MB」这句话只在**没启用插件**时成立，引用它必须带这个限定词。
- 不要为了省内存把几个插件塞进一个进程，除非同时接受崩溃与卡死不再隔离、并给协议加上
  plugin id。实测能省 112 MB 工作集（3 个插件），但今天「崩一个只死一个」是遏制故事的
  一部分，那是设计决定而不是优化。
