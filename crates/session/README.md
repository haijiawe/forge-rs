---
dir: crates/session
layer: L2
role: session log store, projection, compaction, decision watching, and fork
public: [Store, Options, Projection, Snapshot, DecisionWatcher, Decision, ForkOptions, ForkResult, fork, CompactPlan, Summarizer, TruncatingSummarizer, plan_compaction, compact, compact_log, last_compacted_span, DEFAULT_KEEP_TURNS, DEFAULT_SUMMARY_BYTES]
may_import: [hearth-core, hearth-config]
must_not: [hearth-llm, hearth-tool, hearth-protocol, hearth-server, hearth-cli]
gate: make arch
---

# hearth-session

## 做什么

`hearth-session` 拥有会话日志这一唯一事实来源：`<dataDir>/sessions/<id>.jsonl`。
它负责按行追加事件、在打开时修复被中断写入留下的半行、按序回读、把日志折叠成
`Snapshot`、在日志增长时轮询审批决定、把完整轮次之前的前缀复制成一份新会话，以及
把最早一段完整历史替换成一条摘要记录。

日志是只追加的：本 crate 从不重写已有记录。遇到不认识的 `v`（事件格式版本）时，
`Store::open` 直接返回 `UnsupportedVersion`，不截断、不追加、不改动文件——宁可拒绝
一个未来版本，也不能把它当旧格式写坏。compaction 也一样只追加：`plan_compaction` 是
纯选择器，只决定哪一段序号可以被替换；`compact` 只向日志尾部追加一条
`compaction/summary` 事件，其 payload 用 `replaces: {from, to}` 声明它替身覆盖的范围。
原始事件一条不删，projection 读到该事件后清空消息、只显示摘要及其后的内容。

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
- `CompactPlan { from, to, reason }` / `plan_compaction(events, keep_turns)`：纯函数，
  返回“最早可被替换的完整区间”。它保证切点落在 `turn/end` 上、不拆开
  `tool/call` 与 `tool/result` 的配对、不重压已摘要过的范围，并保留最近
  `keep_turns` 个完整轮次（0 表示 `DEFAULT_KEEP_TURNS`）。无事可做时返回 `to == 0`
  的空计划，而不是错误；`keep_turns` 之外的输入损坏才返回 `ErrInvalidLog`。
- `Summarizer` / `TruncatingSummarizer { budget }`：把一段事件折叠成字符串。默认实现
  是确定性的、离线的：每个事件一行“序号 类型: 摘要”，按 UTF-8 边界裁剪到
  `budget` 字节（0 表示 `DEFAULT_SUMMARY_BYTES`），因此测试和 CLI 无需模型即可压缩。
- `compact(&Store, &CompactPlan, summary, now_ms)`：追加且只追加一条
  `compaction/summary`。计划非法、覆盖范围越界、与既有摘要重叠、摘要为空时一律
  拒绝，且不写任何字节；`now_ms` 为 `None` 时由 store 填入真实时间戳。
- `compact_log(&Store, &dyn Summarizer, keep_turns, min_events)`：读一次日志、低于
  `min_events` 就直接返回 `false`、按计划只把被覆盖的事件交给 summarizer、再追加。
  并发压缩把同一段抢先写掉时返回 `Ok(false)`（视为已完成），不当作错误。
- `last_compacted_span(&Store)`：从日志里读回最新一条摘要的 `replaces` 区间。

## 依赖与边界

只依赖 `hearth-core`（事件包络、错误种类、ID、canonical JSON）和标准库；`hearth-config`
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
- 不要在摘要为空或计划切在轮次中间时写入：宁可本次不压缩，也不要留下无法回放的日志。
