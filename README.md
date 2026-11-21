# hearth

`hearth` 是一个本地编程 Agent 宿主：一个可执行文件、一条只追加的事件日志、一个 loopback 上的
HTTP/WebSocket 服务，以及可选的一层图形外壳。工具跑在沙箱里，每个动作先过策略与审批，
会话可以中断、恢复、分叉、压缩。

它是**一棵 Go 实现（`forge-go`）的 Rust 重写**，那棵 Go 树已于 **2026-11-17 删除**——
在移植通过 W6、五个能力 crate 接进 CLI 之后。那棵树是参照物：它的测试就是判据，
移植过来的行为要求**与它一致**，而不是比它更好。删掉它没有改变任何一个测试：
一致性用例、fixture 与 golden 都复制进了 `tests/`，构建与测试期间没有任何东西读那棵 Go 树。
代码注释里出现的 `forge-go/internal/...` 指的是那棵已退役树里的文件，留着是为了说明某段行为的出处；
`cargo test` 真正比对的是 `docs/contracts/` 里的冻结契约与 `tests/` 下的语料。

工作区由**二十四个产品 crate**组成，一个 crate 对应一条「越界时应该大声报错」的边界，
外加 `tools/archcheck` 与 `tools/bench` 两个门禁。`architecture.yaml` 写明分层图，
`tools/archcheck` 在一个 `Cargo.toml` 的依赖指向上层、或某个 crate 没有边界说明书时让构建失败。
Cargo 已经保证了图能编译；这些门禁保证它**指对了方向**、并且被写下来了。

CI 跑在 GitHub Actions 上：`ci.yml` 每次推送都跑（Windows 与 Ubuntu 上跑 1.89 这个下限，
lint 与 web 跑当前 stable），`bench.yml` 手动触发，`release.yml` 打 tag 时跑或做一次 dry run——
dry run 于 2026-11-20 演练过，产出了三个平台的原生产物。`docs/ci.md` 记录了每次运行抓到了什么，
包括那些只有第二台机器才看得见的缺陷。

## 先看这一页：每个目录是干什么的

`ls` 只能给出名字。名字表达「类别」，表达不了「这里放什么、不放什么、下一个东西该放哪一格」。
所以每一个装着东西的目录都有一份说明书，而**这件事是被检查的**（`architecture.yaml` 的
`documented_dirs`，规则见 `tools/archcheck`）——一条只写在文档里的规矩会在赶时间时消失。

| 目录 | 放什么 | 说明书 |
| --- | --- | --- |
| `crates/` | 二十四个产品 crate，按 L0–L3 分层；每个 crate 一份边界说明书 | [crates/README.md](crates/README.md) |
| `tests/` | 判据：一致性用例、fixture、golden、端到端与 WebSocket 语料 | [tests/README.md](tests/README.md) |
| `tools/` | 两个不产出功能、只负责「不许退步」的 crate | [tools/README.md](tools/README.md) |
| `docs/` | 决策、执行记录、对比、运维四类文档 + 七份冻结契约 | [docs/README.md](docs/README.md) |
| `web/` | 浏览器界面（React + TypeScript + Vite），编译期嵌进二进制 | [web/README.md](web/README.md) |
| `apps/` | 站在核心外面的图形外壳 | [apps/README.md](apps/README.md) |
| `scripts/` | 安装、发布、基准记录 | [scripts/README.md](scripts/README.md) |
| `skills/` | 给模型看的技能说明书（Markdown），不是代码 | [skills/README.md](skills/README.md) |
| `examples/` | 可以照抄的最小样本（插件） | [examples/README.md](examples/README.md) |
| `bin/` | 手工重建的 release 二进制（内嵌了 `web/dist`） | [bin/README.md](bin/README.md) |

`skills/`、`examples/`、`crates/tool/src/builtin/` 是三套不同的扩展入口；`web/` 与
`apps/desktop/` 是两个不同的前端构建入口。想知道「新东西该放哪」，看
[crates/README.md](crates/README.md) 里那张「我想做 X，去哪」表。

