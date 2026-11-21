<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# M4 冻结面（内部接口契约）

本文件冻结 M4 各并行单元之间的对接面。**改动本文件 = 改契约**，必须通知所有受影响单元；
实现与契约冲突时以契约为准，改实现。

冻结日期：2026-11-16。上一版：无。

## 0. 全局不变量（不许在一个单元里破）

1. 日志是唯一事实源：`<dataDir>/sessions/<id>.jsonl`，只追加。
2. 派生数据（索引、列表、统计）**必须可删可重建**，删掉后功能退化但不能坏。
3. 跨包传递的时间是 `int64` 毫秒；不传 `time.Time`。
4. 可选成员省略，不写 `null`。

## 1. 单元与文件归属

一个文件只有一个所有者。`cmd/forge/main.go` 与各目录 `README.md` 的
`may_import` 行由**集成者**统一改——单元只报告需要哪一行，不自己动。

| 单元 | 拥有的文件 | 禁止写入 |
| --- | --- | --- |
| U1 索引 `internal/store` | `internal/store/**` | 其它一切 |
| U2 旧会话导入 `internal/import` | `internal/import/**` | 其它一切 |
| U3 真实摘要器 | `internal/agent/summarizer.go`、`internal/agent/summarizer_test.go` | `internal/agent/agent.go` 等既有文件 |
| — 集成者（lead） | `cmd/forge/main.go`、`cmd/forge/*.go` 的命令装配、所有 `README.md` frontmatter、`architecture.yaml`、已退役的 Go 树里的 `docs/deps.md` | 单元内部实现 |

## 2. U1：`internal/store`（SQLite 索引）

依赖：`modernc.org/sqlite` v1.34.5（纯 Go，无 cgo；已实测可构建，二进制 +6.1 MB）。

```go
package store

// Session 是索引里的一行：会话列表所需的全部字段，不含事件正文。
type Session struct {
	ID        string
	Workspace string
	Model     string
	Title     string // 第一条 user/message 的前 120 字节，去换行；无则空
	Status    string // protocol.Status* 的词表：idle|running|awaiting_approval|ended|aborted（下划线形态，与 internal/protocol 的 protocol.Status* 常量一致；早期草稿写作 awaiting-approval，与代码不符，M7 审计更正）
	CreatedAt int64
	UpdatedAt int64
	Turns     int
	LastSeq   uint64
}

// Hit 是一次全文命中的定位，不含正文全文。
type Hit struct {
	SessionID string
	Seq       uint64
	Turn      uint32
	Type      string
	Snippet   string // 命中片段，≤200 字节，两侧可截断
}

// Stats 是索引的可展示统计。
type Stats struct {
	Sessions   int
	Events     int
	Bytes      int64
	Workspaces int
}

// Query 是列表查询。零值合法：Limit 0 → 默认 50，上限 200。
type Query struct {
	Limit     int
	Cursor    string // 上一页的 NextCursor；空 = 第一页
	Status    string // 空 = 不过滤
	Workspace string // 空 = 不过滤
}

type Options struct {
	Path    string // index.db 的路径；由调用方决定（<dataDir>/index.db）
	Now     func() time.Time
	Logger  *slog.Logger
}

func Open(opt Options) (*Store, error) // 建库建表，幂等
func (s *Store) Close() error

// Put 幂等写入/更新一行。索引是派生数据：Put 失败不能让会话写入失败。
func (s *Store) Put(ctx context.Context, v Session) error
func (s *Store) Delete(ctx context.Context, id string) error

// List 按 UpdatedAt 降序、ID 破平返回一页。
func (s *Store) List(ctx context.Context, q Query) (rows []Session, next string, err error)

// Search 全文搜索（FTS5 prefix 查询）。空 query 返回空、不报错。
func (s *Store) Search(ctx context.Context, text string, limit int) ([]Hit, error)

func (s *Store) Stats(ctx context.Context) (Stats, error)

// Rebuild 扫 <dataDir>/sessions/*.jsonl 重建索引，返回会话数。
// 这是索引唯一的权威构建方式：它不信任增量 Put 的完整性。
func (s *Store) Rebuild(ctx context.Context, dataDir string) (int, error)
```

