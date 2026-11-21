---
dir: crates/server
layer: L3
role: HTTP/1.1 and WebSocket over the session log
public: [Server, Report, Entry]
may_import: [hearth-core, hearth-session, hearth-protocol, hearth-goal, hearth-subagent]
must_not:
  - import a crate from a higher layer
  - claim behaviour that does not exist yet
gate: cargo run -q -p archcheck --offline
---

## 做什么

本地服务：`docs/contracts/api.md` 里的 REST 路由、WebSocket 事件流，以及静态 UI 托管。它是从已退役的 `forge-go/internal/server` 移植到标准库上的——每连接一个线程，一个手写的 HTTP/1.1 读取器加 RFC 6455 子集，没有 async 运行时，也没有新依赖。唯一的真相是那份只追加的会话日志；HTTP 层写 intake 记录（`turn/start`、`user/message`、审批决定），并把轮次执行交给一个 [`TurnRunner`](api::TurnRunner)。

## 公开面

调用方需要的一切都从 crate 根重新导出：

- **`Server`** —— HTTP/WebSocket 服务（定义在 [`server`]）。
- 来自 [`api`]（那条冻结的边界）：`Options`、`StaticSource`、`EventWriter`、`TurnRunner`、`TurnContext`、`CancelToken`、`DeltaSink`、`SteeringSource`。

### `Server`

```rust
impl Server {
    /// Validate options, prepare the data directory, load or create the bearer
    /// token, and prepare embedded assets. No listener is opened yet.
    pub fn new(options: Options) -> hearth_core::Result<Self>;

    /// The bearer token the service accepts (also written to `<dataDir>/token`).
    pub fn token(&self) -> &str;

    /// The bound address once listening, else `None`. Tests poll this for
    /// readiness when the server binds to port 0.
    pub fn addr(&self) -> Option<String>;

    /// Like `addr()`, but returns an error instead of `None` before bind.
    pub fn bound_addr(&self) -> hearth_core::Result<String>;

    /// Request graceful shutdown. Idempotent.
    pub fn shutdown(&self) -> hearth_core::Result<()>;

    /// Bind and serve on the current thread; returns when shutdown completes.
    /// This is **blocking** and does **not** own a thread — the caller decides
    /// how to schedule it (e.g. `std::thread::spawn`). A non-loopback bind is
    /// refused with `server: refusing to bind "<addr>": protocol 1 is loopback only`.
    pub fn listen_and_serve(&self) -> hearth_core::Result<()>;

    /// Serve on an already-bound listener (useful for tests that bind first).
    pub fn serve_listener(&self, listener: std::net::TcpListener) -> hearth_core::Result<()>;

    /// Create a session directly, without going through HTTP. The HTTP
    /// `POST /api/sessions` route calls the same validation.
    pub fn create_session(
        &self,
        request: &hearth_protocol::CreateSessionRequest,
    ) -> hearth_core::Result<hearth_protocol::CreateSessionResponse>;

    /// Append a fully-formed event to a session log and return its sequence.
    pub fn append_event(
        &self,
        session_id: &str,
        event: &hearth_core::Event,
    ) -> hearth_core::Result<u64>;
    pub fn last_seq(&self, session_id: &str) -> u64;
    pub fn path(&self, session_id: &str) -> std::path::PathBuf;
}

impl EventWriter for Server { /* append, last_seq, path */ }
```

`Server` 是 `Clone` 的（它本身是一个 `Arc` 句柄），所以一个 accept 循环可以把句柄交给工作线程，而不必借用原始值。`shutdown()` 设置一个停止标志；在飞的连接会先跑完，然后线程才退出。

### 行为契约