## crate 地图

| Crate | 层 | 放什么 | 不许做什么 |
| --- | --- | --- | --- |
| `hearth-core` | L0 | 冻结的词汇——事件 envelope、id、规范化 JSON、带类型的错误 | 依赖任何其它 hearth crate |
| `hearth-config` | L0 | 配置文件、home 目录、默认值、MCP 条目的告警 | 知道会话的存在 |
| `hearth-exec` | L1 | 起进程、stdio、超时、杀进程树、沙箱挂点 | 知道工具或会话 |
| `hearth-sandbox` | L1 | 遏制：job object 与 AppContainer、Landlock、`sandbox-exec` | 知道模型 |
| `hearth-mcp` | L1 | 外部 MCP 客户端——stdio 与 Streamable HTTP | 依赖任何 hearth crate |
| `hearth-skill` | L1 | 技能发现，且只在被要时才加载 `SKILL.md` 正文 | 递归进技能目录 |
| `hearth-net` | L1 | 阻塞式 HTTP(S) 客户端、重定向、HTML 转文本 | 知道工具或模型 |
| `hearth-experts` | L1 | 从 Markdown frontmatter 解析出的专家与团队定义 | 伸手进技能或会话 |
| `hearth-session` | L2 | 只追加日志、投影、压缩、分叉 | 在自己的日志之外做 I/O |
| `hearth-llm` | L2 | provider 契约、OpenAI 兼容的 SSE 客户端 | 知道会话 |
| `hearth-tool` | L2 | 注册表、执行器、策略、审批、内置工具 | 写事件，或自己决定策略 |
| `hearth-protocol` | L2 | 服务端与界面共享的 wire 类型 | 依赖服务端 |
| `hearth-agent` | L2 | 单轮循环、事件顺序、摘要器、两个改写 seam | 在轮与轮之间留一份自己的历史 |
| `hearth-store` | L2 | 可重建的索引——SQLite 行、边界消息的 FTS5 | 往会话日志里写 |
| `hearth-import` | L2 | 对旧会话树的只读导入 | 覆盖已存在的会话 |
| `hearth-subagent` | L2 | 有界池委派，跑在全新的子日志上 | 无深度上限地跑 |
| `hearth-goal` | L2 | 从日志折叠出的目标状态，以及续跑判定 | 自己写事件 |
| `hearth-schedule` | L2 | 重复规则、定时任务注册表、到期时刻 | 跑一轮 |
| `hearth-market` | L2 | 市场索引、校验摘要的安装、能识别篡改的卸载 | 在摘要核对前相信一个 tarball |
| `hearth-plugin` | L2 | 插件宿主——用 stdio JSON-RPC 说话的子进程 | 从一份列表里 spawn 任何东西 |
| `hearth-team` | L2 | 共享任务板上的并行 worker，每个 worker 一个信箱 | 依赖某个 provider 或异步运行时 |
| `hearth-context` | L2 | 可命名、可还原、可搜索的压缩块 | 藏起一段没有任何块命名的区间 |
| `hearth-server` | L3 | 日志之上的 HTTP/1.1 + WebSocket | 装 agent 逻辑 |
| `hearth-cli` | L3 | `hearth` 这个二进制 | 留住该属于某个 crate 的逻辑 |

## 工具链约束

这些是在本机**测出来**的，不是选出来的：

- **最低 rustc 是 1.89**，写在 `Cargo.toml` 的 `rust-version` 里，由依赖树决定而不是口味决定：
  `rusqlite 0.43` 要 1.89、`libsqlite3-sys 0.40` 要 1.88。这文件里早先写的 `1.85` 在移植计划里
  错了几个月；`cargo +1.85 check` 会在一行代码都没编之前报
  `rustc 1.85.1 is not supported by the following packages`。两头都验过：整套测试在 1.89 上通过
  （`cargo +1.89.0 test --workspace --offline`，64 个 suite），在 1.96 上也通过。
