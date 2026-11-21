# hearth desktop —— 本地服务器外面那层窗口

一个 Tauri 2 窗口。它把 `hearth web` 当子进程起起来，等服务器打印出它正在服务的
那个 URL，然后在那个 URL 上开一个窗口。窗口里的一切与浏览器看到的是同一份 web
界面；这个 crate 只多给了一个窗口、一个任务栏条目和一个图标，别的什么都没加。

## 为什么它是「启动器」而不是「应用」

* 界面用 HTTP 加 bearer token 跟服务器说话。把这条路走在本机回环 socket 上，就是
  浏览器走的那条路——所以壳里的缺陷不可能悄悄地与浏览器里的缺陷分岔。
* 服务器拥有沙箱、审批弹窗和事件日志。一个把服务器链接进来的壳，就得被信任这三样，
  而一个窗口边框没有资格被信任任何东西。
* `hearth` 保持为「一个二进制、哪里都能跑」。壳是额外的东西：没有桌面的机器不会
  因为它变差，`cargo test --workspace` 也永远不需要 GUI 工具包。

## 构建

```powershell
cd D:\hearth\apps\desktop
cargo build --offline            # debug
cargo build --offline --release  # 要发布的就是这个
```

这是一个**独立的 cargo workspace**（自己的 `Cargo.toml` 里有 `[workspace]`）。
它刻意不是根 workspace 的成员：Linux 上的 `tauri` 需要 `webkit2gtk`，一旦成为成员，
每个 Linux CI job 都得装一套 GUI 工具包，只为了编一个窗口边框。壳与服务器之间的
边界是「把这个进程跑起来」，没有任何东西留给类型检查器去检查。

图标是生成出来的，不是手画的：

```powershell
& "…\python.exe" apps\desktop\icons\make-icons.py
```

它需要 Pillow，会写出 `icon.ico`（16–256 px）以及 `tauri.conf.json` 列出的三张 PNG。
只在标记改动时跑一次；产物是提交进仓库的，因为构建不该需要 Python。

## 运行

```powershell
.\apps\desktop\target\debug\hearth-desktop.exe --model deepseek-chat
```

所有参数都会转给 `hearth web`。壳自己加上 `--no-browser`，所以窗口就是浏览器，
Edge 不会在后面再开一个。

`HEARTH_BIN` 用来改写启动哪个二进制。不设它时，壳先用与自己同目录的
`hearth.exe`/`hearth`，再退到 `PATH`。一个靠猜的壳会启动**另一个**构建，而不是
它旁边那个——这正是「修好的缺陷看起来没修」那类故障。

在这个版本里，没有模型服务器会拒绝启动；`hearth web` 会以
`no model configured: pass --model <name> or set "model" in config.json` 退出，
壳把这句话显示在窗口里，而不是留一张白页。

## 窗口做了什么，以及顺序

1. `setup` 起一个工作线程。主线程从不被阻塞，因为它才是泵窗口消息的那个。
2. 工作线程起服务器（stdin 置空、stderr 接管），然后读 stderr 直到出现以
   `hearth web: open ` 开头的一行。端口是服务器自己挑的，所以壳没法替它选；
   那个 URL 就是服务器自己声明的「我在监听了」。
3. 回到**主线程**（经 `run_on_main_thread`），窗口直接在那个 URL 上创建，
   1440×900，最小 900×600。
4. 如果服务器没起来，窗口打开在**打包进来的兜底页**上，由一段 initialization
   script 把失败原因写进去。错误页是一份自己的文档，而不是往一个还在加载的页面上
   打补丁。

最后三条细节都是硬吃下来的：

* 错误页最初是一张 `data:text/html` URL，而 Tauri **不接受 `data:` URL**，除非编进
  `webview-data-url` 特性；失败发生在建窗那一刻，于是那张「专门用来报告问题」的页
  什么都没报，进程退出一片空白。现在用 `initialization_script`：它没有这个前置条件，
  在兜底页自己的脚本之前、每次加载都跑，所以消息不可能早于或晚于它描述的文档。
  实测：把 `HEARTH_BIN` 指向一个不存在的路径，窗口里出现
  `hearth 没能启动 | could not start D:\nope\missing-hearth.exe: 系统找不到指定的路径。 (os error 3)`。
* 从等待线程里创建窗口，页面确实会加载（用 WebView2 的调试端口读过：`#root`
  里有 7,819 字节 markup，样式表有 267 条规则，`errors` 是空的），然后只画出一片
  纯黑——因为 WebView2 的 controller 归运行事件循环的那个线程所有。
* 先开一张占位页、等服务器起来再导航过去，两次加载会重叠，最后留下一份空白文档：
  新页面是按旧文档还没设置完的视口布局的。

窗口就是 `tauri.conf.json` 里那条 CSP 覆盖的范围。它刻意很窄——
`default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:`——因为 Tauri
自己服务的唯一文档就是 `ui/` 里的兜底页。

## 关闭

服务器是子进程，退出处理里会 kill 并 wait 它，所以关掉窗口不会在回环端口上留下
一个监听者。这是实测的，不是假设：窗口关掉后，对旧端口的
`GET /api/capabilities` 返回 `000`（没有东西在监听），`hearth.exe` 进程也没了。

## 这个 crate 不做什么

* 它不打包模型、token 或配置。第一次运行还是需要 `--model` 或一份 `config.json`。
* 它不开浏览器，也没有自己的「退出」菜单项：关窗就是退出。
* 它不在这里构建安装包。`cargo build` 产出
  `target/{debug,release}/hearth-desktop.exe`；`tauri.conf.json` 里有打包元数据，
  但这台机器上没装 `tauri` CLI，构建也不需要它。
* 它不在 `check.ps1` 或 CI 的覆盖范围内。原因见「构建」；壳自己没有测试，
  所以这里没有任何东西能通过一个它并不参与的门禁。