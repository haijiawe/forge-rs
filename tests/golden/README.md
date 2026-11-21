# golden/ — 输入一份日志，输出一份投影，逐字节相等

Golden 文件解决的是**重构时最贵的那类错误**：投影逻辑改了，测试全绿，但界面上少了一行。
这里的文件是「正确答案」，不是「参考值」——比较方式是**逐字节**。

## 里面有什么

| 文件 | 输入 | 输出是什么 | 谁在读 |
| --- | --- | --- | --- |
| [`projection/basic.json`](projection/basic.json) | `fixtures/events/v1/happy-path.jsonl` | 折叠成 UI 能直接渲染的行序列 | [`crates/session`](../../crates/session/README.md) 的投影测试 |
| [`agent/text-turn.json`](agent/text-turn.json) | 一轮纯文本对话（脚本化 provider） | 这一轮**应该**写进日志的完整事件序列 | [`crates/agent`](../../crates/agent/README.md) 的循环测试 |

## 怎么更新一个 golden

**不是**跑测试拿到新值粘贴进去。顺序必须是：

1. 先改代码，让新行为在你脑子里是「我想要的」；
2. 跑测试，看失败信息里 diff 的**每一行**，逐行判断它是你要的改动还是回归；
3. 确认后重新生成（测试失败信息里会给出完整的期望值），再读一遍新的文件；
4. 提交时在提交信息里写清楚**为什么**这一版的输出是对的。

跳过第 2 步的更新等于把测试变成一张照片：它还在，但不再看守任何东西。

## 比较规则

- 比较的是**序列化后的字节**，不是结构等价。字段顺序、缩进、是否省略空字段都会被抓到。
- 文件里的数字/时间戳必须是**确定的**：golden 里不许出现 `Date.now()`、随机 id、进程 id。
  需要确定性的时候，代码里就把它们固定住（测试里用一个假时钟 / 固定 id 生成器）。

## 相关

- [tests/README.md](../README.md) — 三种语料怎么分工
- [fixtures/README.md](../fixtures/README.md) — 输入放那里
- [conformance/README.md](../conformance/README.md) — 判定规则放那里