- **target 目录必须是全 ASCII 路径，而且它由环境决定，不写在树里。** 当 target 落在
  `D:\魔改版Agent框架\...` 这类路径里时，每个要编 C 的 crate 都会死在 `cl.exe` 的
  `D8050 ... 未能将命令行放入调试记录中`。曾经提交过一个
  `target-dir = "D:/hearth-target/hearth"`，看起来是修法，结果弄坏了第一个 Linux CI job——
  在那台机器上 `D:/...` 是一个**含冒号的路径段**。所以 `.cargo/config.toml` 什么都不钉：
  `check.ps1` 在发现本 checkout 路径不是全 ASCII 时会推导 `CARGO_TARGET_DIR`，CI 每个 job 自己设，
  而裸 `cargo build` 需要你自己 export。它必须是环境变量而不能只是 `build.target-dir`，
  因为 e2e 会 spawn 真的 `hearth` 二进制、并通过 `CARGO_TARGET_DIR` 找到它。
- **只能用 MSVC。** GNU 工具链装了但是坏的：
  `error calling dlltool 'dlltool.exe': program not found`。构建失败不许靠「换成 GNU」来「修」。
- **只能离线。** crate 来自 `D:\Rust\.cargo\config.toml` 里的 rsproxy 镜像，本机连不上 `crates.io`。
  用 `--offline`。加一个不在本地缓存里的依赖会报一个看起来像证书错误的失败。
- **交叉目标是编译检查，仅此而已。** 装了 `x86_64-unknown-linux-gnu` 与 `aarch64-apple-darwin`，
  本机没有这两个 triple 的链接器。`aarch64-unknown-linux-gnu` 与 `aarch64-pc-windows-msvc` 没装。
  `make release` 检查除 `hearth-cli` 之外的每个 crate；排除它是因为 `rustls`/`ring` 需要本机没有的
  目标 C 编译器（`cc`）。原生 CLI 由 `cargo build --release --offline` 覆盖。

## 快速开始

在新开的 shell 里：

```powershell
cargo build --release --offline
cargo test --offline
cargo run -q -p archcheck --offline
```

这个项目开发时的 checkout 是 `D:\hearth`，全 ASCII，所以树内 `target/` 可用。若把它移到路径里
带非 ASCII 字符的地方，先设一个 ASCII 的 target 目录（`$env:CARGO_TARGET_DIR`），否则每个要编 C 的
crate 都会报 `cl: D8050`；`check.ps1` 发现这种情况时会替你做。

`cargo run -q -p archcheck --offline -- -json` 输出给 CI 的机器可读形式；`-root <路径>`
检查工作目录之上的另一棵树。

## 命令

十七种拼法。前九个是 Go 判据在 `docs/contracts/cli.md` 里冻结的，`goal`/`schedule` 是 P2 加的，
`plugin`/`market`/`skill`/`subagent`/`expert`/`team` 更晚：

| 命令 | 干什么 |
| --- | --- |
| `hearth --version` | 打印版本，不读配置 |
| `hearth help` | 用法块 |
| `hearth doctor [--json]` | 十一项只读检查，顺序固定 |
| `hearth chat [flags] [prompt]` | 对着模型跑一轮，流式输出到 stdout |
| `hearth web [flags]` | loopback 上的本地浏览器界面 |
| `hearth index [--rebuild] [--stats] [--search TEXT]` | 查询或重建 SQLite 索引 |
| `hearth import <old-dir> [--dry-run]` | 只读导入旧格式的会话树 |
| `hearth session fork <id> [--at N] [--id NAME]` | 在指定 seq 处开一个新会话 |
| `hearth session archive <id> [--reason TEXT]` | 记录「这个会话不再开始新工作」 |
| `hearth goal start\|status\|end` | 记录、查看或关闭一个目标 |
| `hearth schedule add\|list\|remove\|next\|run` | 重复提示，以及打开它们的那个 tick |
| `hearth plugin list\|start\|stop\|call\|doctor` | 查看并驱动进程外插件 |
| `hearth expert list\|show` | Markdown 的专家与团队名册 |
| `hearth skill list\|show` | 列出或显示模型被提供过的技能 |
| `hearth subagent list\|tree\|resolve\|timing` | 一个会话下面的子代理：名字、树、解析、耗时 |
| `hearth subagent prompt\|interrupt\|providers\|worker` | 续跑一个可续子代理、停一个、列后端、跑 worker |
| `hearth team list\|run` | 让名册里的一个团队跑在共享任务板上 |
| `hearth market list\|search\|install\|remove\|verify` | 安装插件包，用 SHA-256 校验 |
| `hearth "<prompt>"` | 裸提示是 `hearth chat "<prompt>"` 的简写 |

