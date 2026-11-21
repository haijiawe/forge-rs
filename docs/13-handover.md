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

交接之后工作转向了工作台而不是聊天页，这改了几件事，而 harness 本身没动：

- **一个 turn 不再重读整条日志。** `Agent::snapshot` 以前每个 turn 都调 `read_all` 把每个事件重新投影一遍——在本机上实测 88,916 events/sec，所以 100,000 个事件在向模型问任何话*之前*就要花 1.8 s，500,000 个要花 9 s。`Store::read_after` 加上一个压在字节游标后面的 `Projection`，让一个 turn 的代价等于这个 turn 新增的部分；末尾被截断的半行会留给下一次调用，而不是被消费掉。
- **转录是增量扫描的。** `compressedBlocks` 以前每个动画帧都走一遍所有事件；`collectBlocks`/`blocksFromScan` 保留一个扫描位置，而 `toBeyondHead` 的结论是重算而不是缓存，因为它随着 head 增长会翻转。
- **窗口能读 workspace**，走 `GET /api/sessions/{id}/files`、`/file` 与 `/diff`——它们从 `session/start` 解析出 workspace，拒绝 `..` 与符号链接逃逸，文本上限 1 MiB，且从不写入。
- **窗口也能把它写回去**，走 `PUT /api/sessions/{id}/file`，前面挡着一个乐观的 `expectedSha256` 校验和一个 Monaco 编辑器。一次保存追加一条 `file/write` 事件——刻意不是 `tool/call`，因为没有任何模型参与，而一份说反了的转录是没法读的；被拒的保存（会话只读、路径逃逸、摘要过期）则什么都不追加。Monaco 是打包进来的，不是从 CDN 拉的：桌面壳没有网络。
- **改动也能被扔掉**，走 `POST /api/sessions/{id}/revert`，落一条 `file/revert` 事件（第三类动作，理由同 `file/write`）。请求只带路径：提交过的字节是服务端去读的，所以客户端的摘要过期不了，还原也能重复执行。git 从没提交过的文件返回 `409` 而不是被删掉——「已提交版本」不存在时，还原就是删除，而操作者没要求删除。界面里「保存全部（N）」是纯前端便利：逐个标签保存、一个失败不中断其余，wire 上没有任何新形状。
- **工作区能被搜索**，走 `GET /api/sessions/{id}/search?q=`：按子串找行，答案带 `files`（真读过几个文件）与 `truncated`（触到命中或文件上限），所以「没有这一行」与「没找遍」是不同的句子。点一条命中打开文件并跳到那一行。差异列表另有一个只在前端存在的切换：同一段统一 diff 摆成左右两列（左磁盘、右提交，行号取自 `@@` 头），对齐是呈现，不是第二次判定。
- **选中一段代码就能问 agent**，而且只发生在界面里：wire 上没有一个新形状，也没有一条新事件。在编辑器里拖出选区，选区上方出现「问 agent（第 A–B 行）」，按下它把这段代码按围栏块放进输入框，光标落在块后面，剩下的问题接着写、按 Enter 就是一条普通消息。发出去的是**整行**，不是从鼠标落点开始的那半截——按钮承诺的是行，内容就必须是那几行（真机上原来的实现发的是 `t line5 = 5;` 这种从列中间切开的半个词）。按钮按下时不抢焦点，否则人接着敲的头几个字会掉进按钮里；选区超过 400 行或 16,000 字符时只附前一部分，并在消息里说明还差多少。它不做的事同样是刻意的：不替你发送、不猜你要问什么、也不把选区变成一次工具调用。
- **改过的行在编辑器里标出来，一个文件可以就地退回提交版本**，同样没有新形状：它读的是已有的 `GET /api/sessions/{id}/diff`，写的是已有的 `POST /api/sessions/{id}/revert`（落 `file/revert`）。打开一个文件时读一次它自己的 diff，行内画橙色底、行号栏留一道橙边，编辑区上方一行「与提交版本相比：+N −M」和「撤销这个文件」。三件刻意的克制：①条子说的是**与 git 提交的版本比较**，不是「agent 改的」——日志里存的是摘要不是内容，服务端根本没有「哪几行出自模型」这份信息，写了就是替别人的工作瞎猜，所以那句话只在悬停里说清谁分不出；②撤销只报告意图，落盘仍是 store 的事（只读会话、摘要过期这些决定不该有第二个地方再做一遍）；③标记只在**打开了这个文件**时才读、才画，没问过就不画——「没改动」与「没问过」在界面上必须是两句话。换文件时旧答案按路径丢弃，晚到的答案不会盖掉新文件的标记。
- **按轮的改动视图**，一个按钮把中栏从编辑器切成「第 N 轮 · k 个文件 · +A −B」，点一条回到那个文件的第一个改动行。它仍然没有 wire 上的新形状：`tool/result` 的 `meta` 本来就是自由格式，所以行信息是 `fs_write`、`fs_edit` 自己报的——`path`、以及 `lines{added, removed, ranges[{from,to}]}`，段是**新文本**里从 1 起的闭区间，最多 200 段；文件大到没法逐行对齐时给一个 `coarse` 整段，比较撞上上限时给 `cut` 说这只是前缀。规矩是「没有强过说错」：旧文本读不出来（新文件、二进制、不是 UTF 8）或任一边超过 20,000 行，就只报 `path` 不报 `lines`，界面上宁可什么都不显示；一个字节都没变的保存、末尾换行出现或消失，都不声称任何行。折叠由前端做：事件信封的 `turn` 把同一轮同一路径的多次写入并成一条、`ranges` 只留最后一次、`writes` 累加，所以它回答的是「这一轮结束时文件里哪些位置变了」，不是「每一次写改了哪里」。它**不等于**「agent 改的」：只有报告了行号的工具写入才在里面，`shell` 命令改的文件不在其中——空状态就把这句话说出来，而不是让人以为这一轮什么都没动。

