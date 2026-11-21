# hearth web 界面

一个 `hearth` 会话的浏览器界面：会话列表、虚拟化转录、工具卡片、审批弹窗和输入框。
它通过 HTTP 加上每会话一个 WebSocket 跟 `hearth web` 说话。没有 UI 套件、没有 CSS
框架、没有虚拟化库——虚拟滚动与流式 reducer 就在这个目录里，因为它们才是有意思的
部分。

## 跑起来

界面在编译期从 `web/dist` 内嵌进 `hearth` 二进制，并从 `/` 提供。开发时把 API 与
Vite 并行跑：

```powershell
# 终端 1 —— 127.0.0.1:8848 上的 API（token 打在 stderr）
cd D:\hearth
.\bin\hearth.exe web --addr 127.0.0.1:8848

# 终端 2 —— 127.0.0.1:5173 上的界面，把 /api 与 /ws 代理到 :8848
$env:npm_config_cache="D:\hearth\.cache\npm"
cd web
npm run dev
```

带着会话 token 打开一次开发地址：

```
http://127.0.0.1:5173/?token=<token>
```

`?token=` 只被读**一次**，存进 `sessionStorage`（**绝不**用 `localStorage`），
然后用 `history.replaceState` 从地址栏删掉——URL 里的 token 会留在截图、shell 历史
和浏览器历史里。刷新时复用已存的 token。路由是 hash 路由（`#/s/<id>`），所以服务器
不需要任何 SPA 兜底规则。

想把开发代理指向别的服务器，设 `$env:HEARTH_DEV_API="http://127.0.0.1:9000"`。

## 构建、测试、检查

```powershell
$env:npm_config_cache="D:\hearth\.cache\npm"
cd web
npm ci            # 精确锁定的版本，lockfile 进仓库
npm run typecheck # tsc --noEmit
npx vitest run    # 测试
npm run build     # -> web/dist/index.html + web/dist/assets/*
node scripts/bundle-budget.cjs   # 两份预算：首屏载荷，以及懒加载那一组
```

npm 的缓存必须放在仓库里：沙箱挡住了用户目录下的默认缓存（`E:\Cache\npm`），
所以裸跑 `npm install` 会失败。上面那两个变量是修法，不是偏好。

`scripts/bundle-budget.cjs` 量两份载荷，因为工作台里有两种 JavaScript。**首屏**那份
——`index.html` 加它的静态 import，也就是读者要等的东西——上限是 600 KiB gzip，
这是界面设计承诺的数字。Monaco 走动态 `import()`，没打开文件前不会取它；它的
chunk 和语法文件单独算作**懒加载**那一组。两个数字每次运行都会打印，两个上限都写在
脚本里，所以抬高其中任何一个是可见的编辑，而不是悄悄漂移。

`npm run build` 写出 `web/dist`，`hearth-cli` 在编译期用 `include_dir!` 把它内嵌——
跑起来的二进制不读这个目录，所以重建了 bundle 就得重建二进制
（`cargo build --release -p hearth-cli`，再把产物复制成 `bin/hearth.exe`）。
source map 保持关闭（它们会把源码也打进二进制）。

## 每样东西在哪

| 路径 | 负责什么 |
| --- | --- |
| `src/api/protocol.ts` | wire 形状，镜像 `crates/protocol/src/protocol.rs`——两者必须在同一次提交里改（当初生成这个文件的 Go 生成器没有随重写搬过来） |
| `src/api/types.ts` | 协议类型唯一的导入口（让测试不依赖生成器） |
| `src/api/events.ts` | 来自 `docs/contracts/events-v1.md` 的事件信封与 payload 形状，带未知类型的兜底 |
| `src/api/client.ts` | 带类型的 fetch 包装（Bearer + `X-Hearth-Protocol`）、token 捕获、POST 上的 `Idempotency-Key` |
| `src/api/stream.ts` | 流式 reducer：snapshot → event → delta，按 seq 去重，检测空洞 |
| `src/api/transport.ts` | WebSocket 本身：带退避的重连、`?after=<lastSeq>`、心跳 |
| `src/state/rows.ts` | 事件 → 转录行（工具调用/结果与审批请求/裁决各自合并） |
| `src/state/sessionStore.ts` | 给 `useSyncExternalStore` 用的每会话 store，每个动画帧只渲染一次 |
| `src/state/capabilities.ts` | 从最后一次请求的工具名 → 能力面板的分节 |
| `src/state/workbench.ts` | 工作台 store：打开的标签、草稿、每个标签读到时的摘要、保存与冲突状态 |
| `src/api/workbench.ts` | `files`/`file`/`diff`/`PUT file`——列目录、读、存，以及那些「宁可拒绝也不渲染」的解析器 |
| `src/virtual/` | 虚拟滚动：Fenwick 偏移索引、按行键测量的高度、锚点恢复 |
| `src/components/` | 各个界面与行（`Transcript`、`TranscriptRow`、`ToolCard`、`ApprovalDialog`、`Composer`、`StatusBar`、`SessionList`、`SessionView`、`CapabilityPanel`、`SubagentPanel`） |
| `src/components/ide/` | 三栏工作台：`Workbench`、`FileTree`、`Tabs`、`EditorPane`（Monaco，懒加载）、`DiffList` |
| `src/markdown/parse.ts` | 块级 markdown 解析器（没有语法高亮） |

## 这个界面必须守的渲染规矩

出自退役树里 `02-architecture.md` §10.1 的设计；这些规矩在这里由测试守着，
[../docs/web-ui.md](../docs/web-ui.md) 记录了让它们对得起账的实测数字。

1. **DOM 里只有可见窗口 ±1 屏**，不管转录多长。行高在提交后测量、按行键缓存；
   滚动位置做锚定，所以往前面塞旧行不会跳。
2. **流式过程中每个动画帧最多渲染一次。** 流帧同步打进 reducer（顺序等不了），
   但 React 的快照从 `requestAnimationFrame` 发布；一串 delta 帧只花一次渲染。
3. **markdown 按块解析、按块记忆。** 正在增长的助手行只重解析它最后一块，
   不是整篇转录。
4. **delta 只是提示。** `assistant/message` 才是权威文本（`docs/contracts/api.md`
   §4.8）；一个 seq 最终没变成 message 事件的 delta 会被丢弃，而不是显示成幽灵。
5. **工具输出是有界的。** 服务器封顶 64 KiB 并用 `truncated: true` 说明；
   卡片显示这个标记、折叠成前 20 行和后 20 行，工具失败时显示机器错误码。
6. **导出签名里不许有 `any`，token 不许进 `localStorage`。**

## seq 出现空洞是服务器的缺陷，不是界面的状态

这条流绝不能把同一个事件应用两次，也绝不能跳过任何一个。reducer 记着 `lastSeq`，
丢掉任何 `<= lastSeq` 的（重连重放），并把超出 `lastSeq + 1` 的 `seq` 当作协议违规：
停止信任这个 socket，从最后应用过的 seq 重新取 `GET /events` 直到追上。状态栏之所以
显示 `lastSeq`，就是为了让这件事可被观察。

## 仍未做

- 往回翻的旧历史分页：契约里没有向后的 `GET /events` 游标，所以界面总是从 seq 1
  加载整份日志。虚拟滚动的「前插」路径与它的滚动恢复行为已经实现并有测试，
  等向后游标出现时它们仍然能用。
- 关闭编辑器标签前询问未保存的改动：现在关掉一个脏标签只是丢掉草稿，日志里
  不会留下任何记录。