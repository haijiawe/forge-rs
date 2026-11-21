# 交接 —— 现在站在哪，什么东西会咬你

这是你接手这棵树第一天上手要读的文档。它是记录，不是决策：它描述交接提交时的状态、刻意没造的东西，以及那些已经吃掉某人几个小时的坑，免得它们再吃你同样的几个小时。

这里的每一条都在本机上测过。凡是在本机测不了的主张，它就写明测不了，而不是猜。

## 这是什么

`hearth` 是一个本地优先的编码 agent harness：一个 Rust 二进制，在一条只追加的事件日志上驱动模型，外加由同一个二进制提供的本地 Web UI。关于一个会话的每一个事实都是 `<dataDir>/sessions/<id>.jsonl` 里的一行，按顺序，永不重写——见 [../AGENTS.md](../AGENTS.md) 里的三条法则。

二十四个 crate 分四层，由 [../architecture.yaml](../architecture.yaml) 与 [../tools/archcheck](../tools/archcheck) 强制。冻结的接口面是 [contracts/](contracts/README.md)；[../tests](../tests) 下的语料则是证明各读取器守得住它们的东西。

## 交接提交时的状态

| | |
| --- | --- |
| HEAD | `f4ccae4`，与 `origin/master` 一致，工作区干净 |
| 提交数 | `master` 上 88 个（这棵树丢过一次历史又找回来一次——见坑） |
| crate 数 | 24 个（`crates/*`），外加 `tools/archcheck` 与 `tools/bench` |
| Rust | 279 个被跟踪的 `.rs`，含测试约 109k 行 |
| 测试 | 整个 workspace 1425 通过、0 失败（`cargo test --workspace --offline`） |
| 二进制 | `bin/hearth.exe`，9,180,160 B，`hearth 0.0.0-dev (unknown) windows/amd64 rustc 1.96.0` |
| 门禁 | `check.ps1` → `check: ok`（fmt、clippy `-D warnings`、archcheck、workspace 测试） |
| CI | 六个 job；这个提交上的最近一次运行全绿（ubuntu、windows、lint、what-changed；web 与 macos 按设计跳过） |
| 许可证 | MIT（`LICENSE`、`Cargo.toml`） |

`bin/hearth.exe` 是提交进仓库的，**不是** cargo 产物：它内嵌了构建好的 web bundle。在信任它之前先看下面的"重建二进制"。

## 东西都在哪

```
D:\hearth            这棵树（git repo，remote 为 haijiawe/hearth）
D:\reference         只读资料：DSH rc.5 源码、品牌素材、工具
D:\_agent-scratch    更早会话归档的日志（可以安全删掉）
```

`D:\reference\deepseek-harness-master` 是 DSH 0.2.2-rc.5 的源码。它是 [dsh-vs-hearth.md](dsh-vs-hearth.md) 与 [agent-comparison.md](agent-comparison.md) 的参照；本树里没有任何东西 import 它。

## 构建与验证

```powershell
cargo test --workspace --offline
cargo run -q -p archcheck --
pwsh -NoProfile -File .\check.ps1      # 全部门禁，按 CI 顺序；热态约 40 s
```

两个事实，都实测过，都不可协商：

- **只能用 MSVC，且离线。** GNU 工具链装着但是坏的（`dlltool.exe: program not found`），crates.io 不可达；`--offline` 读的是 rsproxy 镜像。
- **构建路径必须是 ASCII。** `D:\hearth` 没问题，什么都不用设。如果你把树挪到路径里带非 ASCII 字符的地方，先把 `CARGO_TARGET_DIR` 指到 ASCII 目录，否则每个要编译 C 的 crate 都会以 `cl: D8050` 失败。

### 重建二进制

```powershell
cd web; npx vite build; cd ..
cargo build --release -p hearth-cli
Copy-Item .\target\release\hearth.exe .\bin\hearth.exe -Force
```

第一步不是可选的：`hearth-cli` 内嵌 `web/dist`，所以没有它构建出来的二进制提供的是上一版 UI。`npx tsc --noEmit` 与 `npx vitest run` 住在 `web/` 里，必须在其中运行——从仓库根跑，`npx` 会找到错的 `tsc`。

## 发布

`git push` 与 `git fetch` 在这个网络上打 github.com:443 都会死（连接重置）。`origin/master` 改为通过 REST API 维护：

