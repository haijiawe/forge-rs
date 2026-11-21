# 原版 DSH 对比（本机，2026-11-16）

本文记录 Rust/Go 重写完成之后按要求做的本机测量。这里不保存任何 API key。

## 在本机上"原版 DSH"指的是什么

| 项目 | 值 |
| --- | --- |
| CLI 启动器 | `D:\node_global\dsh.cmd` → `D:\node_global\node_modules\@deepseek-ai\dsh\lib\bin.js` |
| CLI 包 | `@deepseek-ai/dsh` **0.2.2-rc.1** |
| Node | v24.21.0 |
| 桌面壳 | `D:\Programs\DeepSeekHarness\DeepSeek Harness.exe` |
| 桌面版本文件 | **44.7.0**（Electron 壳的版本，不是 harness 版本） |
| 桌面可执行文件 | 246,341,104 字节 |
| `app.asar` | 209,021,953 字节（虚拟路径；展开后的 `app.asar\dsh` 树不在磁盘上） |
| 活动 profile | `D:\dsh\profiles\desktop` |
| headless profile | `D:\dsh\profiles\headless` |

原版 DSH CLI 是装着的。`DSH_INSTALL_DIR` 报出的源码树是一个 Electron 虚拟路径，在本机上不作为普通文件夹存在，所以对比用的是装好的 CLI 加上原版的 headless profile。

## 启动 / 版本

在本机上用 `Stopwatch` 测量：

| 命令 | 冷启动 | 热启动 | 输出 |
| --- | ---: | ---: | --- |
| `dsh --version` | 1,470 ms | 160 ms | `0.2.2-rc.1` |
| `dsh web --version` | — | 350 ms | `0.2.2-rc.1` |
| `hearth.exe --version` | bench harness 里约 5 ms | 约 5 ms | `hearth 0.0.0-dev (unknown) windows/amd64 rustc 1.96.0` |
| Go 版 `hearth.exe --version` | Go bench harness 里约 7 ms | 约 7 ms | `hearth 0.7.0-m7 …` |

DSH 的冷启动包含引导 Node CLI/profile 启动器。Rust 与 Go 的 CLI 启动是一条原生快路，回答 `--version` 之前根本不加载配置。

## 实时响应对比

同一台机器、同一个网络、同一个真实模型端点
（`https://z1rry.cyou/v1`）、同一个模型 id
（`deepseek-v4.1-flash-1017`）、同一个提示词：

```text
Reply with exactly: COMPARE-OK
```

给 DSH 打了一个临时 `--patch`，只替换它的 provider base URL 和默认模型；装好的原版本身没有被动过。这次对比没有往磁盘写任何 key。

| 轮次 | Rust | Go | 原版 DSH |
| --- | ---: | ---: | ---: |
| 1 | 1,203 ms | 1,143 ms | 5,724 ms |
| 2 | 9,328 ms | 1,252 ms | 17,304 ms |
| 3 | 1,044 ms | 1,095 ms | 5,491 ms |
| 中位数 | 1,203 ms | 1,143 ms | 5,724 ms |
| 最小值 | 1,044 ms | 1,095 ms | 5,491 ms |

三轮都打印了 `COMPARE-OK`。Rust 和 DSH 的第 2 轮很慢，是因为共享端点/网络抖动；它被记录下来而不是丢掉。按中位数算，在这个小样本里原版 DSH 比 **Go 慢约 5.0×**、比 **Rust 慢约 4.8×**。

一个重要的行为差异：DSH headless 有两轮在 `COMPARE-OK` 之前往 stdout 多打印了推理/解释行；Rust/Go 的 CLI 只在 stdout 打印最终答案，诊断信息走 stderr。这是 harness 行为差异，不是模型差异。

## 更早的实时检查

在跑那三轮对比之前，同一套设置产出过：

| 检查 | Rust | Go | 原版 DSH |
| --- | ---: | ---: | ---: |
| 精确字符串提示词 | `FINAL-RUST-OK` / 6,844 ms | `FINAL-GO-OK` / 1,516 ms | 没跑这个精确提示词 |
| 工具调用 / shell 输出 | `rust-tool-ok` / 2,582 ms | `go-tool-ok` / 2,965 ms | 这一轮没跑 |
| Web/UI | healthz 200、index 200、内嵌 JS 200 / 265,721 B | 先前已验证 | 运行中的 desktop profile 没有做基准 |

更早那次精确字符串跑的方差很大，原因是端点/网络抖动；这也是上面那张三轮表是更好对比的原因。

## 为什么 DSH 需要打补丁

原版 DSH 的 headless profile 默认 `deepseek-official` + `deepseek-flash`。在那儿用自定义的 z1rry key 会以官方 DeepSeek 401 失败，这符合预期：那个 key 属于自定义端点，不属于 `api.deepseek.com`。测试里用的临时补丁设置了：

- provider `magpie`，API `openai-completions`
- `baseURL: https://z1rry.cyou/v1`
- 默认模型 `deepseek-v4.1-flash-1017`

没有这个补丁，DSH 在 headless 模式下无法使用提供的自定义端点 key。

## 需要注意的地方

- 这是同机小样本，不是 CI 基准。
- DSH 是 Electron/Node/插件 harness；Rust/Go 是原生单二进制。启动差异是结构性的，不是某个实现里的 bug。
- 桌面版 DSH 壳没有单独做基准；对比用的是原版 CLI/headless profile。
- 测试期间用到的那个 API key 应当轮换，因为它早前出现在本会话的日志里。