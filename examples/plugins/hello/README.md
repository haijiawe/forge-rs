# hello —— 最小的完整插件

一个工具（`greet`），没有依赖，没有构建步骤。写新插件时就从这份文件开始抄。

## 试一下

```text
hearth plugin call hello greet --args '{"name":"world"}'
# hello, world
```

想让它出现在 `plugin list` 里，把目录放到 `<dataDir>/plugins/` 下：

```text
hearth plugin doctor                    # 打印 root: <dataDir>/plugins
```

或者从市场安装，`hearth market pack` 产出的就是这个形状：

```text
hearth market pack examples/plugins/hello --out dist --base-url https://example.invalid/p
# packed hello 1.0.0 (2 file(s)) + dist/index.json
```

## 改它

1. 在 `plugin.json` 和下面 `reply(...)` 握手处把插件改名。
2. 改 `callTool`：一个工具一个分支。工具跑了但拒绝时返回
   `{output, isError: true}`；对从没声明过的名字返回 `null`。
3. 加工具就是往 `TOOLS` 里追加一项，再加一个分支。

## 三条规矩

决定一个插件能不能用的只有这三条，其余都是风格。

| 规矩 | 为什么 |
| --- | --- |
| stdout 只放协议帧 | 一句 `console.log` 就是一个坏帧；诊断信息走 stderr |
| 把请求的 `id` 原样回送 | 宿主会拒绝 id 不同的答复，所以两个调用的答案不可能搞混 |
| `isError: true` 是「有结果」，JSON-RPC error 是「没试成」 | 前者给模型读，后者给操作者读 |

完整契约见 [`crates/plugin/PROTOCOL.md`](../../../crates/plugin/PROTOCOL.md)。