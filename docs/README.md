# docs —— 这里有什么，以及先读哪一篇

这棵树是 `hearth`，一个 Rust 实现：二十四个 crate 架在一条只追加的事件日志之上，[architecture.yaml](../architecture.yaml) 说明哪个 crate 可以 import 哪个，[tools/archcheck](../tools/archcheck) 在依赖边指向上层时让构建失败。冻结的东西是 [docs/contracts/](contracts/README.md)——v1 事件格式、CLI 文本、HTTP/WS 协议以及 M4–M6 的 crate 接口面——因为一个要由别的实现来遵守的承诺不能悄悄挪动。[tests/](../tests) 下的语料才是证明 Rust 读取器守住了这个承诺的东西：35 个事件用例、34 个 HTTP 用例、8 个 WS 用例、5 份 fixture 日志、`tests/golden/` 与 `crates/agent/tests/golden/` 下的 3 个 golden，以及内联在 `crates/llm/tests/openai.rs` 里的 SSE 转录——全部语言中立，并且是在 Go 树于 2026-11-17 被删除之前拷进来的。

下面每一篇文档都属于两类之一，而类别很重要：**决策**（decision）记录某件事为什么是现在这样，并且不该自行改变；**记录**（record）是对已经发生的工作的叙述，带提交和数字，预期是收尾而不是继续延长。一个已经执行完的计划留在树里就是记录，因为它里面的验收标准是那份工作形状唯一的书面出处。

编号从 08 开始，是因为 01–07 写在被本树取代的那棵 Go 树里，并在 2026-11-17 随它一起删除。保留这些编号而不是重新压实，是为了让既有引用（`docs/contracts/` 里、各 crate README 里、提交里的引用）仍能解析；这个目录里没有缺东西——本来就没有过 01 可丢。后面的文档提到某个已退役编号时，会说清楚这一点，并给出它实际指的是哪一节。

`ls docs` 只显示十七个文件名，看不出它们的区别；下面的分节就是目录列表无法充当的那份索引。依次是：决策（为什么长这样）、执行记录（造了什么、证据是什么）、对比（本树与 DSH 的关系）、运维（怎么构建、怎么发布、怎么测）、以及冻结的契约。

## 从这里开始

| 如果你想 | 读 |
| --- | --- |
| 知道这棵树是什么、怎么构建 | [../README.md](../README.md) |
| 从别人手里接过这棵树 | [13-handover.md](13-handover.md) |
| 知道一次改动必须遵守哪些规则 | [../AGENTS.md](../AGENTS.md) |
| 知道什么被冻结、不许动 | [contracts/README.md](contracts/README.md) |
| 理解 agent 执行层 | [agent-comparison.md](agent-comparison.md)，然后 [agent-gaps.md](agent-gaps.md) |
| 规划下一块工作 | [agent-gaps.md](agent-gaps.md)（缺什么的账本） |
| 判断某个文件/功能是被删了，还是从没存在过 | [removals.md](removals.md)（删除与改名台账） |

## 决策 —— 为什么长成这个样子

| 文档 | 回答什么问题、谁读 | 状态 |
| --- | --- | --- |
| [08 移植](08-rust-port.md) | 为什么用 Rust、哪个 crate 拥有什么、这次移植代价多大——crate 作者 | 已交付 (ADR-0002) |
| [09 平台层](09-platform-tier.md) | 平台层当时缺什么、每一部分用什么写的——下一轮规划者 | 部分交付 (ADR-0003；P1–P5 已进，有一项刻意没做) |
| [11 agent 钩子](11-adr-agent-hooks.md) | 为什么恰好有两个重写缝和一个决策缝、每个缝不许做什么 | 已交付 (ADR-0004，W5 + D 波) |
| [14 终端 ADR](14-adr-terminal.md) | 为什么终端是一条行式命令通路而不是 shell 复用器、为什么不逐命令审批、它的非目标是什么——面板与服务器作者 | 已交付 (ADR-0005) |

## 执行记录 —— 造了什么，以及证据

| 文档 | 回答什么问题、谁读 | 状态 |
| --- | --- | --- |
| [10 agent 对齐](10-agent-parity.md) | 补齐 agent 缺口的波浪计划：范围、契约改动、每波的验收 | 已交付 (W1–W6、S1–S5、A–E、G1–G8、H1–H7) |
| [12 子代理移植](12-subagent-port.md) | 深度子代理移植 (S1–S5) 交付了什么，含进程外 worker | 已交付 (S1–S5) |
| [agent 缺口](agent-gaps.md) | DSH 有、本树没有的东西，逐条给出状态和代价——规划时打开的那本账 | 已交付；剩一项刻意的非目标（`archive-admission` 现在是 **done**） |
| [13 交接](13-handover.md) | 树现在站在哪、什么刻意没造、哪些坑已经吃掉过几个小时——给下一个接手的人 | 已交付 (状态截至 2026-11-21) |
| [apps/desktop](../apps/desktop/README.md) | 包住 Web UI 的 Tauri 2 窗口：怎么启动服务、为什么是独立 workspace、为两个窗口创建缺陷付了多少代价——桌面壳负责人 | 已交付 |

## 对比 —— 本树与 DSH 的关系