`chat` 与 `web` 都从**同一个地方**（`crates/cli/src/wiring.rs`）构建工具集：
十个常驻工具（`fs_read`、`fs_write`、`fs_edit`、`fs_glob`、`fs_grep`、`read_image`、`shell`、
`todo_write`、`worktree`、`present`）与两个网络工具 `web_fetch`、`web_search`（构建不出来时**缺席**——
缺席的工具能被发现，永远失败的工具不能）；每个已配置 MCP 服务声明的工具（各自一条 `ask` 规则，
因为它跑在 hearth 外面）；每个已加载插件声明的工具加 `<插件>.<工具>` 前缀（也是 `ask`：调用离开了本进程）；
名册里有可召唤专家时的一个 `expert` 工具；`team.maxParallel > 0` 且名册里有团队时的一个 `team` 工具
（`ask`，因为每个 worker 都花钱）；至少找到一个技能时的 `skill` 工具；`subagent.maxDepth > 0` 时的
`subagent` 工具。`goal_end` 只在目标循环里（`hearth chat --goal`）加入，因为没人读的裁决不算工具。
一条坏掉的 MCP 条目、或一个加载失败的插件，是报告并跳过，而不是让运行时停下。

`read_image` 给模型看一个恰好是图片的文件。字节**不**进事件日志：工具回一次字节，循环把它作为
一条后续 user 消息转发，日志里只留 `meta.imagePath` / `meta.mediaType` / `meta.bytes`——
于是后续轮次从工作区重读这个文件，而不是让整个会话背着一份 base64。这也是为什么删掉那个文件，
模型就忘了那张图。浏览器里附加的图片从另一头走同一条路：正文在 `<dataDir>/blobs/`，
日志里只有引用。

`web_search` 需要一个 endpoint，因为没有 endpoint 的搜索提供商不存在。`config.json`：

```json
{
  "search": { "endpoint": "https://search.example/v1/search", "key": "..." }
}
```

请求是 `POST {"query": "...", "count": n}`，设了 key 时带 `Authorization: Bearer <key>`；
回复里的命中可以放在 `results`、`data`、`items`、`hits`、`web` 或 `web.results` 下，
每条命中的字段可以写成 `url|link|href`、`title|name|heading`、`snippet|description|content|text`。
别的一律报 `bad_content` 并附上回复前 400 字节，而不是被静默读成「没有结果」。

两个网络工具在 `read-only` 与 `workspace-write` 下都是 `ask`：它们跑在 harness 里、不在沙箱里，
所以审批提示是唯一的闸门。

## 桌面外壳

`apps/desktop` 是同一套 web 界面外面的一扇 Tauri 2 窗：它把 `hearth web` 当子进程起起来，
等服务器打印它正在服务的 URL，再在那个 URL 上开窗。它是一个**独立的 cargo workspace**，
所以 `cargo test --workspace` 与 CI 永远不需要为编一个窗口边框而装 GUI 工具链；单独构建它：

```powershell
cd apps\desktop
cargo build --offline --release
```

它自己的参数会转发给 `hearth web`（`hearth-desktop --model deepseek-chat`），
`HEARTH_BIN` 指定要起哪个二进制。关窗会杀掉子服务器，所以不会留下监听端口。
细节——包括为此付出的两个开窗缺陷——在 [apps/desktop/README.md](apps/desktop/README.md)。

