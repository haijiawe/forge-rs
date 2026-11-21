# apps/ — 图形外壳，不是产品的核心

这里放**站在 `hearth` 外面的东西**。核心永远是一个可执行文件加一个 loopback 服务；
这一层只是给它开一扇窗。

## 里面有什么

| 目录 | 是什么 | 说明书 |
| --- | --- | --- |
| [`desktop/`](desktop/README.md) | Tauri 2 桌面壳：起 `hearth web --no-browser` 子进程，等它打印 URL，再开窗 | [desktop/README.md](desktop/README.md) |

## 三条设计约束

1. **壳不链接内核。** 它起一个子进程，用和浏览器完全相同的一条路（HTTP + token + WebSocket）访问会话。
   内核握住沙箱、审批和事件日志，一扇窗边框不该被信任这三样东西。
2. **`apps/desktop` 是独立 cargo workspace。** 它不在根 `architecture.yaml` 的分层图里，也不被
   `check.ps1` 与 CI 构建。理由很实际：若把它并进根 workspace，每个 Linux CI job 都要装
   `webkit2gtk` 才能编一个窗口边框，而它和服务器之间的边界只是「起一个进程」，没有可类型检查的东西。
3. **窗口必须在拿到 URL 之后才创建，而且在主线程创建。** 两条都是踩出来的：先建占位页再
   `navigate` 会让 WebView2 白屏；在 worker 线程建窗会让它加载完整页面却只画一片黑。

## 新东西放哪

- 另一个壳（菜单栏、托盘、移动端）→ `apps/<名字>/`，同样独立 workspace，同样起子进程。
- 想给窗口加能力（文件对话框、通知）→ 先问「浏览器里做不到吗」。做不到的才进壳，
  并且**不要**把内核逻辑搬进来。

## 相关

- [desktop/README.md](desktop/README.md) — 构建方式、`HEARTH_BIN`、两个窗口缺陷的完整复盘
- [web/](../web/README.md) — 壳里显示的那份界面
- [docs/13-handover.md](../docs/13-handover.md) — 坑清单，含这两条窗口事故