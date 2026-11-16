---
dir: crates/session
layer: L2
role: session log store, projection, decision watching, and fork
public: [Store, Options, Projection, Snapshot, DecisionWatcher, Decision, ForkOptions, ForkResult, fork]
may_import: [forge-core, forge-config]
must_not: [forge-llm, forge-tool, forge-protocol, forge-server, forge-cli]
gate: make arch
---

# forge-session

## 做什么

`forge-session` 拥有会话日志这一唯一事实来源：`<dataDir>/sessions/<id>.jsonl`。
它负责按行追加事件、在打开时修复被中断写入留下的半行、按序回读、把日志折叠成
`Snapshot`、在日志增长时轮询审批决定，以及把完整轮次之前的前缀复制成一份新会话。

日志是只追加的：本 crate 从不重写已有记录。遇到不认识的 `v`（事件格式版本）时，
`Store::open` 直接返回 `UnsupportedVersion`，不截断、不追加、不改动文件——宁可拒绝
一个未来版本，也不能把它当旧格式写坏。compaction 事件由 projection 折叠为可见摘要，
但本波不提供生成 compaction 的写入器。

## 公开面

- `Store`：`open(path, Options)`, `append(&Event) -> Result<u64>`, `read_from(after_seq, f)`,
  `last_seq()`, `path()`, `sync()`, `close()`。`append` 补齐 `v/seq/ts/id` 并按
  `Options::sync_every` 落盘；并发追加只会得到连续序号。
- `Options { sync_every, no_fsync }`：`sync_every` 为 0 或 1 表示每次追加都 fsync。
- `Projection` / `Snapshot`：`apply(&Event)` 与 `snapshot()`；未知事件类型计入
  `counters.skipped`，重复序号计入 `counters.duplicates`，已知类型的坏 payload 返回
  `Invalid`。`Snapshot` 可直接序列化为契约 JSON。
- `DecisionWatcher`：`new(path)`, `poll(call_id) -> Result<Option<String>>`,
  `poll_full(call_id) -> Result<Option<Decision>>`, `close()`。
  只读取上一次完整行之后的新字节；半行留到下次重读。
- `ForkOptions` / `ForkResult` / `fork(&ForkOptions)`：默认切在最后一个 `turn/end`，
  也可指定 `at` 序号；子日志序号从 1 紧密排列、事件 ID 全新、payload 原样保留。

## 依赖与边界

只依赖 `forge-core`（事件包络、错误种类、ID、canonical JSON）和标准库；`forge-config`
是允许的同层依赖，目前不需要。不得导入 L2 以上的 crate，也不得知道 LLM、工具策略或
HTTP 的存在。所有文件 I/O 都限定在打开的那一份日志和 fork 目标文件内，不遍历目录、
不读取配置、不写日志之外的状态。

## 不要做

- 不要重写或压缩已有日志行，也不要在版本不匹配时“修复”文件。
- 不要在库代码里对 I/O 或解析结果使用 `unwrap()`/`expect()`。
- 不要引入 async 运行时、`anyhow`/`thiserror` 或新的第三方依赖。
- 不要把未知事件类型当作错误：未来事件是数据，跳过并计数即可。
- 不要在 `turn/end` 时清理 pending approval：黄金投影要求它保持 pending，直到
  `approval/decision` 显式关闭。