界面本身是一个三栏工作台：文件树、Monaco 编辑器、agent 面板。文件树顶上有一个
搜索框：`GET /api/sessions/{id}/search?q=` 在整个工作区里按子串找行，点一条命中就
打开那个文件并跳到那一行。答案会说出它读过多少个文件、有没有触到上限，所以「没有
匹配」与「没看遍」不会穿同一句话。编辑器通过
`GET /api/sessions/{id}/files|file|diff` 读，通过 `PUT /api/sessions/{id}/file` 存，
通过 `POST /api/sessions/{id}/revert` 把改动扔掉、回到 git 已提交的版本；差异列表里
每一行都能展开成正行（统一 diff 文本，不在这边重新解析成 hunk），也可以切换成左右
两列（左是磁盘、右是提交，行号取自 hunk 头）。屏幕上的缓冲区不是
标签页里那个摘要取自的内容时，保存会被当作冲突拒绝，草稿留在屏幕上。一次保存追加一条
`file/write` 事件，一次还原追加一条 `file/revert`，都写明路径、字节数与内容的 SHA-256
——**不是** `tool/call`，因为这里没有模型参与，转录不许声称有。会话的沙箱模式是
`read-only` 时两者都直接拒绝，这样工具与编辑器永远不会用两种说法描述同一个会话。
git 从没提交过的文件不能被还原（`409`），因为对一个未跟踪文件来说「还原」等于删除。
模型做过的事仍然全部出现在转录与事件日志里。

## agent 不用猜就能知道的清单

`GET /api/capabilities` 不只是一份配置快照，它最后三组是**这个构建的词汇表**，在一台
什么都没装的机器上也成立：

| 字段 | 是什么 | 谁填的 |
| --- | --- | --- |
| `tools` | 无条件注册的内置工具，每行一句中文说明 | 装工具的那一方（CLI），因为列出来就要把它们构造出来 |
| `events` | `EventType::ALL` 里的每一种事件类型，按代码里的顺序 | 服务端，从构建本身取 |
| `routes` | 全部 HTTP 端点，一行一个 `"<方法> <路径>"`，路径用 `{占位符}` 写 | 服务端，来自 `crates/server/src/routes.rs` 的那张表 |

`events` 与 `routes` 由服务端自己填，所以进程被装配成什么都没配置时它们也不会是空
——否则面板会说「0 种事件类型」，读起来像构建坏了，而不是「少写了一行」。`tools`
只能由装了工具的那一方填，因为 `hearth-tool` 在 `hearth-server` 之上；插件与 MCP
工具**不在**这张表里，它们要看会话自己的 `llm/call` 事件。

表与真实分发的一致性是被测试守着的：`crates/server/tests/routes_table.rs` 会把
`ROUTES` 每一行都真的请求一次，只要哪一行答 `404 no such endpoint` 就红——一条活得比
它的端点更久的声明，会让 agent 去调一个 URL，然后把换来的 404 当成自己写错了。

## 文档地图

四个地方，各干一件事。按这个顺序读，是从「这是什么」到「我能改什么」的最短路径。

| 在哪 | 回答什么 | 什么时候打开 |
| --- | --- | --- |
| [README.md](README.md) — 本文件 | 这棵树是什么、怎么构建、每个 crate 管什么、不许管什么 | 第一个 |
| [AGENTS.md](AGENTS.md) | 一次改动必须遵守的规则：三法则、六反模式、依赖预算、推送方式 | 写代码之前 |
| [docs/README.md](docs/README.md) | 全部设计与执行文档的索引，按用途分组，每篇标明是记录还是计划 | 问「为什么」、或需要证据时 |
| [docs/contracts/](docs/contracts/README.md) | 七份冻结契约——事件格式、CLI 文本、HTTP/WS、M4–M6 的 crate 表面 | 改动别的实现也要对齐的东西之前 |

