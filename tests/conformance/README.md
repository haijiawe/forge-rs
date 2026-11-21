# conformance/ — 冻结的契约，不在测试里现写

这里的每一个 JSON 都是**一份断言**，不是一份数据。它们的意义是：任何实现（这棵树、以后的别的语言、
一个第三方写的读日志工具）都必须对同一份输入给出同一个答案。所以它们**不许**为了让某个实现通过而修改。

## 三个语料

| 目录 | 是什么 | 多少条 | 谁在读 |
| --- | --- | --- | --- |
| [`cases/events/`](cases/events) | 事件日志的行级判定：接受的行、拒绝的行、拒绝时那句必须一模一样的错误文案 | 33 | [`crates/core/tests/conformance.rs`](../../crates/core/tests/conformance.rs) |
| [`cases/http/`](cases/http) | HTTP 面：准备状态、发一个请求、断言状态码与响应体里的几个字段 | 34 | [`crates/server/tests/http_conformance.rs`](../../crates/server/tests/http_conformance.rs) |
| [`cases/ws/`](cases/ws) | WebSocket 的黄金转录：逐帧、含控制帧与关闭码、含升级前被拒 | 8 | [`tests/ws/`](../ws/README.md) |

## 事件用例的形状

```json
{
  "name": "error_seq_gap",
  "description": "一句话说明这条用例守的是什么",
  "lines": [ { …事件对象… }, "…或者一行字符串…" ],
  "expect": "ok",
  "canonical": [ { …重新编码后应该得到的对象… } ]
}
```

- `lines` 是逐行输入。元素可以是**对象**（按编码后比较）也可以是**字符串**（按字面字节）——
  所以空行、撕裂的尾行、非法的 JSON 都能被精确表达。
- `expect` 只有两个值：`"ok"` 或 `"error"`。
- 拒绝类用例用 `errorContains`（错误文案里必须出现的子串），**不许**给 `canonical`。
- `canonical` 是「重新编码这一行应该得到的字节」。没有它的接受用例只断言「编码是不动点」。
- `description` 不是注释，它是这条用例存在的理由。写「为什么需要它」而不是「它做了什么」。

## HTTP 用例的形状

```json
{
  "name": "approvals_allow_200",
  "description": "…",
  "setup": "pending_approval",
  "request": { "method": "POST", "path": "/api/sessions/$SESSION/approvals/$APPROVAL", "body": { } },
  "expect": { "status": 200, "jsonContains": { "decision": "allow" }, "atLeast": { "seq": 2 } }
}
```

`setup` 造出前置状态（例如真的挂一个待审批），`$SESSION` / `$APPROVAL` 由 runner 替换。
`jsonContains` 只断言列出的键（响应可以有别的字段），`atLeast` 断言「不小于」。

## 三条规矩

1. **不许改用例让实现通过。** 用例是契约，实现是读者。真觉得用例错了，意味着两个实现都错了，
   改动要同时在 `docs/contracts/` 里写清楚。
2. **新事件类型必须带一条用例。** `AGENTS.md` 的 DoD 表里写着：进 `EventType::ALL` +
   契约里加一行 + 这里加一个 case，三件事同一次提交。
3. **用例只描述可观察的东西。** 时间戳与 id 在用例里是固定的字面量（为了让 `canonical` 可比较），
   但**不许**引入「本次运行」的随机值——那会让用例变成一张照片。

## 相关

- [docs/contracts/events-v1.md](../../docs/contracts/events-v1.md) — 事件 envelope 的规则
- [docs/contracts/api.md](../../docs/contracts/api.md) — HTTP 面的路由全集
- [tests/README.md](../README.md) — 三种语料怎么分工