---
dir: crates/import
layer: L2
role: 旧项目会话日志的一次性只读导入：旧格式 → 新 v1 事件日志
public:
  - Source
  - Detect
  - Plan
  - MakePlan
  - Write
  - ERR_EXISTS
  - ERR_COMPRESSED
  - ERR_NO_DIR
may_import:
  - crates/core
must_not:
  - import any other forge crate at runtime
  - read old-format files outside this crate
  - overwrite an existing session log
gate: cargo test -p forge-import --offline
---

# crates/import

## 做什么

把旧项目（TypeScript 版 forge）的会话日志 `session[.vN].jsonl` 一次性转换成
新仓库的 v1 事件日志，写到 `<data_dir>/sessions/<new_id>.jsonl`。

三步分开，副作用只有最后一步：

- `Detect`：只读扫描，认出旧日志的口径版本、会话数和事件数，不创建/修改任何文件。
- `MakePlan(&Source)`：只计算不落盘，给出新会话 id、要写的事件数，以及每一条无法映射的
  旧记录及原因（`Plan::dropped`）。
- `Write`：在 `<id>.jsonl.tmp` 建立日志，校验后原子提交；目标已存在返回 `ERR_EXISTS`。

契约 `docs/contracts/m4-surface.md` §3 同时声明了 `Plan` 类型和 `Plan(src)` 操作；
Rust 里两者可以同名，但为了与 Go 参考实现和调用方保持一致，操作叫 `MakePlan`。

导入是单向的（D3）：新仓库运行时永远不读旧格式，旧格式只能在这里被读一次。

## 公开面

`Source`、`Detect`、`Plan`、`MakePlan`、`Write`，以及哨兵 `ERR_EXISTS` /
`ERR_COMPRESSED` / `ERR_NO_DIR`（配合 `err.kind()` 与 `is_*` 判定函数使用）。
所有函数返回 `forge_core::Result<T>`，错误类型是 `forge_core::Error`。

## 依赖与边界

L2，运行时只依赖 `crates/core`（事件信封、id、规范化 JSON 与错误词汇）。
`ring` 提供事件 id 的 SHA-256 指纹，`serde_json` 解析旧记录 —— 两者都在工作区
依赖预算里。`forge-session` 只作为 dev-dependency 出现，供“重新解析并折叠导入日志”
的验收测试使用，不进入运行时依赖图。

旧格式的事实来源是旧仓库的代码和真实快照，回归样本已复制进 `tests/fixtures`；
测试运行时不引用 Go 树。

不支持 `.jsonl.zstd`：解码 zstd 需要本构建不携带的依赖，`Detect` 直接返回
`ERR_COMPRESSED`，而不是猜测或半懂地读。

## 不要做

- 不要在运行时读旧格式。除本 crate 外任何地方都不该出现旧日志的字段名。
- 不要覆盖或追加已有会话日志：目标存在就是 `ERR_EXISTS`，日志是唯一真相。
- 不要静默丢弃旧记录。映射不了的类型必须进 `Plan::dropped` 并在原因里点名旧类型；
  有信息损失的映射记 `degraded:` 而不是假装无损。
- 不要发明旧记录里没有的事实（审批人、消息耗时、压缩替换区间、请求消息条数）。
- 不要让 `Detect` 产生任何写入：测试用整棵源目录的指纹证明这一点。
