# ws/ — WebSocket 的逐帧黄金转录

八个用例，每个用例都是一份「先铺这些事件、然后这样连接、应该逐帧收到什么」的完整剧本，包括控制帧与关闭码。
它比 HTTP 面的用例更严：帧的顺序、`kind`、以及关闭码都在被比较的范围内。

## 里面有什么

| 文件 | 是什么 |
| --- | --- |
| [`src/lib.rs`](src/lib.rs) | 一个极小的 WebSocket 客户端（握手、masked frame、读 `Incoming`），加八个用例的驱动 |
| [`Cargo.toml`](Cargo.toml) | 独立的测试 crate，`cargo test -p hearth-ws` 跑它 |

## 八个用例

| 用例 | 守的是 |
| --- | --- |
| `01-golden-snapshot-events-close` | 先一份快照、再两条增量（其中一条是**未知类型**，必须原样透传）、正常关闭 |
| `02-golden-snapshot-events-delta-close` | 快照之后客户端要一段增量，再关闭 |
| `03-golden-client-ping-pong` | 客户端 ping 必须得到同 payload 的 pong |
| `04-golden-unsupported-client-frame` | 客户端发一个不许发的 opcode → 按协议关闭，而不是静默忽略 |
| `05-golden-reconnect-after-dedup` | 断线重连后不重复投递已经收到过的事件 |
| `06-golden-policy-rejection` | 策略拒绝发生在**升级之前**：HTTP 401，不是连上再发关闭码 |
| `07-golden-server-ping-control` | 服务端主动 ping，客户端必须回 pong |
| `08-golden-server-ping-timeout` | 服务端 ping 超时后关闭连接 |

## 用例的形状

```json
{
  "name": "golden_snapshot_events_close",
  "description": "…",
  "sessionId": "ws-golden-0001",
  "pingIntervalMs": 30000,
  "seedEvents": [ { …完整事件… } ],
  "post": false,
  "liveEvents": [ { …连接期间追加到日志的事件… } ],
  "runnerDelta": null,
  "connections": [
    {
      "after": 0,
      "expect": [ { "kind": "status", … }, { "kind": "event", "event": { … } } ],
      "expectControlPing": 0,
      "expectClose": 1000
    }
  ],
  "expectHTTPStatus": 0
}
```

- `seedEvents` 是连接**之前**就写在日志里的行；`liveEvents` 是连接期间追加的（用来测增量）。
- `expect` 比较的是序列化后的帧内容，所以里面不许出现「本次运行」的随机值。
- `expectHTTPStatus` 非 0 表示**根本连不上**，用例断言的是升级前那个 HTTP 状态码（见 06）。
- `mode: "policy"` 的用例不建连接，只断言握手被拒。

## 已知的假失败

`golden_ws_transcripts` 偶尔在全量并行跑时红一次，单跑 `cargo test -p hearth-ws --lib` 必绿。
原因是每次用 `127.0.0.1:0` 临时端口、测试之间抢 CPU，不是固定端口冲突，也不是服务端缺陷。
**不要为了让它并行稳定而放宽断言**——放宽之后它就再也抓不到真的帧序错误。

## 相关

- [tests/README.md](../README.md) — 三种语料怎么分工
- [docs/contracts/api.md](../../docs/contracts/api.md) §6 — WebSocket 的握手与子协议
- [crates/server/README.md](../../crates/server/README.md) — 实现在哪里