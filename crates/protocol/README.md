---
dir: crates/protocol
layer: L2
role: the wire types shared by the server and the UI
public: []
may_import: [hearth-core]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

本地 API 的 JSON 形状：会话摘要、事件分页、审批 payload、WebSocket 帧、错误信封。形状冻结自已退役的 Go 树（`forge-go/internal/protocol/protocol.go`），两者有分歧时以 `docs/contracts/api.md` 和 `tests/` 下的金样为准。`protocol.rs` 只放形状，`lib.rs` 只声明模块。

## 公开面

- 常量：`VERSION`、`HEADER_VERSION`、`IDEMPOTENCY_KEY_HEADER`。
- HTTP 信封与请求/响应：`Health`、`SessionSummary`、`SessionDetail`、`SessionList`、`CreateSessionRequest`/`CreateSessionResponse`、`EventsPage`、`PendingApproval`、`Attachment`、`AttachmentRef`、`PostMessageRequest`/`PostMessageResponse`、`SteerRequest`/`SteerResponse`、`CancelResponse`、`ApprovalDecisionRequest`/`ApprovalDecisionResponse`、`FileWriteRequest`。
- WebSocket 帧：`SnapshotFrame`、`EventFrame`、`DeltaFrame`、`ErrorFrame`、`PingFrame`、`PongFrame`。
- 错误：`APIError`、`ErrorResponse`。

浏览器侧的对应类型在 `web/src/api/protocol.ts`。那份文件现在是**手工维护**的（旧树里的生成器没有随 Rust 重写一起搬过来），所以 `crates/protocol` 一改，它就必须在同一次提交里一起改；没有生成器，漂移是无声的。

## 依赖与边界

不要依赖 server。这个 crate 是两套实现的会合点；一旦它导入了其中一套，另一套就用不了它。

## 不要做

- 不要在这里写行为逻辑。协议只描述形状；一个能编译的占位是诚实的，一个看起来完成了的半实现不是。
- 不要在 `Cargo.toml` 的工作区预算之外加依赖。