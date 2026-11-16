---
dir: docs/contracts
layer: L0
role: 冻结的对外契约：事件格式、命令行、HTTP/WS API、各 crate 的公开面
public: []
may_import: []
must_not: [把 Rust 签名改写成与实现不符的样子, 在同一份文档里既写"已实现"又留 NOT IMPLEMENTED]
gate: make arch
---

# docs/contracts —— 冻结的契约

## 做什么

这里保存这个项目对外的四个面，以及三个里程碑冻结的能力面。它们是**语言的边界，不是 Go 的财产**：
Rust 移植逐条实现这些行为，`tests/conformance/cases/{events,http,ws}/` 的语言中立语料是唯一的判据。

| 文件 | 钉住什么 |
|---|---|
| `events-v1.md` | 事件信封、15 种事件类型、payload 键名、追加与回放规则、压缩记录 |
| `cli.md` | 九条命令、帮助文本、stdout/stderr 分工、退出码、doctor 十项 |
| `api.md` | HTTP 与 WebSocket 的路径、状态码、错误信封、帧种类、幂等键、快照与增量 |
| `m4-surface.md` | 索引、旧格式导入、resume/fork、压缩的公开面 |
| `m5-surface.md` | MCP 客户端、技能加载、子代理运行器的公开面 |
| `m6-surface.md` | 沙箱三档、执行挂载点、危险命令名单、doctor 的沙箱检查 |

## 公开面

没有代码，只有契约。读它的人（人、agent、测试）都只看行为。

## 依赖与边界

这些文档描述行为，不描述实现。文中出现的 Go 签名是冻结当时的历史：Rust 的对应签名在各 crate 的
`README.md` 的 `public:` 行里。两者冲突时以 **Rust 代码 + 语料测试** 为准，然后回来改这里。

## 不要做

- 不要在契约里写实现细节（哪一行、哪个函数、哪个 crate 内部结构）。
- 不要把"计划中"写成"已实现"：`api.md` 曾经整篇写着 NOT IMPLEMENTED 而服务器已经在跑，那种诚实
  和那种谎言是同一种错误——描述与代码不一致。
- 不要只改契约不改测试：契约变了，语料或断言必须跟着变，否则下一个人会以为契约是愿望。