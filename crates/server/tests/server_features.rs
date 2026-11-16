//! Feature tests for the parts of the server contract that the 34 JSON cases
//! do not exercise: WebSocket framing, static UI serving, auth via cookie,
//! loopback-only binding, and turn control. Everything runs against a real
//! loopback listener with no model in the loop.

use forge_core::{Event, EventType};
use forge_protocol::SnapshotFrame;
use forge_server::api::{Options, StaticSource, TurnContext, TurnRunner};
use forge_server::{RateLimit, Server, WS_SUBPROTOCOL};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn unique_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "forge-feature-{}-{}-{}",
        label,
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ))
}

struct Fixture {
    addr: String,
    token: String,
    server: Server,
    stop: Arc<AtomicBool>,
    _temp: TempGuard,
}

/// A runner that optionally holds a turn open until cancelled, so cancel and
/// steer have a live turn to act on.
struct HoldRunner {
    stop: Arc<AtomicBool>,
}

impl TurnRunner for HoldRunner {
    fn run_turn(
        &self,
        context: &TurnContext,
        _session: &str,
        _text: &str,
    ) -> forge_core::Result<()> {
        while !self.stop.load(Ordering::Relaxed) && !context.cancel.is_cancelled() {
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    fn cancel(&self) -> forge_core::Result<()> {
        Ok(())
    }
}

impl Fixture {
    fn start(options: Options) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let data_dir = options.data_dir.clone();
        let server = Server::new(options).expect("start server");
        let worker = server.clone();
        thread::spawn(move || {
            let _ = worker.listen_and_serve();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let addr = loop {
            if let Some(addr) = server.addr() {
                break addr;
            }
            assert!(Instant::now() < deadline, "server did not bind");
            thread::sleep(Duration::from_millis(2));
        };
        Self {
            addr,
            token: server.token().to_string(),
            server,
            stop,
            _temp: TempGuard(data_dir),
        }
    }

    fn default() -> Self {
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: unique_dir("default"),
            version: "feature-test".to_string(),
            poll_interval: Duration::from_millis(2),
            delta_interval: Duration::from_millis(2),
            ..Options::default()
        };
        Self::start(options)
    }

    fn raw_request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.addr).expect("connect");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.addr);
        for (key, value) in headers {
            request.push_str(key);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        // Only add the default close header when the caller did not supply
        // one, so WebSocket policy tests can send `Connection: Upgrade`.
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("Connection"))
        {
            request.push_str("Connection: close\r\n");
        }
        if !body.is_empty() {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).expect("write request");
        stream.write_all(body).expect("write body");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("header terminator");
        let head = std::str::from_utf8(&response[..split]).expect("utf8 head");
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("status");
        let parsed = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            .collect();
        (status, parsed, response[split + 4..].to_vec())
    }

    fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let bearer = format!("Bearer {}", self.token);
        let origin = format!("http://{}", self.addr);
        let content_type = "application/json; charset=utf-8";
        let mut headers = vec![
            ("X-Forge-Protocol", "1"),
            ("Origin", origin.as_str()),
            ("Authorization", bearer.as_str()),
            ("Content-Type", content_type),
        ];
        for &(name, value) in extra {
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
            if !value.is_empty() {
                headers.push((name, value));
            }
        }
        let bytes = body.map(|value| serde_json::to_vec(value).expect("encode body"));
        self.raw_request(method, path, &headers, bytes.as_deref().unwrap_or(&[]))
    }

    fn create_session(&self, workspace: &std::path::Path) -> String {
        // The server requires an absolute workspace and creates it, but a
        // nested path needs its parent present first.
        if let Some(parent) = workspace.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let body = json!({ "workspace": workspace.to_string_lossy(), "model": "feature-model" });
        let (status, _, bytes) = self.api("POST", "/api/sessions", Some(&body), &[]);
        assert_eq!(
            status,
            201,
            "create failed: {}",
            String::from_utf8_lossy(&bytes)
        );
        let value: Value = serde_json::from_slice(&bytes).expect("decode create");
        value["session"]["id"].as_str().expect("id").to_string()
    }

    fn append(&self, session: &str, typ: EventType, turn: u32, payload: Value) {
        let mut event = Event::new(session, typ, Some(payload));
        event.turn = turn;
        self.server
            .append_event(session, &event)
            .expect("append seed event");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.server.shutdown();
    }
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

