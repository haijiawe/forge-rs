---
dir: crates/cli
layer: L3
role: the forge binary: argument parsing, chat/web/doctor/index/import/session, usage, version
public: [run, reorder_flags, VERSION, COMMIT, USAGE]
may_import: [forge-core, forge-config, forge-sandbox, forge-llm, forge-session, forge-agent, forge-tool, forge-server, forge-protocol, forge-store, forge-import, forge-mcp, forge-skill, forge-subagent]
must_not: []
gate: make arch
---

# forge-cli

## 做什么

`forge-cli` 提供 `forge` 可执行文件。它把参数解析、帮助文本、版本快路径、`chat`/`web`/`doctor`/
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
- `chat` 子模块：`forge chat [flags] [prompt]` 单轮运行，流式 stdout、诊断 stderr、JSON 报告；
  `is_prompt_invocation` 供裸 prompt 简写分派。`RecordingApprover` 在审批前写 `approval/request`、
  在真实回答后写 `approval/decision`（失败用 `cancelled`/`timeout`/`error`，绝不用审批人名字）。
- `web` 子模块：`forge web [flags]` 在 loopback 起浏览器 UI；server 拥有事件追加，runner 用
  `DecisionWatcher` 轮询浏览器回答。
- `wiring` 子模块：把一个 registry、一条策略和三种外部桥（MCP 服务器、技能、子代理）组装进
  `WiredTools`；`forge chat` 与 `forge web` 都走这里，所以两个面不会对"哪些工具存在"各说各话。
- `indexcmd`/`importcmd`/`sessioncmd`：三条子命令的 `run` 与它们的 `USAGE`。

## 依赖与边界

可依赖所有更低层 crate：`forge-core`、`forge-config`、`forge-sandbox`、`forge-llm`、
`forge-session`、`forge-agent`、`forge-tool`、`forge-server`、`forge-protocol`，以及能力层的
`forge-store`、`forge-import`、`forge-mcp`、`forge-skill`、`forge-subagent`。`ctrlc = "3.4"`
用于 `forge web` 的优雅关停与第二次 Ctrl+C 退出 130（离线缓存已有，避免手写平台信号代码）。
不得被其他 crate 导入（`main.rs` 是唯一入口），也不得把 clap 之类的解析器或 async 运行时带进来——
参数解析是手写的，与 Go 版保持一致。

## 不要做

- 不要在 `--version` 路径上读取配置；快路径必须保持快且可用。
- 不要在 `doctor` 里写系统状态；唯一允许的写是数据目录里的临时探针，且必须删除。
- 不要增删 `help` 的九行契约文本：`USAGE` 必须与 `docs/contracts/cli.md` §2.2 逐字节一致。
- 不要在库代码里对 I/O 或解析结果使用 `unwrap()`/`expect()`（`#[cfg(test)]` 内允许）。
- 不要在 `index` 里顺手 `Store::open` 一个不存在的索引：那会把"打错路径"变成"看起来是空的项目"。
- 不要在 `session fork` 时创建空的 sessions 目录：父日志不存在时创建它，只会掩盖打错的 id。