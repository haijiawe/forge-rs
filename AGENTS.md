# AGENTS.md —— 改任何文件之前先读这一页

这棵树是某个 Go 实现的 Rust 重写版，那个实现**已于 2026-11-17 删除**。下文注释里
出现的 `forge-go/...` 指的是那棵已退役树里的文件；它们在这里解析不到，这是预期的。
Go 树定义过的东西现在冻结在 `docs/contracts/` 与 `tests/` 下的语料里，测试比对的就是
它们。

## 三条法则

1. **事件日志是唯一的真相。** 关于一个会话的每一个事实，都是
   `<dataDir>/sessions/<id>.jsonl` 里按顺序追加的一行，永不重写。如果某个行为不在
   日志里，重启就会丢掉它——而用户有权重启。
2. **读者必须能读懂一份从没见过的日志。** 未知的事件类型是数据（跳过并计数）。
   未知的**版本**是错误（拒绝这个文件，不要碰它）。payload 里的成员由契约描述，
   从不由读者强制。
3. **该 fail closed 的地方就 fail closed，并且说清你没做什么。** 一个无法约束的
   沙箱必须拒绝启动，而不是安静地不受约束地启动。一个跑不了的检查是 `SKIP`，
   永远不是 `OK`。一波没做完就不许标成做完。

## 东西都在哪

二十四个 crate，四层。`architecture.yaml` 是权威；下面这张表是同一份信息，外加
每个 crate 的用途——因为层号不能告诉你一个新文件该放哪一格。

| 层 | Crate | 它拥有什么 |
| --- | --- | --- |
| L0 | `crates/core` | 事件信封、id、规范 JSON、错误种类、摘要 |
| L0 | `crates/config` | 配置文件、home 目录、默认值、校验 |
| L1 | `crates/exec` | 起进程、杀进程树、沙箱钩子 |
| L1 | `crates/sandbox` | job object / Landlock / sandbox-exec / AppContainer |
| L1 | `crates/mcp` | MCP 客户端：stdio 与 HTTP 两种传输 |
| L1 | `crates/skill` | 技能发现、frontmatter、扫描 |
| L1 | `crates/net` | HTTP GET/POST、TLS 根证书、HTML→文本 |
| L1 | `crates/experts` | Markdown 写的专家/团队名册 |
| L2 | `crates/session` | 日志存储、投影、压缩、fork、二进制块 |
| L2 | `crates/llm` | provider 契约、SSE 客户端、内容分段 |
| L2 | `crates/tool` | 注册表、执行器、策略、审批、内置工具 |
| L2 | `crates/protocol` | 与界面共享的 wire 类型 |
| L2 | `crates/agent` | 轮次循环、历史、钩子、收件箱 |
| L2 | `crates/store` | SQLite 索引与它的检索 |
| L2 | `crates/import` | 导入外来的会话日志 |
| L2 | `crates/subagent` | 目录、描述符、后端、归档、权限 |
| L2 | `crates/goal` | 目标的领域模型、事件、折叠、运行时认领 |
| L2 | `crates/schedule` | rrule 子集与时刻表 |
| L2 | `crates/market` | 索引、摘要校验的安装、能发现篡改的卸载 |
| L2 | `crates/plugin` | 讲 stdio JSON-RPC 的插件宿主 |
| L2 | `crates/team` | 共享工作板、并行 worker、任务图 |
| L2 | `crates/context` | 压缩块、裁剪、账本 |
| L3 | `crates/server` | HTTP/1.1 + WebSocket，全部路由 |
| L3 | `crates/cli` | `hearth` 二进制与它的子命令 |

不是 crate、但同样是这张地图一部分的东西：