约束：
- `index.db` 与 `sessions/` 同级，**不在** `sessions/` 里面（删目录不能删日志）。
- 表结构版本化（`PRAGMA user_version`）；版本不认识时**丢弃重建**，不迁移。
- 写索引用单连接 + WAL；读可以并发。FTS 表与主表在同一个事务里更新。
- `Rebuild` 必须能在 1,000 个会话 / 100,000 事件上于 10 s 内完成（给实测数字）。

命令行（装配由集成者做，单元提供函数）：
`hearth index [--rebuild] [--stats] [--search TEXT] [--json]`。

## 3. U2：`internal/import`（旧 hearth 会话只读导入）

D3 已定：只读导入，不做双向兼容。

```go
package import // 包名 importold？见下

// Source 是一个旧会话目录的探测结果。
type Source struct {
	Dir       string
	Format    string // "hearth-session-v1" 等，未知则 "unknown"
	Sessions  int
	Events    int
	Warnings  []string
}

// Detect 探测目录能否导入；不写任何文件。
func Detect(dir string) (Source, error)

// Plan 把旧记录转换成新事件，返回将要写入的条数与不可转换的清单。
// 它不写文件：先看后写，是"只读导入"这个决定的落地方式。
func Plan(src Source) (Plan, error)

type Plan struct {
	SessionID string
	Events    int
	Dropped   []string // 无法映射的记录 + 原因
}

// Write 把 Plan 写成新的 v2 日志，返回写入的日志路径（与 internal/import/write.go 一致）。
// 已存在同名会话时返回 ErrExists，绝不覆盖。
func Write(ctx context.Context, src Source, plan Plan, dataDir string) (string, error)
```

约束：
- 旧格式的真相在 `D:\魔改版Agent框架\hearth\packages\session-format\**` 与 `hearth\snapshots\`、
  `hearth\projects\` 的真实数据里。**先读代码与真实样本再写映射**，不要凭猜。
- 导入产物必须能被 `session.Project` 与 `hearth` 的读路径完整消费（写测试证明）。
- 无法映射的记录进 `Dropped` 而不是静默丢弃；命令行必须把它们打印出来。
- 包名不能用 `import`（关键字）：用 `internal/import` 目录 + `package importold`。

命令行：`hearth import <旧目录> [--dry-run] [--json]`；`--dry-run` 只打印 Plan。

## 4. U3：真实摘要器（替换 `TruncatingSummarizer` 作为默认）

`internal/session/compact.go` 已定义 `Summarizer` 接口。新增实现放在
`internal/agent/summarizer.go`（agent 拥有 provider 与历史类型，session 不该反向依赖 llm）。

```go
// Summarize 用模型把一段历史压成摘要文本。
// 失败必须返回错误而不是退回截断：调用者要能决定"这次不压缩"。
type ProviderSummarizer struct {
	Provider llm.Provider
	Model    string
	MaxBytes int // 摘要上限，默认 2048；裁在 rune 边界
}
```

约束：
- 输入是**投影后的消息**（`[]llm.Message`），不是原始事件；它必须能在被压缩区间的
  任意切点上工作。
- 提示词写成常量并注释：为什么要求"不要编造决策与文件路径"。
- 测试用脚本化 provider（`internal/llm/llmtest`），断言：正常摘要、模型报错时错误传播、
  超长摘要被裁到 rune 边界、空历史不调用模型。

## 5. 集成者负责的改动（单元不要碰）

- `cmd/forge/main.go`：`index`、`import` 两个 case 与 `usage` 文本。
- 各目录 `README.md` 的 `may_import`（archcheck R3 会逼你加）。
- `architecture.yaml`：新增 `internal/store`、`internal/import` 的层。
- 已退役的 Go 树里的 `docs/deps.md`：`modernc.org/sqlite` 一行（版本、理由、替代方案、实测构建时间）。
- `docs/contracts/cli.md`：`hearth index`、`hearth import` 两节。