fn read_ws_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).expect("ws head");
    let opcode = head[0] & 0x0f;
    let mut length = (head[1] & 0x7f) as usize;
    if length == 126 {
        let mut bytes = [0u8; 2];
        stream.read_exact(&mut bytes).expect("ws len16");
        length = u16::from_be_bytes(bytes) as usize;
    } else if length == 127 {
        let mut bytes = [0u8; 8];
        stream.read_exact(&mut bytes).expect("ws len64");
        length = u64::from_be_bytes(bytes) as usize;
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).expect("ws payload");
    (opcode, payload)
}

fn write_masked_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
    let mask = [0x37, 0xfa, 0x21, 0x3d];
    let mut frame = vec![0x80 | opcode];
    if payload.len() < 126 {
        frame.push(0x80 | payload.len() as u8);
    } else {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(&mask);
    for (index, byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[index & 3]);
    }
    stream.write_all(&frame).expect("write client frame");
    stream.flush().expect("flush client frame");
}

#[test]
fn websocket_snapshot_backlog_live_ping_and_close() {
    let fixture = Fixture::default();
    let workspace = std::env::temp_dir().join(format!("forge-ws-workspace-{}", std::process::id()));
    let session = fixture.create_session(&workspace);
    fixture.append(&session, EventType::TURN_START, 1, json!({ "number": 1 }));

    let mut stream = TcpStream::connect(&fixture.addr).expect("connect ws");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let bearer = format!("Bearer {}", fixture.token);
    let origin = format!("http://{}", fixture.addr);
    let upgrade = format!(
        "GET /api/sessions/{session}/stream?after=0 HTTP/1.1\r\n\
         Host: {}\r\n\
         Upgrade: websocket\r\n\
         Connection: keep-alive, Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         X-Forge-Protocol: 1\r\n\
         Origin: {origin}\r\n\
         Authorization: {bearer}\r\n\r\n",
        fixture.addr
    );
    stream.write_all(upgrade.as_bytes()).expect("send upgrade");

    // Read the raw 101 response up to the blank line.
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("handshake byte");
        response.push(byte[0]);
    }
    let head = String::from_utf8(response).expect("handshake utf8");
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols"),
        "{head}"
    );
    assert!(head.contains("X-Forge-Protocol: 1"), "{head}");
    assert!(head.contains("Sec-WebSocket-Accept:"), "{head}");

    let mut accept_value = String::new();
    for line in head.lines().skip(1) {
        let mut parts = line.splitn(2, ':');
        let name = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        if name.eq_ignore_ascii_case("Sec-WebSocket-Accept") {
            accept_value.push_str(value);
        }
    }
    // RFC 6455 §1.2 pins this exact accept value for the canonical sample key,
    // so it verifies the server's SHA-1/base64 handshake without a dep. Note the
    // trailing `=`: SHA-1 is 20 bytes, so the padded form always ends in one. An
    // earlier version of this assertion omitted it, encoding the very bug it was
    // meant to catch.
    assert_eq!(accept_value, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");

    // First data frame is the snapshot, then the backlog event we seeded.
    let (op, payload) = read_ws_frame(&mut stream);
    assert_eq!(op, 0x1);
    let snapshot: SnapshotFrame = serde_json::from_slice(&payload).expect("snapshot frame");
    assert_eq!(snapshot.kind, "snapshot");
    assert_eq!(snapshot.protocol, 1);
    assert_eq!(snapshot.session.summary.id, session);
    assert!(snapshot.last_seq >= 2, "session/start plus turn/start");

    // Backlog starts with session/start (seq 1), then our seeded turn/start.
    let (op, payload) = read_ws_frame(&mut stream);
    assert_eq!(op, 0x1);
    let event: Value = serde_json::from_slice(&payload).expect("event frame");
    assert_eq!(event["kind"], "event");
    assert_eq!(event["event"]["type"], "session/start");

    let (op, payload) = read_ws_frame(&mut stream);
    assert_eq!(op, 0x1);
    let event: Value = serde_json::from_slice(&payload).expect("turn event frame");
    assert_eq!(event["event"]["type"], "turn/start");

    // A JSON ping is answered with a pong frame.
    write_masked_frame(&mut stream, 0x1, br#"{"kind":"ping"}"#);
    let (op, payload) = read_ws_frame(&mut stream);
    assert_eq!(op, 0x1);
    let pong: Value = serde_json::from_slice(&payload).expect("pong frame");
    assert_eq!(pong["kind"], "pong");

    // A close frame ends the stream with a normal close.
    let mut close = 1000u16.to_be_bytes().to_vec();
    write_masked_frame(&mut stream, 0x8, &close);
    close.clear();
    let (op, _) = read_ws_frame(&mut stream);
    assert_eq!(op, 0x8, "server echoes a close frame");
}

// A browser cannot set a request header on a WebSocket handshake and cannot put
// a bearer token in one, so the two things it needs have to travel elsewhere:
// the token as a cookie the shell sets, the version as a subprotocol. Both were
// missing, and a real browser proved it — the UI's stream answered 401 forever,
// and would have answered 400 protocol_required even with a cookie.

/// Reads a raw HTTP response head off a socket, returning the 101's headers.
fn read_upgrade_head(stream: &mut TcpStream) -> String {
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("handshake byte");
        response.push(byte[0]);
    }
    String::from_utf8(response).expect("handshake utf8")
}

