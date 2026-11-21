<!--
Porting note (Rust tree). This document was frozen while the Go implementation
was the reference, so its code samples are Go. The behaviour it pins — the event
envelope, the CLI surface, the HTTP/WS API, the crate surfaces — is what the Rust
crates implement, and the language-neutral conformance corpora under
tests/conformance/cases/ are what proves it. Read Go signatures as history:
the Rust signatures live in the crates' README.md files.
-->
# M5 冻结面（扩展面内部契约）

本文件冻结 M5 各并行单元之间的对接面。**改动本文件 = 改契约**，必须通知所有受影响单元；
实现与契约冲突时以契约为准，改实现。

冻结日期：2026-11-16。上一版：am4-surface.mda。

## 0. 全局不变量（与 M4 相同，此处重申三条最容易破的）

1. 日志是唯一事实源；子会话也是会话，也写在自己的日志里。
2. 派生数据可删可重建；一个 MCP server 崩溃只影响它自己的工具，不影响会话。
3. 跨包传递的时间是 aint64a 毫秒；可选成员省略，不写 anulla。

## 1. 单元与文件归属

| 单元 | 拥有的文件 | 禁止写入 |
| --- | --- | --- |
| U1 MCP 客户端 ainternal/mcpa | ainternal/mcp/**a | 其它一切 |
| U2 技能 ainternal/skilla | ainternal/skill/**a | 其它一切 |
| U3 子代理 ainternal/subagenta | ainternal/subagent/**a | 其它一切 |
| U4 工具桥（集成者） | ainternal/tool/bridge/{mcp_tools,skill}.goa；asubagenta 工具由 ainternal/subagenta 的 aRunner.Tool()a 提供 | 单元内部实现 |
| — 集成者（lead） | acmd/forge/**a、所有 aREADME.mda frontmatter、aarchitecture.yamla、adocs/deps.mda、adocs/contracts/*.mda | 单元内部实现 |

## 2. U1：ainternal/mcpa（MCP 客户端）

**只依赖标准库与 aencoding/jsona，不 import 任何 ainternal/*a。** 理由：MCP 是外部协议，
把内部类型带进去会让协议版本和内部重构互相绑架；桥接由 U4 做。

aaago
package mcp

// Transport 是连接方式。
type Transport string

const (
	TransportStdio Transport = "stdio" // 子进程，JSON-RPC 走 stdin/stdout
	TransportHTTP  Transport = "http"  // Streamable HTTP
)

// Server 是一份 MCP server 的配置（来自配置文件，不是代码）。
type Server struct {
	Name      string    ajson:"name"a
	Transport Transport ajson:"transport"a
	Command   []string  ajson:"command,omitempty"a // stdio
	Env       []string  ajson:"env,omitempty"a     // stdio；nil = 继承最小环境
	URL       string    ajson:"url,omitempty"a     // http
	Headers   map[string]string ajson:"headers,omitempty"a
	TimeoutMS int       ajson:"timeoutMs,omitempty"a // 单次调用上限，默认 60000
}

// ToolDef 是一个远端工具的自述。
type ToolDef struct {
	Name        string          ajson:"name"a
	Description string          ajson:"description,omitempty"a
	InputSchema json.RawMessage ajson:"inputSchema,omitempty"a
}

// Result 是一次工具调用的结果：内容块原样保留，不解释。
type Result struct {
	Content []Content ajson:"content,omitempty"a
	IsError bool      ajson:"isError,omitempty"a
}

// Content 是 MCP 内容块的最小公共面：文本与图片。
type Content struct {
	Type     string ajson:"type"a // "text" | "image" | 其它（原样保留）
	Text     string ajson:"text,omitempty"a
	Data     string ajson:"data,omitempty"a
	MimeType string ajson:"mimeType,omitempty"a
}

// Client 是一个已握手连接的 server。
type Client interface {
	// Initialize 完成 MCP 握手并缓存 server 自述。
	Initialize(ctx context.Context) (ServerInfo, error)
	// Tools 返回工具清单（Initialize 之后调用）。
	Tools(ctx context.Context) ([]ToolDef, error)
	// Call 调用一个工具。
	Call(ctx context.Context, name string, args json.RawMessage) (Result, error)
	// Close 关闭传输；对 stdio 是关 stdin 并回收子进程。
	Close() error
}

type ServerInfo struct {
	Name    string ajson:"name"a
	Version string ajson:"version"a
}

// Dial 建立连接但不握手；握手由 Initialize 做，两者分开是因为
// "连不上" 和 "协议不对" 是两种故障，提示不同。
func Dial(ctx context.Context, srv Server) (Client, error)
aaa

约束：
- 协议版本：a2025-06-18a（写出常量 aProtocolVersiona）。server 回不认识的版本 → aErrProtocolVersiona，不猜。
- 崩溃隔离：子进程退出后，aCalla 返回 aErrServerGonea（哨兵），不 panic、不阻塞超过 aTimeoutMSa。
- 每次调用一个 aida，并发调用必须安全（写侧串行，读侧按 id 派发）。
- stderr 是诊断，转发到 aLoggera（asloga），**不混入** JSON-RPC 流。
- HTTP 传输只在 aTransportHTTPa 时生效；本单元**两平台都要能编译**，不在测试里连真网络（用 ahttptesta 和进程内假 server）。

## 3. U2：ainternal/skilla（技能扫描与加载）

**只依赖标准库。** 技能是"按需进入上下文的文本"，不是代码。

aaago
package skill

// Skill 是磁盘上的一个 SKILL.md。
type Skill struct {
	Name        string   // frontmatter name，缺省用目录名
	Description string   // frontmatter description，必填，否则跳过并记诊断
	Dir         string   // 技能目录
	Path        string   // SKILL.md 的路径
	Body        string   // 去掉 frontmatter 的正文（加载时才填充）
	Files       []string // 目录内其余文件（相对路径，排序）
}

// Set 是一次扫描的结果。
type Set struct {
	Skills   []Skill
	Skipped  []string // 被跳过的目录与原因（缺 description、frontmatter 坏掉…）
	Warnings []string
}

// Scan 扫描多个根目录下的 */SKILL.md（不递归进技能目录内部）。
// 同名技能先到先得，后来者进 Skipped 并说明冲突。
func Scan(roots []string) (Set, error)