| 路径 | 是什么 | 规矩 |
| --- | --- | --- |
| `web/` | React 界面，由 Vite 构建进 `web/dist` | 按决定，界面依赖为零；`web/src/api/protocol.ts` 必须与 `crates/protocol` 在同一次提交里改 |
| `apps/desktop` | Tauri 2 窗口，自己的 cargo workspace | 谁都不许 import 它；不许进 `check.ps1`（否则 Linux CI 要装 webkit2gtk） |
| `tests/` | 语言中立的判据：一致性用例、fixture、golden | 按法则只读——永远不要改用例去让某个 reader 通过 |
| `skills/` | 随树一起发布的技能 | 技能是数据不是代码；不许 import 任何 crate |
| `examples/` | 能跑的样例（目前是一个插件） | 要么一直能跑，要么删掉 |
| `tools/` | `archcheck`、`bench` | 只在构建期用；这里的东西不进二进制 |
| `scripts/` | `sync.cjs`、安装脚本 | 运行时不许依赖它们 |
| `docs/` | 决策、记录、对比、运维 | 哪份是哪类见 `docs/README.md` |
| `bin/` | 发布用的二进制，手工重建 | 不是 cargo 产物——见 `docs/13-handover.md` |

### 我要加东西，该放哪

| 你想 | 放这 | 因为 |
| --- | --- | --- |
| 加一个 agent 工具 | `crates/tool/src/builtin/`，然后在 `policy.rs` 里点名 | 没被点名的工具会被兜底规则静默拒绝 |
| 加一个 HTTP 路由 | `crates/server/src/<x>_route.rs` + `handle_api` 里一个分支 | 路由表刻意就是一个函数 |
| 加一个 wire 类型 | `crates/protocol/src/protocol.rs` **和** `web/src/api/protocol.ts` | 没有生成器；漂移是静默的 |
| 加一个事件类型 | 五处，缺一处就有一条谎：`EventType::ALL`、`docs/contracts/events-v1.md` 的目录表行与 §4 小节、`tests/conformance/cases/events/` 一个用例、`crates/core/tests/conformance.rs` 的用例计数、`docs/README.md` 的语料计数 | 法则 2 让类型可加，而用例才是证明；`crates/core/tests/contract.rs` 会替你核对表格与计数 |
| 加一个平台集成 | 新的 L1 或 L2 crate | crate 才是边界；模块不是 |
| 加一个由操作者打开的能力 | 一个 `Config` 段 + `wiring.rs`，绝不默认开启 | 缺一个工具是可发现的；一个永远失败的工具不是 |
| 加一个界面面板 | `web/src/components/`，不先问就不加 npm 依赖 | 那份预算是一份决策记录 |

分层图不是建议。`architecture.yaml` 声明它，边指向上层时 `make arch` 会让构建失败。
一个 crate 就是一个边界：如果你动了「只 import 上层一样东西」的念头，答案是下层
crate 里的一个 trait，不是一句 import。

## 构建与验证

```powershell
cargo test --offline          # 单测 + 一致性测试
cargo run -q -p archcheck --  # 依赖门禁
cargo clippy -- -D warnings
node scripts/check-links.cjs  # 文档链接与反引号里的仓库路径
make check                    # 以上全部，顺序与 CI 一致
```

`check-links.cjs` 查两样东西：markdown 里的相对链接，以及**反引号里写出来的仓库路径**。
后者是因为实测过：239 条相对链接全部解析成功的同时，散文里有四处指向不存在的文件。
它只把「顶层目录名 + 已知后缀或以 `/` 结尾」的 token 当路径，所以 `tools/call` 这种
JSON-RPC 方法名不会被误判；指向已退役的 Go 树或 DSH 仓库的路径必须在同一行写明来历，
否则照样报错——一个不带限定的 `docs/…` 旧文档名会让读者去本树的 docs 里找，然后什么都不信。

两条环境事实，都实测过，都不可商量：