```powershell
node scripts/sync.cjs "commit message"   # 提交、推送、对齐
node scripts/sync.cjs --no-commit        # 推送并对齐当前 HEAD
node scripts/sync.cjs --dry "message"    # 只说会做什么，不写任何东西
```

第三步是大家会忘的那一步。通过 API 创建的提交是一个新对象，所以即使树逐字节相同，它的 SHA 也和本地不同；`sync.cjs` 在本地重建那个对象（API 会把时间戳归一到 `Z` 并丢掉消息末尾的换行，所以它两个维度都试），并且**拒绝移动分支**，除非重建对象哈希到 GitHub 报出的那个 SHA。它从 `$GITHUB_PAT`，或 `$HEARTH_CREDENTIALS`（默认 `D:\dsh\.credentials.yaml`）里的 `GITHUB_PAT:` 读 token，且从不打印它。

如果你哪天把 `git push` 弄通了，删掉 `scripts/sync.cjs`，回到裸 git——这个脚本只是为了绕开一台机器的网络。

## 造了什么

下面每一波都已收尾，并且在本目录里有自己的记录。"收尾"指的是记录里的验收标准跑过，而不是功能已经穷尽。

| 波次 | 加了什么 | 记录 |
| --- | --- | --- |
| M0–M7 | 移植本身：信封、日志、投影、工具、沙箱、服务端、CLI、发布流水线 | [08-rust-port.md](08-rust-port.md) |
| P1–P5 | `hearth-net`、`web_fetch`/`web_search`/`worktree`/`present`/`read_image`、把附件移出日志、capabilities 端点、插件、市场、专家名册、团队 | [09-platform-tier.md](09-platform-tier.md) |
| W1–W6 | 结构化子代理输出、goal 修订/CAS、压缩 `surfaceOp` + 剪枝器、两个重写缝、团队任务图 | [10-agent-parity.md](10-agent-parity.md) |
| A–E | header 级委派深度、每步 goal 栅栏、压缩事务、`request-error` 缝、跨进程 `interrupt` | [10-agent-parity.md](10-agent-parity.md) |
| S1–S5 | 子代理标识（`catalog`/`descriptor`）、有界池、冷恢复、settled 通知、进程外 worker 后端 | [12-subagent-port.md](12-subagent-port.md) |
| G1–G8 | attempt 记账、通用监听面、常驻阶段机、持久 inbox 队列、steer、异步 settlement、具名后端、团队板投影 | [agent-gaps.md](agent-gaps.md) |
| H1–H7 | `request/context`、`cancelledBy` 原因、goal 的 `domain`/`fold`/`runtime` 拆分、队列排空、会话归档、不可变性证据、每个 Web 会话一个常驻 agent | [agent-gaps.md](agent-gaps.md) |
| I1–I3 | 字节游标之后的增量投影、增量转录扫描、workspace 读取 API（`files`/`file`/`diff`） | 本文件 §"IDE 转向" |
| D1 | Tauri 2 桌面壳，以及它打开的三栏工作台 | [apps/desktop/README.md](../apps/desktop/README.md) |

### IDE 转向

交接之后工作转向了工作台而不是聊天页，这改了三件事，而 harness 本身没动：

- **一个 turn 不再重读整条日志。** `Agent::snapshot` 以前每个 turn 都调 `read_all` 把每个事件重新投影一遍——在本机上实测 88,916 events/sec，所以 100,000 个事件在向模型问任何话*之前*就要花 1.8 s，500,000 个要花 9 s。`Store::read_after` 加上一个压在字节游标后面的 `Projection`，让一个 turn 的代价等于这个 turn 新增的部分；末尾被截断的半行会留给下一次调用，而不是被消费掉。
- **转录是增量扫描的。** `compressedBlocks` 以前每个动画帧都走一遍所有事件；`collectBlocks`/`blocksFromScan` 保留一个扫描位置，而 `toBeyondHead` 的结论是重算而不是缓存，因为它随着 head 增长会翻转。
- **窗口能读 workspace**，走 `GET /api/sessions/{id}/files`、`/file` 与 `/diff`——它们从 `session/start` 解析出 workspace，拒绝 `..` 与符号链接逃逸，文本上限 1 MiB，且从不写入。
- **窗口也能把它写回去**，走 `PUT /api/sessions/{id}/file`，前面挡着一个乐观的 `expectedSha256` 校验和一个 Monaco 编辑器。一次保存追加一条 `file/write` 事件——刻意不是 `tool/call`，因为没有任何模型参与，而一份说反了的转录是没法读的；被拒的保存（会话只读、路径逃逸、摘要过期）则什么都不追加。Monaco 是打包进来的，不是从 CDN 拉的：桌面壳没有网络。