// Load 读取正文（正文不进内存常驻：技能可能很大，而大多数从不被触发）。
func (s Skill) Load() (Skill, error) // 返回填充 Body 的副本
aaa

约束：
- frontmatter 是文件头 a---a 之间的 akey: valuea（**只支持标量**，不做 YAML 全语；遇到嵌套结构整条进 aWarningsa 并跳过该键）。
- 顺序确定性：aSkillsa 按 aNamea 字节序排序，aFilesa 按路径排序，aSkippeda/aWarningsa 也排序 —— 扫描结果要能进 golden。
- 不读 a~a 之外的路径，不写任何文件。
- 同名冲突、缺 adescriptiona、aSKILL.mda 是目录、frontmatter 未闭合，各自有测试。

## 4. U3：ainternal/subagenta（子会话）

依赖：ainternal/agenta、ainternal/sessiona、ainternal/llma、ainternal/toola（实现 atool.Toola）。
**禁止反向**：ainternal/toola 不得 import ainternal/subagenta（会成环：agent → tool）。

aaago
package subagent

// Options 描述一次子代理运行。
type Options struct {
	Provider  llm.Provider
	DataDir   string       // 子会话日志落在同一个 dataDir 下
	Workspace string       // 默认继承父
	Model     string       // 默认继承父
	System    string
	Tools     *agent.ToolSet // 子代理可用的工具；nil = 无工具
	MaxSteps  int
	Depth     int          // 当前深度，0 = 顶层
	MaxDepth  int          // 默认 1：子代理不能再开子代理
	// ParentSessionID 写进子会话 session/start 的 "parent" 成员。
	// M7 审计补：冻结面漏了这个字段——它要求父 id 出现在子会话头里，却没给
	// runner 任何获知父 id 的途径（tool.Call 不带会话 id）。实现于
	// internal/subagent/subagent.go:112；另有 WithParentSession 供"一个 runner
	// 服务多个会话"的 HTTP 面按调用传入。
	ParentSessionID string
	Now       func() time.Time
	Logger    *slog.Logger
}