- **构建路径必须是 ASCII。** 这份检出在 `D:\hearth`，所以树内的默认 `target/` 就行，
  什么都不用设。如果你把树搬到路径里带非 ASCII 字符的地方，先把
  `CARGO_TARGET_DIR` 指到一个 ASCII 目录——否则每个编 C 的 crate（bundled 的
  rusqlite、ring）都会以 `cl: D8050 ... 未能将命令行放入调试记录中` 失败。
  `.cargo/config.toml` 解释了为什么这个设置不写在文件里。
- **只用 MSVC，且离线。** GNU 工具链装了但是坏的（`dlltool.exe: program not
  found`），而且 crates.io 不可达；`--offline` 读的是
  `D:\Rust\.cargo\config.toml` 里的 rsproxy 镜像。

## 推送

在这张网络上 `git push` 和 `git fetch` 打 github.com:443 都会死（连接被重置），
所以 `origin/master` 是靠 REST API 维护的。一条命令做完整个循环：

```powershell
node scripts/sync.cjs "提交信息"                      # 提交、推送、对齐
node scripts/sync.cjs --message-file note.txt        # 提交多段式信息
node scripts/sync.cjs --no-commit                    # 只推送并对齐当前 HEAD
node scripts/sync.cjs --dry "信息"                   # 说会发生什么，什么都不写
```

比一行标题长的东西就用 `--message-file`：反引号、引号和空行在文件里能活下来，
而在通到这里之前的每一个 shell 里都活不下来。

第三步很重要：经 API 创建的提交是一个新对象，所以即使树完全一样，它的 SHA 也与本地
不同。`sync.cjs` 会在本地重建那个对象（API 会把时间戳归一到 `Z`，所以它会试几个可能
的时区偏移），并且除非重建出来的对象哈希等于 GitHub 报的那个 SHA，否则拒绝移动分支。
它跑完之后，`git status` 会显示 `master...origin/master`，前不领先、后不落后。

token 来自 `$GITHUB_PAT`，或者 `$HEARTH_CREDENTIALS`（默认
`D:\dsh\.credentials.yaml`）里的 `GITHUB_PAT:`。它永远不会被打印出来。

## 任务卡

写代码之前，先在脑子里（或任务里）写下四行：

```
goal:      这件事做完时什么叫成真
scope:     哪些文件允许改
accept:    哪条命令、期望什么输出
unknowns:  哪些东西你会如实报告而不是编出来
```

## 按改动类型定义的「完成」

| 改动 | 完成的含义 |
| --- | --- |
| 新 crate | 带 frontmatter 与四个小节的 README；`make arch` 绿 |
| 新事件类型 | 上面「我要加东西」表里那五处都改到；`cargo test -p hearth-core --test contract` 会核对表格与计数 |
| 修 bug | 一个在旧代码上失败、在新代码上通过的测试 |
| 性能声明 | 这台机器上测出来的数字，以及产生它的那条命令 |
| 平台声明 | 要么在这台机器上测过，要么标成 `本机未验证` |

## 交接的形状

按这个顺序报告：`done`（文件 + 行数）、`evidence`（命令 + 输出尾部）、`open`
（什么没做、为什么）、`risk`（什么会咬到下一个接手的人）。永远不要报告一个你没跑过的
门禁。

## 六条反模式

1. 不要为了某个 reader 通过去改 `tests/conformance/cases/**`。用例就是契约；
   如果它错了，那两边的实现都错了。
2. 不要不问就加依赖。`Cargo.toml` 里的预算是一份决策记录；每一行都在回答
   「标准库为什么做不到这件事」。
3. 不要「为了以后」加 `tokio`（或任何异步运行时）。服务器是每连接一个线程，
   因为负载是一个项目一个进程。
4. 不要在库代码里对 I/O 或解析结果写 `unwrap()`。测试可以。
5. 不要声称一个沙箱在约束它并没有约束的东西。报告 `filesystem=false`，并说清缺什么。
6. 不要让 `help` 列出跑不通的命令。少一行代价是一个问题，骗人的一行代价是一个下午。