`docs/` 里有四类文档，索引会说明哪篇是哪类：**决策**（[08](docs/08-rust-port.md) 为什么是 Rust、
哪个 crate 在哪一层；[09](docs/09-platform-tier.md) 平台层缺什么；
[11](docs/11-adr-agent-hooks.md) 两个改写 seam）、**执行记录**（[10](docs/10-agent-parity.md) 波次计划、
[12](docs/12-subagent-port.md) 子代理移植、[agent-gaps](docs/agent-gaps.md) 差距台账）、
**对比**（[dsh-vs-hearth](docs/dsh-vs-hearth.md) 逐层读源码、
[dsh-comparison](docs/dsh-comparison.md) 在本机实测、
[agent-comparison](docs/agent-comparison.md) 两条 agent 循环）、以及
**运维**（[bench](docs/bench.md)、[ci](docs/ci.md)、[release](docs/release.md)、
[web-ui](docs/web-ui.md)、[office](docs/office-run.md)）。

## 验证

完整的门禁是一条命令：

```powershell
make check
```

等价于按顺序跑这五条：

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo run -q -p archcheck --offline
cargo test --offline
cargo run --release -q -p hearth-bench --offline -- -strict
```

架构检查干净时以 `archcheck: ok (.)` 结束；在完整的树上测试以 `test result: ok` 结束。
`make release` 只对已安装的交叉目标做编译检查——它不链接，因为本机没有它们的链接器。

`.github/workflows/ci.yml` 每次推送与 PR 跑四个 job：`test` 在 ubuntu 与 windows 上对着**最低**
rustc（1.89）跑，`sandbox macos-latest`（`sandbox-exec` 的证据，真 macOS 内核），`lint` 在当前 stable 上
（`fmt --check`、`clippy --all-targets -D warnings`），以及 `web`（`npm ci`、`tsc --noEmit`、
`vitest run`、`npm run build`，然后 `web/scripts/bundle-budget.cjs`——它把首屏压在 600 KiB gzip、
把懒加载那一堆（Monaco 与它的语法）压在自己的上限内）。用两套工具链是因为 clippy 会在新版本里长出新
lint：否则在 stable 上开发的人可以写出一个钉死版本的工具链永远看不见的问题——`% 2 != 0` 变成
`is_multiple_of` 就正是这件事。CI **刻意不**传 `--offline`（冷 runner 的 registry 是空的）；
本地的 `Makefile` 与 `check.ps1` 传，因为本机连不上 crates.io。
`.github/workflows/release.yml` 在版本 tag 上构建并算摘要。

两个 workflow 都真的在 GitHub runner 上跑过。每一次运行、各自抓到了什么、以及 CI 找到的
十一个本地门禁看不见的真缺陷，都记在 `docs/ci.md`。

下面的小节刻意把证据与限制分开：在本机做出的测量不是对另一台机器的承诺，
一件从未跑过的事不是一件能工作的事。

## 在本机验证过的，以及方法

下面每一条都由本机或 hosted runner 上的某条命令产出，段落里点名的那份文档写着数字与取数方式。

- 压缩在两条表面上都接了线（`hearth chat` 每轮之前、`hearth web` 在轮次的顶部，都沿用 Go 原版的
  200 条底线）。web 表面只折叠已经关闭的轮次，所以摘要覆盖的是用户已经看不见的历史、
  并且落在触发它的那一轮之内——待处理的提示在摘要之后被投影，和 CLI 上的顺序完全一致。
  回来的摘要是空的会被拒绝而不是追加（`crates/cli/src/web.rs` 的
  `web_compaction_refuses_an_empty_summary`）：一次空的折叠会删掉历史，却不在原地记录任何东西。

- 附件正文既被**存下**也被**看懂**。POST /messages 把上传写进 `<dataDir>/blobs/`，事件里留一个引用
  （从不留字节：日志只追加，分叉会逐字复制它），而 `crates/cli/src/attachments.rs` 在构造一轮时
  把一个 `image` 引用变回 `ContentPart::Image`，于是模型收到 `data:` URI、能看见这张图。
  字节每轮都从 blob 存储重读，而不是缓存进历史，所以日志里没有 base64、图片在整个会话里都可见。
  `read_image` 对工作区里的文件做同一件事。没有任何东西会把 `diff` 或 `file` 变成像素——
  它们保持为工具可以打开的引用。

- 基准移植在 `tools/bench`，记录了三份基线：本机 settled 树上的 windows/amd64，
  以及 GitHub runner 上的 linux/amd64 与 darwin/arm64。没有记录的平台打印一行 skip，
  而不是拿别的平台的数字来比。hosted 的 Windows 数字**不是**基线，因为它的 `fsync` 比本机慢 16 倍——
  一台 runner 是另一台机器，不是同一天的另一次测量。

- 语言中立的 HTTP、事件与 WebSocket 黄金语料与 Go 判据共享，在两棵树里都跑。
  WS 抓到过三个真的 Rust 服务端缺陷（增量 seq、控制 ping 的成帧、关闭码 1000），已修。

- Linux 的遏制（Landlock）每次推送都在运行时验证：Ubuntu 的 CI job 跑
  `crates/sandbox/tests/linux.rs`，它在工作区内写成功、在外被拒、在 `read-only` 下写被拒、
  按名字授权的 `extra_write` 路径可用、`allow_net = false` 时被内核拒绝（`allow_net = true` 时 `connected`）。
  macOS 的 `sandbox-exec` 受同样待遇：`sandbox macos-latest` job 跑 `crates/sandbox/tests/macos.rs`，
  在真 macOS 内核上断言模式矩阵、工作区写范围、`read-only`、`extra_write` 与两个方向的网络。

- **在 Windows 上遏制是两个环，第二个环真的收窄了工具能做的事。** Job Object 给进程树、
  单进程/整作业内存与进程数封顶；AppContainer 决定子进程能碰哪些文件与网络
  （对工作区的写权限，只有当 `allow_net` 时才有 `internetClient`）。本机实测：容器里的命令能写自己的
  工作区、写同级目录被操作系统拒绝、loopback 连接被拒。代价是真的——容器里的进程可能读不到它被启动时
  所在的构建目录，而且启动时**别的什么都不许读**，于是那些依赖 home 目录的工具（git、npm、pip）
  会在原本能用的地方失败。修法是一张清单而不是一个洞：`crates/cli/src/grants.rs` 授予机器上真的存在的、
  属于工具的路径的读权限（`~/.gitconfig`、`~/.cargo`、`~/.npmrc`、`%APPDATA%\npm`、`%APPDATA%\pip`、
  `~/.config`）与 `%TEMP%` 的读写，`config.json` 里的 `sandboxAccess.read` / `sandboxAccess.write`
  替换这张清单（显式空数表示「不额外授予任何东西」），而 `sandboxAccess.network: true` 是第二个开关：
  没有它，容器里的子进程根本没有 socket。本机验证方式：在容器里读 `~/.gitconfig` 成功、读它旁边一个
  未授权的文件失败（`windows_reads_are_granted_only_where_asked`），以及在容器里跑真的
  `git config --global --get user.name`——只有在授权存在之后才打印出配置里的名字。工作区之外的被授权路径
  还需要它上面每一层目录的 `FILE_TRAVERSE`，否则工具在打开任何东西之前就死在
  `failed to stat '...AppData\Local': Permission denied`——所以授权会走祖先目录，跳过盘根
  （改写 `E:\` 的 DACL 实测 160 s），并在被拒处停下。那次写入不是免费的：Windows 会重算整棵子树的继承，
  实测 `%TEMP%` 9 s、`AppData\Local` 11 s、`AppData` 20 s、用户目录根 25 s。每个目录每个身份只付一次——
  重复授权靠先问 ACL 来跳过，超过两秒的停顿会打印它在做什么。每个 workspace 路径会在
  `%LOCALAPPDATA%\Packages` 留下一个 profile（永久，按路径键）；`hearth doctor --prune-profiles` 删掉它们——
  这台机器上删了 106 个，之后计数读回 0。那个服务是这段故事里唯一不可靠的部分：
  在一台攒够这些 profile 的机器上，它会连续几分钟干脆拒绝**新**名字，所以已存在的 profile 靠 lookup 采纳、
  永不重建；一次抖动会重试约八秒；如果新身份仍然造不出来，遏制退回到该用户的一个共享身份——
  它仍然把命令关在授予它的目录里，但不再把两个 workspace 彼此隔开。最后这一手是**说出来**的，
  不是静默跳过的。删也撞同一堵墙：106 个删掉半小时后，同一条命令答
  `57 found, 0 removed, 57 failed`，每个名字都是 `0x80070005`，而那时新建也在失败——
  服务是整体拒绝，不是一个固执的 profile。所以 prune 只把重试预算花一次当探针，如果第一个名字被拒且一个都没删掉，
  它就停下并说「服务现在在拒绝，过几分钟再试」（3.9 s），而不是磨三分钟再报同一个数字。
  造不出包身份的主机（或没有 AppContainer 的 Windows 版本）会拿到 `filesystem=false` 的 WARN，
  并且 `read-only` 被拒绝而不是降级。这样一台主机比看上去更容易造出来：
  **一个进程以它自己镜像的完整性级别运行，当那个标签低于它的父进程时**，于是一个标了 `Low` 的目录下
  （浏览器解压出来的下载就是这个标记）的每个二进制都以 `Low` 运行，而 Low 进程写不了 profile 服务建 profile 的
  `%LOCALAPPDATA%`。症状是从那些与沙箱命令毫无关系的调用里返回 `ERROR_ACCESS_DENIED`，
  它让这个项目花了一个下午去追「非 ASCII 路径」——因为每个失败的测试都从带标签的树里跑，
  每个通过的都从 `%TEMP%` 跑。现在的拒绝会点名这个条件并打印补救办法
  （`icacls <dir> /setintegritylevel (OI)(CI)M /T /C`），而 `available()` 会把机制报成缺席，
  而不是让严格模式声称它交付不了的遏制。

- web 转录在真浏览器里量过：5,000 条消息（10,001 个事件、480,130 px 滚动高度）在挂载 27 行的状态下
  平稳 **60 fps** 滚动，加载约 650 ms，堆约 18 MB。数字、方法与这次测量暴露的三个只属于浏览器的缺陷
  在 `docs/web-ui.md`。那是 Windows/amd64 上的一个无头 Edge；别的浏览器或 GPU 没有量过。

- `LICENSE` 是 MIT。`Cargo.toml` 在它是私有移植期间带的 `UNLICENSED` 已经没有了；
  这棵树可以按 `LICENSE` 的条款再分发。

## 真正的限制

这些是这个构建的性质，不是待办清单。关掉任何一条都意味着改变这个产物所声称的东西，
所以没有一条是「悄悄修掉」的 bug。

- Rust 的 provider 通过钉死的 `webpki-roots` 包接受 `https://`，另外可以用一个可选的 `ca_file` PEM
  （见 `crates/llm/README.md`）配私有 CA。移植期间没有对示例端点发过真实请求。

- 从本机做交叉目标 release 构建只是编译检查；见工具链一节与 `docs/release.md`。
  CI 用它声称的产物的原生 runner。

- `archcheck` 只检查层方向、README 契约、帮助文本指向的路径、以及容器目录有没有说明书。
  它不读 Rust 源码，不强制依赖预算，也不检查一个 crate 的公开 API 与它的 README 是否一致；
  那些是评审与 `Cargo.toml` 的事。

- `make check` 在作者机器上跑不了，因为没装 GNU make。它的分步命令被直接跑过，并且就是上面那些
  预期输出的来源；`check.ps1` 在 Windows 上跑同样的五步。

## 关于参照文档的说明

- 冻结契约（`docs/contracts/`）是在 Go 还是参照物的时候写的，所以里面的代码样例是 Go；
  它们钉住的行为才是 Rust crate 实现的东西，证据是 `tests/conformance/cases/` 下的语料。
  `src/` 里没有 `unsafe`（每个 crate 都 deny 它）。