| 文档 | 回答什么问题、谁读 | 状态 |
| --- | --- | --- |
| [dsh vs hearth](dsh-vs-hearth.md) | 两个代码库实际差在哪，按 rc.5 源码逐层对照——设计读者 | 已交付 |
| [dsh 对比](dsh-comparison.md) | 装好的 rc.1 CLI 在这台机器上有多快、测量是怎么做的——性能主张，以及上文引用数字的出处 | 已交付（测量记录） |
| [agent 对比](agent-comparison.md) | 两套 agent 循环差在哪——两个各自演化出来的设计 (DSH→forge-go→hearth) | 已交付 |

## 运维 —— 怎么构建、怎么发布、怎么测

| 文档 | 回答什么问题、谁读 | 状态 |
| --- | --- | --- |
| [bench](bench.md) | 构建是不是比基线慢了、三条基线从哪来——性能负责人 | 已交付 |
| [ci](ci.md) | CI 跑什么、每轮跑到了什么、哪些只是排练过——工作流编辑者 | 已交付 |
| [release](release.md) | 一个产物声称什么、不声称什么——发布构建者 | 已交付 |
| [web-ui](web-ui.md) | 10k 事件的转录能不能稳住 60 fps——转录负责人 | 已交付 |
| [office](office-run.md) | Office 技能是不是真的产出了文档——技能编辑者 | 已交付 |

## 契约 —— 冻结的接口面

[contracts/](contracts/README.md) 目录里有七个文件：
[events-v1.md](contracts/events-v1.md)（信封、载荷、重放）、
[cli.md](contracts/cli.md)（命令、帮助文本、退出码）、
[api.md](contracts/api.md)（HTTP/WS 路径、状态码、帧）、
[m4-surface.md](contracts/m4-surface.md)（索引、导入、分叉、压缩）、
[m5-surface.md](contracts/m5-surface.md)（MCP、技能、子代理）、
[m6-surface.md](contracts/m6-surface.md)（沙箱层级、危险清单、doctor），
外加目录自己的 [README.md](contracts/README.md)。

## 状态，照着诚实的读法

`已交付` 是文档自身的证据，不是这张表的看法。[08](08-rust-port.md) 和 [bench](bench.md) 的抬头行曾经自相矛盾（"accepted, in progress" 底下配一张每波都已进场的波次表；"no baseline has been recorded" 底下配一个有三行的基线文件），做法是改正它们，而不是留着一个已知的谎。[09](09-platform-tier.md) 是故意 `部分交付` 的：P1–P5 已进，而它刻意没造的那一项是纯 Rust 文档写出器——它的 §5 说，只有当"装一个二进制就能用"成为硬需求时，这东西才值得写。
[ci](ci.md) 与 [release](release.md) 作为文档已交付，两者现在描述的都是真跑过的运行，在托管 runner 上，带 run id。
[office](office-run.md) 是同一类记录，对象是 Office 技能：随树发布的指令产出了什么、缺了什么、什么从未被验证。
[dsh vs hearth](dsh-vs-hearth.md) 一开头就写明版本边界——它读的是 rc.5 源码，却复用 rc.1 的性能数字，并在下任何结论之前先声明这一点；[dsh 对比](dsh-comparison.md) 则是那些数字及其测量方法所在之处，这就是两者分成两个文件而不是一个的原因。
[agent 对比](agent-comparison.md) 把同一份 rc.5 源码收窄到 agent 执行层：循环形状、inbox、扩展点、流式记账、取消、子代理、team/goal 与压缩。它把 `hearth` 读作 `forge-go` 的 Rust 移植（`forge-go` 由 DSH 团队写成），所以两者不同之处是两个各自演化的设计之间的差异，不是移植 bug；
[agent 缺口](agent-gaps.md) 是 DSH 有而本树没有的东西的账本，评级 P0–P2，规划下一波时打开的就是它。当这张表和某篇文档冲突时，文档是事实来源，表才是 bug。
[agent 对齐](10-agent-parity.md) 是跟着缺口账本走的执行路线图：它把语言选择定下来（单一 Rust 内核、插件本体任意语言、TypeScript 前端），把工作切成 W1–W6 波，并写明每一波的契约改动、测试与验收——**它点名的每一波现在都已建成**（W2 后来发现冻结契约已经满足，所以不需要写代码），而它之后的那些波（子代理的 S1–S5、A–E、G1–G8 与 H1–H7）关掉了账本剩下的部分。
[13 交接](13-handover.md) 是一份自带保质期的记录：它的计数、它的门禁结果、它的二进制大小都是在交接提交上测的，所以后来的读者应当重跑 `check.ps1` 而不是相信这些数字；不失效的部分是那份坑的清单。

## 别让这份索引烂掉

`hearth help` 的用法文本结尾会点名这个文件（[crates/cli/src/lib.rs](../crates/cli/src/lib.rs) 里的 `USAGE`），而 `cargo run -q -p archcheck --offline` 现在会在它点名的某个 `docs/<file>.md` 不在磁盘上时让 `docs.missing` 失败。新文档在这里加一行；删掉一篇，意味着要在同一次改动里先改那行用法文本。

开头那段里的计数是最容易悄悄烂掉的东西，所以值得点出来：每加一个事件类型事件语料就长大一次，而这个文件里的数字曾经错过。检查办法是 `Get-ChildItem tests/conformance/cases/*`，并且 `crates/core/tests/conformance.rs` 会断言事件数，所以对不上会让构建失败，而不是安静地留在这里。