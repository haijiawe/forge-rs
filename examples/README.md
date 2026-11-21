# examples/ — 可以照抄的最小样本

这里的代码**不参与构建**，也不被 CI 编译。它们的用途是：想知道插件长什么样、
协议怎么握手的时候，有一个能直接跑起来的东西，而不是去读 4,000 行宿主代码。

## 里面有什么

| 目录 | 是什么 | 怎么跑 |
| --- | --- | --- |
| [`plugins/hello/`](plugins/hello/README.md) | 最小的插件：一个 `plugin.json` 加一个 `index.js`，讲 stdio JSON-RPC | 见它自己的 README |

## 插件的三条事实

1. **语言由作者定。** 宿主只讲 stdio 上的一行一条 JSON-RPC，所以 Python、Node、Rust、
   一个 shell 脚本都能当插件。实测往返开销：空 result 80.6 µs、4 KB 118.3 µs、64 KB 343.7 µs，
   而一次模型往返约 1,200 ms —— 语言在这里不是性能问题。
2. **默认关闭。** `plugins.enabled` 是空数组，插件不会被加载。一个数据目录里多余的
   `plugin.json` 不该把新工具塞到模型面前。
3. **工具名带命名空间。** 注册成 `<插件名>.<工具名>`，因为两个插件都可能有 `add`；
   每个插件工具在三种沙箱模式下都是 `ask`，调用离开本进程就该先被看见。

## 新东西放哪

- 新的参考实现 → `examples/plugins/<名字>/`，附一个 README 说明它演示了什么。
- 想让它成为**测试**而不是例子 → 放到对应 crate 的 `tests/` 里，例子目录里的东西不会被跑。

## 相关

- [crates/plugin/README.md](../crates/plugin/README.md) — 宿主、manifest、超时与清理
- [crates/market/README.md](../crates/market/README.md) — 打包成 tarball 并校验摘要