/// A browser-shaped upgrade: cookie credential, subprotocol version, and no
/// `Authorization` or `X-Forge-Protocol` header at all.
const WS_PADDED_KEY: &str = "4P9nCkKqC5XSj9D3a7utoQ==";
/// SHA-1 of `WS_PADDED_KEY` plus the RFC 6455 GUID, base64 with padding.
const WS_PADDED_ACCEPT: &str = "uYxl2aMFAJzUVMCCqNX8cPhh+8U=";
fn browser_upgrade(addr: &str, session: &str, cookie: Option<&str>) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect ws");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    // Chosen because the accept for this key ends in `=`; the RFC's sample key
    // produces an unpadded accept, which is how a padding bug hid here once.
    let mut request = format!(
        "GET /api/sessions/{session}/stream?after=0 HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Upgrade: websocket\r\n\
         Connection: keep-alive, Upgrade\r\n\
         Sec-WebSocket-Key: {WS_PADDED_KEY}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Protocol: {WS_SUBPROTOCOL}\r\n\
         Origin: http://{addr}\r\n"
    );
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).expect("send upgrade");
    stream
}

#[test]
fn the_shell_sets_the_cookie_for_the_printed_token() {
    let fixture = Fixture::default();
    let (status, headers, _) = fixture.raw_request("GET", "/?token=wrong", &[], &[]);
    assert_eq!(status, 200, "a wrong token still serves the shell");
    assert!(
        !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("Set-Cookie")),
        "a wrong token must not mint a cookie: {headers:?}"
    );

    let path = format!("/?token={}", fixture.token);
    let (status, headers, _) = fixture.raw_request("GET", &path, &[], &[]);
    assert_eq!(status, 200);
    let cookie = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Set-Cookie"))
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("no Set-Cookie in {headers:?}"));
    assert_eq!(
        cookie,
        format!(
            "forge_token={}; HttpOnly; SameSite=Strict; Path=/",
            fixture.token
        )
    );
}

#[test]
fn a_browser_style_upgrade_authenticates_by_cookie_and_names_its_subprotocol() {
    let fixture = Fixture::default();
    let workspace = std::env::temp_dir().join(format!("forge-ws-browser-{}", std::process::id()));
    let session = fixture.create_session(&workspace);
    fixture.append(&session, EventType::TURN_START, 1, json!({ "number": 1 }));

    // Without the cookie there is no credential on this request at all.
    let mut anonymous = browser_upgrade(&fixture.addr.to_string(), &session, None);
    let head = read_upgrade_head(&mut anonymous);
    assert!(
        head.starts_with("HTTP/1.1 401"),
        "a handshake with no credential must be refused: {head}"
    );

    let cookie = format!("forge_token={}", fixture.token);
    let mut stream = browser_upgrade(&fixture.addr.to_string(), &session, Some(&cookie));
    let head = read_upgrade_head(&mut stream);
    assert!(
        head.starts_with("HTTP/1.1 101 Switching Protocols"),
        "cookie plus subprotocol must upgrade: {head}"
    );
    // RFC 6455 §4.2.2: selecting a subprotocol without naming it makes the
    // browser fail the connection on its own.
    assert!(
        head.contains(&format!("Sec-WebSocket-Protocol: {WS_SUBPROTOCOL}")),
        "the 101 must echo the selected subprotocol: {head}"
    );
    // Padding included: a browser compares this byte for byte, and a value one
    // `=` short is a failed handshake.
    assert!(
        head.contains(&format!("Sec-WebSocket-Accept: {WS_PADDED_ACCEPT}")),
        "the accept must be padded standard base64: {head}"
    );

    let (op, payload) = read_ws_frame(&mut stream);
    assert_eq!(op, 0x1);
    let snapshot: SnapshotFrame = serde_json::from_slice(&payload).expect("snapshot frame");
    assert_eq!(snapshot.session.summary.id, session);
}