## 刻意没造的东西

这些是决策，不是待办。关掉其中一条会改变这个产物声称什么，所以每条都需要自己的 ADR，而不是一个安静的提交。

- **纯 Rust 的 Office 写出器。** `office` 技能驱动一个外部程序并且明说了；[office-run.md](office-run.md) 记录了产出了什么。
- **[09-platform-tier.md](09-platform-tier.md) §6 里的非目标**：浏览器内终端复用器、ACP agent 协议、Office 的*渲染*、图像生成模型，以及任何 DSH/DS 服务器依赖。这棵树按设计是进程内 Rust。桌面壳曾在这张清单上，后来按需造了出来；它打开的编辑器现在也能保存，因为一次保存走的是 `PUT /api/sessions/{id}/file`——带 `expectedSha256`、落一条 `file/write` 事件并按只读会话、路径逃逸、过期摘要拒写——这是"一个人按键"的动作，不是模型的动作，所以它既不绕过策略也不绕过审批提示，而 `file/write` 事件正是让它可复核的东西。
- **DSH 实现方式的 `archive-admission`** 被替换成一条真正的 `session/archived` 事件，加上在新工作进来的两道门口做的检查（[agent-gaps.md](agent-gaps.md) 解释了为什么这里没有否决面）。

## 真实限制 —— 这个构建不做的声称

- `.github/workflows/release.yml` 里的 `gh release create` **从未执行过**。从本机做的跨目标构建是编译检查；CI 用原生 runner。
- 比 CI 的 ubuntu 镜像更老的内核、比 runner 更老的 macOS 版本，都没有测。
- ETXTBSY 的 spawn 重试只在 CI 的 ubuntu 上验证过。Windows 与 macOS 走的是直通分支，所以本地门禁绿只证明"没有回归"，不证明"重试有效"。
- `scripts/sync.cjs` 依赖 API 的元数据能精确复现一个提交对象。如果 GitHub 改了那个格式，它会 fail closed（不移动分支），需要的是新的候选格式，不是手改 SHA。
- 移植期间没有对示例模型端点做过真实请求；provider 是对着脚本化的本地服务器演练的（完整清单见 [../README.md](../README.md) 的 "Real limits"）。
- **`web/src/api/protocol.ts` 不再是生成的。** 它镜像 `crates/protocol/src/protocol.rs`，两者必须在同一个提交里改；过去生成它的 Go 生成器没有跟过来。两份冻结文档（[contracts/m6-surface.md](contracts/m6-surface.md) 与 [contracts/api.md](contracts/api.md) §1）里仍然写着 `go run ./scripts/gen-protocol`，因为它们描述的是 Go oracle，而它们正是对着它冻结的；那些不是给本树的指令。

## 已经吃掉过某人几个小时的坑

第一次动手改东西之前先读这些。每一条都是真实事故，不是抽象警告。

