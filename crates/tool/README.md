---
dir: crates/tool
layer: L2
role: registry, executor, policy, approvals and the builtin tools
public: [Tool, Call, Result, Limits, DefaultLimits, Registry, Executor, Policy, Rule, Rules, Decision, Approver, ApprovalRequest, AllowAll, Dangerous, builtin, Options, names, WebFetchTool, WebSearchTool, SearchConfig, WorktreeTool, PresentTool, ReadImageTool, GoalEndTool, GOAL_META_KEY, MAX_IMAGE_BYTES, ALLOWED_MEDIA_TYPES]
may_import: [hearth-core, hearth-exec, hearth-llm, hearth-net]
must_not:
  - import a crate from a higher layer
  - write event-log or approval events; session owns the log
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

工具系统：`Tool` trait、注册表、一个在真正执行之前先落实策略与审批的执行器，以及内建工具。有十个永远注册（`fs_read`、`fs_write`、`fs_edit`、`fs_glob`、`fs_grep`、`read_image`、`shell`、`todo_write`、`worktree`、`present`）；平台工具只在对应能力被配置时才出现——`web_fetch` 需要一个网络客户端，`web_search` 还要再加一个端点。`goal_end` 只在设置了 `Options.goal` 时注册，因为它的唯一读者就是开启这一轮的那个循环。执行器从不写事件；循环从 `Call` 和 `Result` 值里记录这次调用与结果。`Result.parts` 是唯一不进日志的字段：在每一种线格式上，工具结果都是一条文本消息，所以产出像素的工具（`read_image`）把像素放在这里返回，由循环把它们作为后续的 user 消息转发出去。

## 公开面

`Tool`、`Call`、`Result`、`Limits`、`DefaultLimits`、`Registry`、`Executor`、`ExecutorOptions`、`Policy`、`PolicyFunc`、`Rule`、`Rules`、`Decision`、`Approver`、`ApprovalRequest`、`AllowAll`、路径助手、截断助手、`Dangerous`，以及 `builtin` 模块的 `all(&Options)`、`names(&Options)`、`Options`、四个平台工具和 `SearchConfig`。

## 依赖与边界

不要让执行器自己决定策略。策略是数据（`Rule`），由调用方合并进来；一个把 shell 硬编码成 `ask` 的执行器，离开 shell 就没法测试。shell 与 worktree 命令只用 `hearth-exec`。文件系统路径一律在调用方给出的 workspace 下解析，绝不从进程 CWD 推断。

网络访问是 `hearth-net` 的职责，不是本 crate 的：一个自己伸手够网的工具，等于第二套 TLS 栈，还自带一套关于信任根、deadline 和重定向的看法。需要网络的工具接收一个 `Arc<hearth_net::Client>`，调用方没有它时这个工具就不注册，于是「没有网络」在工具列表里就能看见，而不是一次一次调用地失败。

## 不要做

- 不要导入 `hearth-session`、`hearth-agent`、`hearth-server` 或任何更高层。
- 不要在工具真正允许执行之前先跑它；需要审批时，也要先拿到审批人的同意。
- 不要产出无界输出；每个工具结果都要经过 `truncate`。
- 不要在没有对应能力时注册平台工具。一个总是失败的工具比一个不存在的工具更糟，因为模型会一直去试它。