- **别人写过的文件，编辑器自己跟上**：会话日志里最新一条带 `lines` 的 `tool/result` 就是信号（`latestWrite`），会话 store 把它作为 `agentWrite{seq, path}` 发布，工作台看到 `seq` 变了就重读那个文件与它的 diff。所以 agent 在自己打开着的文件里写字时，文字和橙色标记会自己变，不必先关标签再开。`seq` 进依赖列表是因为同一个文件连写两次是两条事件，只按路径判断第二次会看不见。三种情形分开处理，这不是没想清楚：①不是当前标签的写入不看，换个文件才看；②当前标签读还没回来时不看，那次读本来就会带最新字节；③当前标签有**未保存草稿**时不覆盖——草稿只在这个窗口里存在，重读等于替人删掉键盘敲出来的东西——只在条子上加一句「磁盘上又写了一次，保存会冲突」，而保存本来就会以 `conflict` 拒绝。判据仍是 `meta.lines`：读工具也会报 `path`，拿它当「写盘」会让人读一眼文件就被判定草稿过期。
- **读不到会话的时候，页面会自己再试，再不行就留一个「重试」**：首屏那几读（`GET /sessions/{id}`、第一页事件、以及流断过之后的重同步）以前失败一次就只剩一条只能「关闭」的红横幅，而那句话读起来像是「你刚才那个操作失败了」——可屏幕上根本没有事件，转录不是真相。现在 `isTransientFailure` 把两类失败分开：没到服务器的（`fetch` 拒绝，浏览器报 `TypeError: Failed to fetch`）、5xx、429、408 都算能自己好的，按 400 ms / 1.2 s / 3 s 退避各试一次再报；401、404、协议版本不符这类重试一万次也一样，第一次就报出来，少让人等四秒。重试用尽或终态失败时，状态从 `actionError` 里独立出来叫 `loadError`，带一个「重试」按钮走 `store.reload()`；重同步失败也走这条，因为那时候滚上去的转录同样不是日志本身。刻意没做的是无限重试：一个 401 变不成 200，一直转只会让人以为网络卡了，而不是让他去检查令牌。
- **终端是一条行式命令通路**，走 `GET /api/sessions/{id}/terminal`：WebSocket 升级、子协议 `hearth.v1`，与事件流共用 `ws_upgrade` 这一个握手函数。一个人敲一行，服务端先写 `terminal/run`（写不进日志就不跑），起进程，stdout 与 stderr 按 UTF-8 字符边界实时回传（子进程按控制台代码页写的字节，例如中文 Windows 的 GBK，先在 runner 里解码，所以 `output` 与两条尾巴都是 UTF-8），结束时写 `terminal/end`——退出码、耗时、`cancelled`，以及两条流各自的末 4 KiB（`TAIL_BYTES = 4 << 10`；实时流另按每流 1 MiB 截断并留一条说明）。可取消：`cancel` 走 `crates/exec` 已有的杀进程树，取消不写第三条事件，`cancelled: true` 就是记录；socket 关掉时正在跑的那条同样被取消，不留看不见的进程。read-only 会话根本不提供它（`new_terminal` 为 `None`，升级前就 403）。它比 `shell` 工具多一层：沙箱 enforcer 逐条装在子进程上，而 `--sandbox` 过去只影响策略层提供哪些工具、不约束子进程。它**不是** PTY：不是持久 shell，跑不了全屏 TUI，没有交互 stdin（子进程 stdin 是 EOF），也没有超时——只有 `cancel` 或关掉 socket 能让一条 build 提前结束。理由与取舍见 [14-adr-terminal.md](14-adr-terminal.md)。