- **鉴权与协议**：每个响应都带 `X-Hearth-Protocol: 1`。`/healthz` 不需要鉴权，也不碰磁盘。`/api/*` 要求 `Authorization: Bearer <token>` 或 `hearth_token` cookie，外加 `X-Hearth-Protocol: 1`；缺头是 `400 code=protocol_required`，版本不对是 `400 code=protocol_unsupported`。只接受 loopback 对端。
- **路由**：`GET/POST /api/sessions`、`GET /api/sessions/{id}`、`.../events`、`POST .../messages`、`.../cancel`、`.../steer`、`GET .../stream`、`POST .../approvals/{id}`。不支持的方法返回 `405`，带 `Allow` 头和 `code=method_not_allowed`。
- **工作区路由**：`GET .../files`、`GET .../file`、`GET .../diff`、`PUT .../file` 与 `POST .../revert`，实现在 [`workspace_route`](src/workspace_route.rs)。工作区来自 `session/start`；`..`、绝对路径和符号链接逃逸一律拒绝；文本上限 1 MiB。写路径是唯一会改动日志之外东西的路由：除非设了 `Options.workspace_write`（CLI 从 `--sandbox` 推导它），否则一律 `403` 拒绝；保存接受一个可选的 `expectedSha256` 作为乐观令牌、经临时文件加一次 rename 落盘、事后恰好追加一条 `file/write`；还原把文件退回 git 的已提交版本，事后追加一条 `file/revert`（不是 `file/write`：一个说「人写了这行」，一个说「人把改动扔了」），未被跟踪的文件会被拒绝而不是删除。两条路径上被拒绝的请求都什么都不追加。
- **幂等**：`POST .../messages` 要求 `Idempotency-Key`；首次投递是 `202`，重放是 `200` 且带 `deduplicated: true`。追加 `turn/start(number)` 再追加信封 `turn` 匹配的 `user/message` 的是**服务端**，不是 runner，同时把该 key 存进消息 payload。
- **WebSocket**：`GET .../stream` 做一次手写的 RFC 6455 握手（SHA-1 accept 值、掩码客户端帧），然后发一份快照、`(after, lastSeq]` 区间的积压事件，以及后续的实时事件/delta。升级之前的策略失败是普通 HTTP 状态码（401/403/400）。
- **静态 UI**：`StaticSource::Dir(path)` 对壳路由返回 `index.html`（`no-cache`），把 `assets/*` 当不可变资源提供；`StaticSource::Embedded` 提供由 [`build.rs`](build.rs) 从 `web/dist` 生成的文件，UI 尚未构建时则返回一个 200 的占位页。`/api/*` 永远不会被静态回退吞掉。

## 依赖与边界

运行时依赖限于 `hearth-core`、`hearth-session`、`hearth-protocol`、`hearth-goal`、`hearth-subagent`、`serde`、`serde_json`（见 `Cargo.toml`）。不要把 agent 逻辑放进来：服务端读日志、追加 intake 记录、把轮次交给 `TurnRunner`；它不得拥有 provider 或工具注册表。审批超时是一个由日志承诺下来的决定，所以这个决定在与其他所有变更相同的追加锁之下被追加。

### 内嵌 UI 的构建步骤

内嵌的源由 `build.rs` 从 `../../web/dist` 读取（web UI 是一棵只读的兄弟树）。在工作区根目录：

```powershell
cd web
npm ci
npm run build
```

这条命令产出 `web/dist/index.html` 与 `web/dist/assets/*`。之后重新构建 server crate，好让 `build.rs` 把它们重新内嵌；`cargo:rerun-if-changed` 盯着 dist 目录。不构建的话，内嵌源是一个仍然回答 `200` 的占位页。

## 不要做

- 不要加 `tokio` 或任何 async 运行时——这个服务端是每连接一个线程。
- 不要在 `Cargo.toml` 的工作区预算之外加依赖。
- 不要让 runner 去追加 intake 事件；它们归 HTTP 层所有，这样一次被重放的 key 才能在不开启第二个轮次的情况下被去重。
- 不要让静态回退去回答一个 `/api/*` 路径。