#[test]
fn websocket_policy_failure_before_upgrade_is_http() {
    let fixture = Fixture::default();
    let workspace = std::env::temp_dir().join(format!("forge-ws-policy-{}", std::process::id()));
    let session = fixture.create_session(&workspace);
    let bearer = format!("Bearer {}", fixture.token);
    let (status, _, bytes) = fixture.raw_request(
        "GET",
        &format!("/api/sessions/{session}/stream"),
        &[
            ("X-Forge-Protocol", "1"),
            ("Origin", "http://example.com"),
            ("Authorization", bearer.as_str()),
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Key", "abc"),
            ("Sec-WebSocket-Version", "13"),
        ],
        &[],
    );
    assert_eq!(
        status,
        403,
        "foreign origin refused before upgrade: {}",
        String::from_utf8_lossy(&bytes)
    );
}

// ---------------------------------------------------------------------------
// Static UI
// ---------------------------------------------------------------------------

#[test]
fn static_dir_serves_shell_and_assets_with_distinct_cache_headers() {
    let root = std::env::temp_dir().join(format!("forge-static-{}", std::process::id()));
    let assets = root.join("assets");
    fs::create_dir_all(&assets).expect("create assets dir");
    fs::write(root.join("index.html"), b"<html>shell</html>").expect("write shell");
    fs::write(assets.join("app.js"), b"console.log(1)").expect("write asset");

    let options = Options {
        addr: "127.0.0.1:0".to_string(),
        data_dir: root.join("data"),
        static_source: StaticSource::Dir(root.clone()),
        ..Options::default()
    };
    let fixture = Fixture::start(options);

    let (status, headers, body) = fixture.raw_request("GET", "/some/deep/route", &[], &[]);
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8_lossy(&body), "<html>shell</html>");
    assert!(headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("Cache-Control") && value == "no-cache"));
    assert!(headers.iter().any(|(name, value)| name
        .eq_ignore_ascii_case("X-Content-Type-Options")
        && value == "nosniff"));

    let (status, headers, body) = fixture.raw_request("GET", "/assets/app.js", &[], &[]);
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8_lossy(&body), "console.log(1)");
    assert!(headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("Cache-Control")
            && value.contains("immutable")));

    let (status, _, body) = fixture.raw_request("HEAD", "/index.html", &[], &[]);
    assert_eq!(status, 200);
    assert!(body.is_empty(), "HEAD has no body");

    // /api/* is never swallowed by the static fallback: missing auth is 401,
    // not a 200 HTML shell.
    let (status, _, _) = fixture.raw_request("GET", "/api/sessions", &[], &[]);
    assert_eq!(status, 401, "api routes stay behind auth");
}

#[test]
fn static_embedded_answers_200_even_without_a_build() {
    let fixture = Fixture::default();
    let (status, headers, body) = fixture.raw_request("GET", "/anything", &[], &[]);
    assert_eq!(status, 200);
    assert!(!body.is_empty(), "a placeholder shell is still returned");
    assert!(headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("Cache-Control") && value == "no-cache"));
}

// ---------------------------------------------------------------------------
// Auth, binding, turn control
// ---------------------------------------------------------------------------

#[test]
fn cookie_auth_and_loopback_only_binding() {
    let fixture = Fixture::default();
    let cookie = format!("forge_token={}", fixture.token);
    let (status, _, _) = fixture.raw_request(
        "GET",
        "/api/sessions",
        &[("X-Forge-Protocol", "1"), ("Cookie", cookie.as_str())],
        &[],
    );
    assert_eq!(status, 200, "cookie is an accepted credential");

    let options = Options {
        addr: "0.0.0.0:0".to_string(),
        data_dir: unique_dir("bind"),
        ..Options::default()
    };
    let server = Server::new(options).expect("construct non-loopback server");
    let error = server
        .listen_and_serve()
        .expect_err("non-loopback must be refused");
    assert!(
        error.to_string().contains("refusing to bind"),
        "unexpected error: {error}"
    );
    let unused = server.path("unused");
    let _ = fs::remove_dir_all(unused.parent().unwrap_or(&unused));
}