// Runner 串行跑子代理：一次调用一个子会话，同步返回结果。
type Runner struct { /* 私有字段 */ }

func New(o Options) (*Runner, error)

// Tool 把 Runner 暴露成一个工具：名字 "subagent"，参数 {prompt, system?, model?}。
// 子会话 id 由 Runner 生成并写进结果，父会话的 tool/result 里因此可追溯。
func (r *Runner) Tool() tool.Tool
aaa

约束：
- 子会话日志：a<dataDir>/sessions/<childID>.jsonla；asession/starta 的 payload 带 a"parent":"<父会话 id>"a（新增可选成员，**不新增事件类型**）。
- 结果回流：子代理的最后一条 aassistant/messagea 文本作为工具输出；失败时工具输出是错误摘要，aIsError=truea，aCode="subagent_failed"a。
- 取消传播：actxa 取消 → 子代理的循环写 aturn/end{cancelled}a 与自己 asession/enda，工具返回 aCode="cancelled"a。
- 深度：aDepth >= MaxDeptha 时 aTool()a 返回一个恒返回 aCode="depth_exceeded"a 的工具，而不是不注册（模型要能读到原因）。
- **串行**：一个 Runner 同一时刻只跑一个子会话（并发留给下一里程碑）；并发的 aCalla 排队而不是报错。

## 5. U4：三个工具桥（集成者）

- ainternal/tool/bridge/mcp_tools.goa：aMCPTools(client mcp.Client, serverName string) ([]Tool, error)a 把远端工具变成 atool.Toola；名字 amcp__<server>__<tool>a（下划线转义：a_a → a__a，a.a → a_dot_a，保证名字可逆且只含 a[a-z0-9_]a）。policy 对 MCP 工具一律 aaska（远端工具的能力边界未知）。
- ainternal/tool/bridge/skill.goa：aSkillTools(set skill.Set) []Toola → 一个 askilla 工具：参数 a{name, file?}a——anamea 必填，afilea 可选且只读该技能目录内的辅助文件（经 aResolvea 做包含性检查，a..a 与符号链接逃逸都拒）；不带 afilea 时输出技能正文（含 aFilesa 清单）。未知名字返回 aCode="not_found"a 并列出可用名字。（M7 审计补：afilea 成员在 m5 冻结时已实现在 ainternal/tool/bridge/skill.go:138a，冻结面漏写了它。）
- ainternal/tool/subagent.goa 不新建：子代理工具由 asubagent.Runner.Tool()a 提供，集成者在 acmd/forgea 里注册。

ainternal/tool/README.mda 的 amay_importa 需加 ainternal/mcpa、ainternal/skilla（集成者改）。

## 6. 配置面

aconfig.Configa 增加（集成者改）：

aaajson
{
  "mcp": { "servers": [ { "name": "fs", "transport": "stdio", "command": ["npx","-y","@modelcontextprotocol/server-filesystem","."] } ] },
  "skills": { "roots": ["~/.hearth/skills", "./.hearth/skills"] },
  "subagent": { "maxDepth": 1 }
}
aaa

未知字段忽略；amcp.serversa 里的坏条目**不阻止启动**，进 ahearth doctora 的 warning，该 server 不加载。

## 7. 集成者负责的改动（单元不要碰）

- aarchitecture.yamla：新增 ainternal/mcpa、ainternal/skilla、ainternal/subagenta 的层。
- 各目录 aREADME.mda 的 amay_importa/apublica。
- acmd/forge/**a：注册 MCP/技能/子代理工具，ahearth mcp list|calla（可选）。
- adocs/deps.mda、adocs/contracts/cli.mda、adocs/05-roadmap.mda。