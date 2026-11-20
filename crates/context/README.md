---
dir: crates/context
layer: L2
role: compressed blocks that can be named, restored and searched instead of being dropped
public: [Block, Ledger, Hit, MAX_SUMMARY_BYTES, is_block_id, context_compressed, parse_block, EVENT_TYPE, ERR_BAD_BLOCK, ERR_OVERLAP, ERR_EMPTY_SUMMARY, ERR_BLOCK_TOO_LARGE, is_bad_block, is_overlap, is_empty_summary, is_block_too_large]
may_import: [hearth-core]
must_not:
  - read a log, a file or a blob
  - call a model or start a process
  - hide a range that the log has not closed yet
gate: cargo test -p hearth-context --offline
---

# hearth-context

## 做什么

裁剪（compaction）在这个项目里已经存在，而且它是单向的：最旧的那段完整历史被摘要替换，再也回不来。那是"窗口满了"的正确形状，却不是"我不想再为这四十条消息付钱、但可能还要再看其中一条"的正确形状。

这个 crate 提供第二种形状。一个**压缩块**指名日志里一段闭区间的 `seq`，带一段摘要（模型读它而不是读原文），并有一个 id（模型把它交回来就能取回原文）。日志依然只追加：建立一个块是追加一条 `context/compressed` 事件，取回一个块是一次读。

它同时也回答"我之前压缩过什么"：`Ledger::search` 在摘要里按词搜，`Ledger::covering` 给出某个 `seq` 属于哪个块。

## 公开面

- `Block`：`id`（32 位 hex）、`from`/`to`（闭区间 seq）、`summary`、`messages`（建立时的消息条数，仅作诊断）；`Block::new`/`with_messages`/`validate`/`covers`/`overlaps`/`span`。
- `Ledger`：`new`/`from_events`/`blocks`/`get`/`covering`/`hides`/`hidden_span`/`insert`/`search`/`len`/`is_empty`。
- `wire`：`EVENT_TYPE`、`context_compressed(session, &Block) -> Result<Event>`、`parse_block(&Event) -> Result<Option<Block>>`。
- 错误判定：`ERR_*` 四个字符串与对应的 `is_*`。

## 依赖与边界

只依赖 `hearth-core`（事件、错误、id）。不读日志、不读文件、不起进程、不碰模型——把块应用到历史上是调用方的事（CLI 的 `crates/cli/src/bridges.rs` 附近），因为只有调用方手里有日志。

三条规则都是"日志是唯一真相"的推论：

1. **按区间寻址，不按切片。** 块记的是 `seq` 闭区间。重放、fork、换一个读者之后 `seq` 仍然指向同一批记录；消息下标三者都活不过。
2. **一段只有一个地址。** 两个块不许重叠，重叠会让同一条消息被隐藏两次，而"取回"将有两个答案。
3. **块不隐藏尚未闭合的边界。** 由写入方检查（它知道日志写到哪），本 crate 只校验形状。

## 不要做

- 不要在这里读磁盘或打开 store。它必须能在一个只有事件切片的地方跑（测试、浏览器后端、未来的导入器）。
- 不要把 `messages` 当索引用。它是诊断数字，重放后可能与当前投影的条数不同，用它定位会错位到别的记录。
- 不要在 `Ledger::insert` 里接受重叠的"更窄的块"。宁可让调用方报错，也不要让同一条消息有两个 owner。
- 不要把摘要截断当作体积保护：`MAX_SUMMARY_BYTES` 是拒绝，不是裁剪，截断的摘要会让模型以为原文就这么短。