- **`shell` 工具起的每个子进程现在都装进同一个 enforcer**，与终端那条通路共用一个桥（`crates/cli/src/exec_sandbox.rs`）。这不是新承诺，是补上一个一直没接的线：`--sandbox workspace-write` 以前只决定「哪个调用要被问」，`ShellTool` 从头到尾没拿到 sandbox，所以修前用真二进制做过对照——`workspace-write` 下的 `echo escaped > D:\tmp-shots\escaped-proof.txt` 成功写到了工作区外；修后同一条命令以 `Access is denied.` 失败（`meta.sandbox = windows-job-object+appcontainer`），而工作区内的写照常（`exitCode = 0`）。`full-access` 仍然拿到那个什么都不做的 enforcer，行为不变。

  同一处还修掉一个从终端第一天就存在的输出缺陷：`echo abc` 会打成 `abc"`，`ver` 直接变成 `'ver"' 不是内部或外部命令`。原因是 `hearth-exec` 先把 raw 命令行交给 `Command::raw_arg`，然后才让 enforcer 包一层，而 Windows 的包装器要按 program + args 重建命令行——`raw_arg` 的文本不在 `get_args` 里，于是整行被当成一个参数重新加引号。修法不是把顺序调过来（那会让包装器先分词再逐参数加引号，`echo "a&b|c"` 会静默塌成一条带管道的命令），而是让原始行走 `HEARTH_EXEC_RAW_CMDLINE`：runner 在 apply 前写入，包装型 enforcer 把它取进 payload 并删掉变量，包装器逐字追加到子进程命令行尾部；没有包装的 enforcer 不动它，runner 自己追加并清理。变量名在 `hearth-exec` 与 `hearth-sandbox` 各声明一次（两个 crate 互不依赖），由 `crates/cli` 的一条测试钉住两边拼写一致。真机对照：终端 `echo abc` 输出 `abc`，`echo "a&b|c"` 输出 `"a&b|c"` 只一条命令，`shell` 工具 `echo abc` 的 `stdoutBytes` 是 5，越界写仍然 `Access is denied.`。

  代价要说清：**第一次**在机器上跑受约束的子进程，AppContainer 要重写祖先目录的 DACL，本机实测 60–90 秒（`hearth: sandbox: granting read access to C:\Users\…\AppData\Local for the first time (11685 ms)` 这样几行），之后同一条命令暖跑 200 毫秒级。这笔钱原本按**工作区**付：祖先的 traverse ACE 授给每工作区自己的 package SID（profile 名按工作区路径哈希，`hearth.sandbox.{hash:016x}`），所以每个新工作区的第一条命令都要再等一次，e2e 用例就是被这一分钟拖到 120 秒超时上炸的。现在祖先的 traverse ACE 改授给一个本项目所有容器共享的 capability SID（`S-1-15-3-2048-40`），而工作区与 `sandboxAccess` 里的每个路径本身仍只授给那个工作区自己的 package SID——**数据隔离没变，只有「穿过祖先目录」从每工作区一次变成每机器一次**（验收测试量到第一块新工作区 61 秒、第二块 5 秒内，改前同样的第二块要再付一遍）。这个 capability 不单独给予任何访问权：它只出现在 traverse ACE 上，而 traverse 只让人走到路径、不能列目录或读文件。剩下的账是每台机器第一次的那一分钟，它发生在子进程里，所以会算进那条命令的耗时（`shell` 工具默认 120 秒超时仍然够，但一台更慢的机器上这笔账要和命令本身抢同一份预算）。

## 刻意没造的东西

这些是决策，不是待办。关掉其中一条会改变这个产物声称什么，所以每条都需要自己的 ADR，而不是一个安静的提交。

- **纯 Rust 的 Office 写出器。** `office` 技能驱动一个外部程序并且明说了；[office-run.md](office-run.md) 记录了产出了什么。
- **[09-platform-tier.md](09-platform-tier.md) §6 里的非目标**：浏览器内终端复用器、ACP agent 协议、Office 的*渲染*、图像生成模型，以及任何 DSH/DS 服务器依赖。这里的"终端复用器"指的是全屏 xterm 那种东西，它仍然没有；有的是一条行式命令通路（上一节最后一条），两者不是同一件东西。这棵树按设计是进程内 Rust。桌面壳曾在这张清单上，后来按需造了出来；它打开的编辑器现在也能保存，因为一次保存走的是 `PUT /api/sessions/{id}/file`——带 `expectedSha256`、落一条 `file/write` 事件并按只读会话、路径逃逸、过期摘要拒写——这是"一个人按键"的动作，不是模型的动作，所以它既不绕过策略也不绕过审批提示，而 `file/write` 事件正是让它可复核的东西。
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