1. **永远不要用 PowerShell 字符串插值去构造 Rust 或 Markdown 字符串。** 那里反引号和反斜杠是转义符，而且损伤是静默的：一个孤立的 CR、BEL（`0x07`）或 FF（`0x0C`）曾经落进 `agent.rs` 和 `cli/lib.rs` 的注释里，还活过了 fmt、clippy 和 review，因为 `clippy` 不检查那些字节。用编辑器工具或脚本文件。怀疑就查：`Select-String -Pattern '[\x00-\x08\x0b\x0c\x0e-\x1f]' -Path crates -Recurse`。
2. **Linux 上的 `ETXTBSY`。** 只要有*任何*进程以写方式打开着文件，`exec` 就会被拒，而兄弟线程的 `fork` 会在 `exec` 之前的那一瞬继承写者的描述符。`crates/plugin/src/connection.rs` 里的 `spawn_retrying_busy` 会重试一秒。同样的形状会出现在任何"刚写完二进制就立刻运行"的地方。
3. **不是 bug 的并行测试抖动。** `hearth-mcp --test http` 与 `hearth-ws --lib` 在 `check.ps1` 里可能失败（端口竞争），单独跑就过。在"修"它们之前先重跑。靠放松断言来修抖动，就是真实回归混进来的方式。
4. **`Event::new` 会把 `seq = 0` 留下来。** 你手写的日志必须自己给密集的 `seq`、32 位十六进制的 `id` 和毫秒级 `ts`，否则读取器会报 `not dense`，测试看起来像"记录从来没被写下来"。
5. **`Config::load` 不调用 `validate`。** 负值与自相矛盾的测试必须显式调 `cfg.validate()`。
6. **挪动这棵树可能毁掉 `.git`。** 当 shell 的 cwd 在源目录里面时，`Move-Item` 会退化成半途而废的复制；删掉那份副本把 `.git` 一起带走了，83 个本地提交只好从远端重建。先把 cwd 挪出去，再比较文件数和抽样哈希，并单独检查 `.git`。
7. **改名让文档一致了，文件有时没有。** 当 `forge` 变成 `hearth` 时，正文里的引用被重写了，但某个文件名没有，于是索引指向 `dsh-vs-hearth.md` 而文件还叫 `dsh-vs-forge.md`。如果你要改名，改完之后把两种拼写都 grep 一遍。
8. **`cargo fmt` 会重排你正要改的那个测试。** 格式化之后把文件读回来，再拿某一行精确文本去锚定编辑。
9. **一条 CSS 规则和一个 `className` 对不上时，失败是静默的。** 工具卡片的头是 `className="tool-head"`，而样式表定义的是 `.tool-card-head`；`.tool-callid` 对的是 JSX 里的 `tool-call-id`。没有任何报错，`gap` 就是不生效，渲染出来是 `fs_writecall_smoke_1`。不抛异常的布局 bug 靠看截图找，不是靠读那两个文件。
10. **flex item 不会缩到比它最长的单词更窄。** `.session-head` 在一个 flex 行里，默认 `min-width: auto`，于是一条长错误消息把整个文档撑得比窗口还宽，把 agent 面板挤出了屏幕。修法是在会变大的那个 item 上加 `min-width: 0` 加 `overflow: hidden`；CSS 审查找不到这个，按真实窗口尺寸截一张图才找得到。
11. **WebView2 窗口必须在跑事件循环的那个线程上创建。** 从 worker 线程建出来的窗口会把页面完全加载好——`#root` 有 7,819 字节的 markup、267 条 CSS 规则、空的错误列表，全是透过调试端口读到的——然后永远画纯黑。在主线程之外等一个子进程是可以的；在那里创建窗口不行，所以 `apps/desktop` 用 `run_on_main_thread` 跳回来。
12. **一张截图可能与错的那一层一致。** 同一个页面在 headless Edge 里渲染正常，而桌面窗口是黑的，这曾经说明"UI 没问题"；反过来那个坑是读到一个空 DOM 就去怪浏览器。当图和 API 不一致时，先查页面本身（`--remote-debugging-port` 加 `Runtime.evaluate`），再动任何一边。
13. **你没法通过 DevTools 协议往 Monaco 里打字。** Monaco 0.54 监听的是 `textarea.ime-text-area`，而 Chrome 146+ 把文本走 `EditContext`，所以 `Input.insertText` 和带 `text` 的 `Input.dispatchKeyEvent` 都会报成功却什么都不插入。要导入页面已经加载的那个 bundle——`await import('/assets/monacoSetup-*.js')` 命中模块缓存并返回同一个单例——然后调 `monaco.editor.getEditors()[0].setValue` 来走真正的 change handler。`window.monaco` 是 `undefined`；bundle 不发布全局变量。

## 先做的三件事

1. `pwsh -NoProfile -File .\check.ps1` —— 如果它不是 `check: ok`，停下来先把环境修好，再读任何代码。
2. `.\bin\hearth.exe doctor` 与 `.\bin\hearth.exe web --addr 127.0.0.1:8811` —— UI 是看日志、投影和工具是否一致的最快办法。token 打印在 **stderr** 上。
3. 规划任何东西之前先读 [agent-gaps.md](agent-gaps.md)。它是"DSH 有而本树没有"的账本，每项旁边带着代价，而且是那份对比唯一的书面出处。

## 交回去

这棵树期待的形态，来自 [../AGENTS.md](../AGENTS.md)：报 `done`（文件 + 行数）、`evidence`（命令与输出尾段）、`open`（什么没做、为什么）、`risk`（什么可能咬到下一个接手的人）。你没跑过的门禁不是通过了的门禁。