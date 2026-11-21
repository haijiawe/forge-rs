---
dir: crates/experts
layer: L1
role: expert and expert-team definitions parsed from Markdown plus YAML frontmatter
public:
  - Set
  - Expert
  - Team
  - TeamMember
  - scan
  - prompt_for
  - team_prompt
  - Error
  - ErrorKind
  - Result
  - ERROR_CODES
may_import: []
must_not:
  - depend on any hearth crate or any external crate
  - resolve YAML that is not the documented scalar / flow-list / block-list subset
  - normalise, trim or re-encode a definition body
  - decide which expert a turn should summon
gate: cargo test -p hearth-experts --offline
---

# hearth-experts

## 做什么

一个 expert 就是一个能被当作子代理召来的人格：一个 Markdown 文件，frontmatter 里带 `name`、`slug`、`description`，可选带 `division`、`enabled` 和 `tools`，正文就是系统提示。一个 team 也是一个 Markdown 文件，点名若干 expert 及其职责、外加一个 `coordinator`，正文是团队级指令。

`scan(root)` 遍历一棵目录树，解析每个 `.md` 文件，返回一个**已经对自身做过校验**的 `Set`：slug 合法且整棵树内唯一，每个必填字段都在，每个 team 引用都指向一个真实且已启用的 expert。根目录不存在是**空集合**而不是错误，因为一台从没建过 experts 目录的机器是正常机器，它必须仍然能启动。

`prompt_for(&set, slug)` 和 `team_prompt(&set, slug)` 把那个集合里的一条变成调用方发给模型的准确字符串。两者都是集合与 slug 的纯函数，所以调两次得到逐字节相同的字符串；一个 team 的成员按其文件里的顺序出现，绝不重新排序。

塑造了所有拒绝的三条规则：

- **拒绝，不警告。** 丢掉一行 `tools:` 会把一个只读审阅者变成一个可以跑命令的人格。丢掉一行 `members:` 会让一个四个人的 team 悄悄变成三个人的团队。每一种拒绝都点名文件以及要改什么。
- **正文是字节。** `Expert::prompt` 与 `Team::prompt` 就是文件正文原样，包括 `\r\n`、制表符、一行看起来像结束围栏的文本，以及结尾换行。任何做规范化的加载器，都会让提示与审阅者批准过的那份文件不一致。
- **一个命名空间。** team slug 与 expert slug 共用一个命名空间，两个文件定义同一个 slug 时被拒，消息里带上两条路径。「哪一个赢」正是一份名册绝不能有的歧义。

## 公开面

- `scan(&Path) -> Result<Set>`；`Set::empty()`。
- `Set`：`experts()`、`teams()`、`expert(slug)`、`team(slug)`、`summonable(slug)`、`is_empty()`、`len()`。只有 `scan` 能造出非空的那个，所以「一个 `Set` 存在」就意味着「那棵树被检查过」。
- `Expert { slug, name, description, division, enabled, tools, prompt, path }`。`tools: Option<Vec<String>>`：`None` 表示没有声明任何限制，`Some(vec![])` 表示 `tools: []`。这是两条不同的指令，拼出来的提示会明说。
- `Team { slug, name, description, coordinator, members, prompt, path }`，`TeamMember { expert, duty, description }`（`description` 是被点名成员自己的，在加载时解析出来）。
- `prompt_for(&Set, slug) -> Option<String>`——正文，加上 `tools` 存在时的工具限制行。未知或已禁用的 slug 得到 `None`。
- `team_prompt(&Set, slug) -> Option<String>`——正文，加上 coordinator 指令，加上每个成员一行带编号的 slug、职责与描述。未知 slug 得到 `None`。
- `Error` 带 `kind()`、`code()` 与 `message()`；`ErrorKind` 带 `as_str()`；`ERROR_CODES` 给出全部稳定码。

对 CLI 稳定的码表：`missing_frontmatter`、`bad_frontmatter`、`missing_field`、`bad_slug`、`duplicate_slug`、`unknown_member`、`disabled_member`、`too_few_members`、`unreadable`。

## 依赖与边界

只用标准库。没有 hearth 内部依赖，没有外部 crate，没有 `anyhow`，没有 async 运行时。这份名册就是文本：加一个依赖只会多出版本要争论，而任务书里的第三条法则不允许这么做。

为什么不复用 `hearth-skill` 的读取器——尽管那是一条同层边，而且从三个角度看都合法：

- 它的 frontmatter 解析器和它的遍历器都是 `pub(crate)`，复用意味着为了拿到一个 `BTreeMap` 而把另一个 crate 的隐私面撕开；
- 它只扫 `root/*/SKILL.md` 且从不递归，而一份名册是真正嵌套的树；
- 它的策略与本 crate 的正相反：它丢掉读不懂的键，把 `\r\n` 规范成 `\n`，还裁掉正文开头的换行。这三条对一个 skill 都对，对一个必须逐字节保持原样、且丢掉一行 `tools:` 就会改变模型能做什么的提示来说，全都是不可接受的。

边界是「读并检查，绝不做决定」。本 crate 报告存在哪些 expert 和 team，并产出它们的提示；哪一轮该召哪一个 expert 是策略，属于 L1 之上。

## 不要做

- 不要支持 YAML 的其余部分。可接受的子集是：一行 `---` 围栏、第 1 列上的标量 `key: value`、`key: [a, b]` 流式列表，以及由标量或单层 `- key: value` 映射构成的 `- item` 块列表。块标量（`|`、`>`）、嵌套映射、嵌套列表、锚点、标签、流式映射和多文档文件一律带消息拒绝，绝不猜测——长文本该放的地方是正文。
- 不要规范化正文。不转换换行、不裁剪、正文里不处理 BOM。有测试断言正文能挺过 `\r\n`、制表符、一行 `---` 和非 ASCII。
- 不要整份读入一个文件。读取上限 1 MiB，超限文件被拒绝而不是截断：被截断的提示是没人审阅过的提示。
- 不要排序、去重或重排成员，也不要给提示行排序。文件里的顺序就是被审阅过的顺序。
- 不要把未知字段当警告。把 `tool:` 误写成 `tools:` 会悄悄丢掉一条工具限制，所以认不出的字段就是一次拒绝。
- 不要无界递归。深度上限 32 层，一个目录被第二次遇到时只走一次，这样一条错根或一个符号链接环不可能把加载器挂死。
- 明确说清楚**没有**实现的：没有写入器（`scan` 从不创建或修改文件，这里没有 `hearth expert new`）；没有监视或缓存（每次 `scan` 重新读那棵树）；expert 之间没有继承；也不校验 `tools:` 里的名字是不是 `hearth-tool` 真的注册了的工具——本 crate 不导入 L2，无从知道，所以那里的拼写错误由注册表在调用时抓到，而不是在这里。