#[test]
fn cancel_and_steer_act_on_a_live_turn() {
    let stop = Arc::new(AtomicBool::new(false));
    let runner_stop = Arc::clone(&stop);
    let options = Options {
        addr: "127.0.0.1:0".to_string(),
        data_dir: unique_dir("control"),
        new_runner: Some(Arc::new(move |_| {
            Arc::new(HoldRunner {
                stop: Arc::clone(&runner_stop),
            })
        })),
        ..Options::default()
    };
    let fixture = Fixture::start(options);
    let workspace = std::env::temp_dir().join(format!("forge-control-ws-{}", std::process::id()));
    let session = fixture.create_session(&workspace);

    let message = json!({ "text": "start a turn" });
    let (status, _, _) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/messages"),
        Some(&message),
        &[("Idempotency-Key", "feature-turn-key")],
    );
    assert_eq!(status, 202);

    // Wait until the HTTP layer has written turn/start (status running).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, _, bytes) = fixture.api("GET", &format!("/api/sessions/{session}"), None, &[]);
        assert_eq!(status, 200);
        let detail: Value = serde_json::from_slice(&bytes).expect("detail");
        if detail["status"] == "running" {
            break;
        }
        assert!(Instant::now() < deadline, "turn never became running");
        thread::sleep(Duration::from_millis(5));
    }

    let (status, _, bytes) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/steer"),
        Some(&json!({ "text": "new direction" })),
        &[("Idempotency-Key", "feature-steer-key")],
    );
    assert_eq!(status, 202, "steer: {}", String::from_utf8_lossy(&bytes));

    let (status, _, bytes) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/cancel"),
        None,
        &[],
    );
    assert_eq!(status, 202, "cancel: {}", String::from_utf8_lossy(&bytes));
    let cancelled: Value = serde_json::from_slice(&bytes).expect("cancel response");
    assert_eq!(cancelled["cancelled"], true);

    // The steering text is durable even though delivery is in memory.
    let log = fs::read_to_string(fixture.server.path(&session)).expect("session log");
    assert!(
        log.contains("new direction"),
        "steer must append user/message"
    );
    assert!(log.contains("steering"), "steer payload is marked");
}

// ---------------------------------------------------------------------------
// Rate limiting (`rate_limited` / 429)
// ---------------------------------------------------------------------------

/// Ends the turn immediately: the budget is charged per accepted turn, so this
/// test only needs turns to be accepted, not to do anything.
struct QuickRunner;

impl TurnRunner for QuickRunner {
    fn run_turn(
        &self,
        _context: &TurnContext,
        _session: &str,
        _text: &str,
    ) -> forge_core::Result<()> {
        Ok(())
    }

    fn cancel(&self) -> forge_core::Result<()> {
        Ok(())
    }
}

#[test]
fn a_burst_of_turns_is_refused_with_429_and_retry_after() {
    let options = Options {
        addr: "127.0.0.1:0".to_string(),
        data_dir: unique_dir("ratelimit"),
        new_runner: Some(Arc::new(|_| Arc::new(QuickRunner) as Arc<dyn TurnRunner>)),
        // Two turns for the whole minute, so the third request in this test is
        // the first one a real client would have to wait for.
        rate_limit: RateLimit::new(2, Duration::from_secs(60)),
        ..Options::default()
    };
    let fixture = Fixture::start(options);
    let workspace = std::env::temp_dir().join(format!("forge-ratelimit-ws-{}", std::process::id()));
    let session = fixture.create_session(&workspace);

    for (index, key) in ["rl-1", "rl-2"].iter().enumerate() {
        let (status, _, bytes) = fixture.api(
            "POST",
            &format!("/api/sessions/{session}/messages"),
            Some(&json!({ "text": format!("turn {index}") })),
            &[("Idempotency-Key", key)],
        );
        assert_eq!(
            status,
            202,
            "post {key}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }

    let before = fs::read_to_string(fixture.server.path(&session)).expect("session log");
    let (status, headers, bytes) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/messages"),
        Some(&json!({ "text": "one too many" })),
        &[("Idempotency-Key", "rl-3")],
    );
    assert_eq!(
        status,
        429,
        "third post: {}",
        String::from_utf8_lossy(&bytes)
    );

    let retry = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("Retry-After"))
        .map(|(_, value)| value.clone())
        .expect("a 429 must carry Retry-After");
    let secs: u64 = retry
        .parse()
        .expect("Retry-After is an integer number of seconds");
    assert!((1..=60).contains(&secs), "unexpected Retry-After: {secs}");

    let error: Value = serde_json::from_slice(&bytes).expect("error body");
    assert_eq!(error["error"]["code"], "rate_limited");
    assert_eq!(error["error"]["details"]["sessionId"], session);
    assert_eq!(error["error"]["details"]["limit"], 2);

    // A refused request must leave no trace: a phantom turn/start would make
    // the next reader believe work had begun.
    let after = fs::read_to_string(fixture.server.path(&session)).expect("session log");
    assert_eq!(before, after, "a refused request must not append anything");
    assert!(!after.contains("one too many"));
}
