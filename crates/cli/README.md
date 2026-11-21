---
dir: crates/cli
layer: L3
role: the hearth binary: argument parsing, chat/web/doctor/index/import/session/goal/schedule/plugin/expert/team/market, usage, version
public: [run, reorder_flags, command_after_flags, VERSION, COMMIT, USAGE, EXIT_OK, EXIT_FAILURE, EXIT_USAGE]
may_import: [hearth-core, hearth-exec, hearth-config, hearth-context, hearth-sandbox, hearth-llm, hearth-session, hearth-agent, hearth-tool, hearth-server, hearth-protocol, hearth-store, hearth-import, hearth-mcp, hearth-skill, hearth-subagent, hearth-net, hearth-goal, hearth-schedule, hearth-market, hearth-plugin, hearth-experts, hearth-team]
must_not: []
gate: make arch
---

# hearth-cli

## 做什么

`hearth-cli` 提供 `hearth` 可执行文件。它把参数解析、帮助文本、版本快路径、`chat`/`web`/`doctor`/
`index`/`import`/`session` 集中在一个可测试的 `run(args, stdout, stderr)` 入口里，`main.rs` 只负责
取进程参数和设置退出码。`--version` 不读取配置：配置文件损坏时版本仍必须可用。`chat` 在 stdout 只
输出回答，诊断走 stderr；流式输出在步号变化时插入换行（Go D2 修复）。`web` 在 loopback 起本地
HTTP/WS 服务并把带 token 的 URL 打到 stderr；所有日志追加由 server 拥有，runner 只跑模型循环并通过
轮询日志回答审批。`doctor` 按固定顺序执行十项只读检查，无法执行的检查报告 `SKIP`，从不伪装成 `OK`。
`index` 只读地查询 SQLite 索引（缺索引时报错并指向 `--rebuild`，绝不静默建空索引）；`import` 把
旧格式会话只读地映射成 v1 日志（绝不写入源目录）；`session fork` 在指定 seq 处开一条新会话。

## 公开面

- `run(&[String], &mut dyn Write, &mut dyn Write) -> i32`：完整 CLI 分派；返回 0/1/2 退出码。
- `reorder_flags(&[String], &[&str]) -> Vec<String>`：把 flag 移到位置参数之前，供
  `chat`/`import`/`index`/`session` 共用；`--` 之后的词保持位置参数顺序。
- `VERSION`, `COMMIT`, `USAGE`：版本串与冻结契约文本。
- `write_json`, `one_line`, `wants_help`, `default_data_dir`, `new_session_id`, `civil_from_epoch`,
  `epoch_stamp`：子命令共用的输出、时间与会话 id 助手（三处命令各写一份就会漂移）。
- `doctor` 子模块：`Report`/`Check` 的文本表格和稳定 JSON（`rustVersion` 键）。
- `chat` 子模块：`hearth chat [flags] [prompt]` 单轮运行，流式 stdout、诊断 stderr、JSON 报告；
  `is_prompt_invocation` 供裸 prompt 简写分派。`RecordingApprover` 在审批前写 `approval/request`、
  在真实回答后写 `approval/decision`（失败用 `cancelled`/`timeout`/`error`，绝不用审批人名字）。
- `web` 子模块：`hearth web [flags]` 在 loopback 起浏览器 UI；server 拥有事件追加，runner 用
  `DecisionWatcher` 轮询浏览器回答。
- `wiring` 子模块：把一个 registry、一条策略和三种外部桥（MCP 服务器、技能、子代理）组装进
  `WiredTools`；`hearth chat` 与 `hearth web` 都走这里，所以两个面不会对"哪些工具存在"各说各话。
- `indexcmd`/`importcmd`/`sessioncmd`：三条子命令的 `run` 与它们的 `USAGE`。
- `capabilities` 模块：`report(&Config, &Facts) -> hearth_server::Report` 把"这个进程此刻能提供
  什么"整理成一份只读快照，供 `GET /api/capabilities` 与浏览器面板使用。它只发现、不启动：
  插件走 `Host::discover`，名册与 market 走磁盘扫描，MCP 与技能读配置。做不出结论的项落一行
  诊断而不是让整个报告失败——报告缺席比报告说"没配置"更误导人。
- `attachments` 模块：`parts_hook(&Path)` 把日志里的附件引用读成 `ContentPart`，让图片进模型
  请求；读不到主体就跳过（少给一个附件不算失败），日志里始终只有引用。
- `context_tools` 模块：`compress`/`decompress`/`search_context` 三个工具与 `hidden_hook(&Path)`。
  它们**从不写日志**——`compress` 只把要追加的 payload 放进 `result.meta.contextBlock`，由
  循环写入，因为日志只有一个写者；`hidden_hook` 每轮读一次块区间，下一轮请求就少付那部分
  token。放在 CLI 而不是 `hearth-context` 里，是因为 archcheck 禁同层边，而这个 crate 已经依赖
  全部更低层。

## 依赖与边界

可依赖所有更低层 crate：`hearth-core`、`hearth-config`、`hearth-sandbox`、`hearth-llm`、
`hearth-session`、`hearth-agent`、`hearth-tool`、`hearth-server`、`hearth-protocol`，以及能力层的
`hearth-store`、`hearth-import`、`hearth-mcp`、`hearth-skill`、`hearth-subagent`、`hearth-net`、
`hearth-goal`、`hearth-schedule`、`hearth-market`、`hearth-plugin`、`hearth-experts`、`hearth-team`。
`ctrlc = "3.4"`
用于 `hearth web` 的优雅关停与第二次 Ctrl+C 退出 130（离线缓存已有，避免手写平台信号代码）。
不得被其他 crate 导入（`main.rs` 是唯一入口），也不得把 clap 之类的解析器或 async 运行时带进来——
参数解析是手写的，与 Go 版保持一致。

## 不要做

- 不要在 `--version` 路径上读取配置；快路径必须保持快且可用。
- 不要在 `doctor` 里写系统状态；唯一允许的写是数据目录里的临时探针，且必须删除。
- 不要增删 `help` 的九行契约文本：`USAGE` 必须与 `docs/contracts/cli.md` §2.2 逐字节一致。
- 不要在库代码里对 I/O 或解析结果使用 `unwrap()`/`expect()`（`#[cfg(test)]` 内允许）。
- 不要在 `index` 里顺手 `Store::open` 一个不存在的索引：那会把"打错路径"变成"看起来是空的项目"。
- 不要在 `session fork` 时创建空的 sessions 目录：父日志不存在时创建它，只会掩盖打错的 id。