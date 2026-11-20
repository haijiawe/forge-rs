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
use std::path::{Path, PathBuf};
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

// ---------------------------------------------------------------------------
// Attachments: the body goes to the blob store, the event keeps a reference
// ---------------------------------------------------------------------------

fn attachment_fixture(label: &str) -> Fixture {
    let options = Options {
        addr: "127.0.0.1:0".to_string(),
        data_dir: unique_dir(label),
        new_runner: Some(Arc::new(|_| Arc::new(QuickRunner) as Arc<dyn TurnRunner>)),
        rate_limit: RateLimit::disabled(),
        ..Options::default()
    };
    Fixture::start(options)
}

fn post_message(fixture: &Fixture, session: &str, body: &Value, key: &str) -> (u16, Vec<u8>) {
    let (status, _, bytes) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/messages"),
        Some(body),
        &[("Idempotency-Key", key)],
    );
    (status, bytes)
}

#[test]
fn an_attachment_body_goes_to_the_blob_store_and_the_event_names_it() {
    let fixture = attachment_fixture("attach-store");
    let workspace = std::env::temp_dir().join(format!("forge-attach-ws-{}", std::process::id()));
    let session = fixture.create_session(&workspace);

    let payload = b"the bytes of a small image";
    let body = json!({
        "text": "look at this",
        "attachments": [{
            "kind": "image",
            "name": "shot.png",
            "mediaType": "image/png",
            "data": base64_for(payload),
        }],
    });
    let (status, bytes) = post_message(&fixture, &session, &body, "attach-1");
    assert_eq!(
        status,
        202,
        "post with attachment: {}",
        String::from_utf8_lossy(&bytes)
    );

    let log = fs::read_to_string(fixture.server.path(&session)).expect("session log");
    let user_line = log
        .lines()
        .find(|line| line.contains("user/message"))
        .expect("the message is in the log");
    // The base64 body must not be in the log: it would be re-read by every
    // reader, copied by every fork, and returned by every GET /events page.
    assert!(
        !user_line.contains("data"),
        "the event must not carry the body: {user_line}"
    );
    assert!(
        user_line.contains("\"bytes\":26"),
        "size recorded: {user_line}"
    );
    assert!(user_line.contains("shot.png"), "name recorded: {user_line}");
    assert!(
        user_line.contains("\"mediaType\":\"image/png\""),
        "media type recorded: {user_line}"
    );

    let event: Value = serde_json::from_str(user_line).expect("event json");
    let reference = &event["payload"]["attachments"][0];
    let id = reference["id"]
        .as_str()
        .expect("an attachment with a body must carry an id");
    assert_eq!(id.len(), 32);
    assert!(
        id.chars().all(|c| c.is_ascii_hexdigit()),
        "id is a hex name: {id}"
    );
    assert!(reference.get("data").is_none(), "no body in the reference");

    // The bytes are on disk, unchanged, under the name the event gave.
    let blob = fixture._temp.0.join("blobs").join(id);
    let stored = fs::read(&blob).expect("blob file");
    assert_eq!(stored, payload, "the blob is byte-for-byte what was sent");

    // And they come back out of the documented endpoint.
    let (status, headers, body) = fixture.api(
        "GET",
        &format!("/api/sessions/{session}/attachments/{id}"),
        None,
        &[],
    );
    assert_eq!(status, 200, "download: {}", String::from_utf8_lossy(&body));
    assert_eq!(body, payload, "the download returns the same bytes");
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    };
    assert_eq!(header("Content-Type").as_deref(), Some("image/png"));
    assert_eq!(header("Content-Length").as_deref(), Some("26"));
    // These bytes were uploaded by a client; a browser must not run them as a
    // document in the origin that holds the token.
    assert_eq!(header("X-Content-Type-Options").as_deref(), Some("nosniff"));
    assert_eq!(header("Content-Disposition").as_deref(), Some("attachment"));
}

#[test]
fn an_attachment_name_is_never_a_path() {
    let fixture = attachment_fixture("attach-names");
    let workspace =
        std::env::temp_dir().join(format!("forge-attach-name-ws-{}", std::process::id()));
    let session = fixture.create_session(&workspace);

    for name in [
        "../../token",
        "abc",
        "0123456789abcdef0123456789abcde",
        &"z".repeat(32),
    ] {
        let (status, _, bytes) = fixture.api(
            "GET",
            &format!("/api/sessions/{session}/attachments/{name}"),
            None,
            &[],
        );
        assert_eq!(
            status,
            404,
            "{name} must not be served: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
}

#[test]
fn an_attachment_from_another_session_is_not_served_here() {
    let fixture = attachment_fixture("attach-cross");
    let one = std::env::temp_dir().join(format!("forge-attach-own-{}", std::process::id()));
    let two = std::env::temp_dir().join(format!("forge-attach-other-{}", std::process::id()));
    let owner = fixture.create_session(&one);
    let stranger = fixture.create_session(&two);

    let body = json!({
        "text": "mine",
        "attachments": [{ "kind": "image", "data": base64_for(b"private") }],
    });
    let (status, _) = post_message(&fixture, &owner, &body, "attach-own");
    assert_eq!(status, 202);

    let log = fs::read_to_string(fixture.server.path(&owner)).expect("session log");
    let event: Value = serde_json::from_str(
        log.lines()
            .find(|line| line.contains("user/message"))
            .expect("user line"),
    )
    .expect("event json");
    let id = event["payload"]["attachments"][0]["id"]
        .as_str()
        .expect("id")
        .to_string();

    let (status, _, bytes) = fixture.api(
        "GET",
        &format!("/api/sessions/{stranger}/attachments/{id}"),
        None,
        &[],
    );
    assert_eq!(
        status,
        404,
        "another session's attachment must not be served: {}",
        String::from_utf8_lossy(&bytes)
    );
}

#[test]
fn a_body_that_is_not_base64_is_refused_and_leaves_no_turn() {
    let fixture = attachment_fixture("attach-bad64");
    let workspace =
        std::env::temp_dir().join(format!("forge-attach-bad-ws-{}", std::process::id()));
    let session = fixture.create_session(&workspace);
    let before = fs::read_to_string(fixture.server.path(&session)).expect("session log");

    for bad in ["not base64!", "abc", "AAAAAAAAA", "AA=A"] {
        let body = json!({
            "text": "broken",
            "attachments": [{ "kind": "image", "data": bad }],
        });
        let (status, bytes) = post_message(&fixture, &session, &body, "attach-bad");
        assert_eq!(
            status,
            400,
            "{bad} must be refused: {}",
            String::from_utf8_lossy(&bytes)
        );
        let error: Value = serde_json::from_slice(&bytes).expect("error body");
        assert_eq!(error["error"]["code"], "bad_request");
    }

    // A refused upload must not read as an accepted turn.
    let after = fs::read_to_string(fixture.server.path(&session)).expect("session log");
    assert_eq!(before, after, "a refused attachment must append nothing");
}

#[test]
fn an_image_without_a_body_is_refused() {
    let fixture = attachment_fixture("attach-nobody");
    let workspace =
        std::env::temp_dir().join(format!("forge-attach-nobody-ws-{}", std::process::id()));
    let session = fixture.create_session(&workspace);

    let body = json!({
        "text": "empty",
        "attachments": [{ "kind": "image" }],
    });
    let (status, bytes) = post_message(&fixture, &session, &body, "attach-none");
    assert_eq!(
        status,
        400,
        "an image without bytes: {}",
        String::from_utf8_lossy(&bytes)
    );
}

#[test]
fn a_file_attachment_names_a_workspace_path_and_carries_no_body() {
    let fixture = attachment_fixture("attach-file");
    let workspace =
        std::env::temp_dir().join(format!("forge-attach-file-ws-{}", std::process::id()));
    let session = fixture.create_session(&workspace);

    let body = json!({
        "text": "read that",
        "attachments": [{ "kind": "file", "path": "src/main.rs", "name": "main.rs" }],
    });
    let (status, bytes) = post_message(&fixture, &session, &body, "attach-file");
    assert_eq!(
        status,
        202,
        "a path attachment needs no body: {}",
        String::from_utf8_lossy(&bytes)
    );

    let log = fs::read_to_string(fixture.server.path(&session)).expect("session log");
    let user_line = log
        .lines()
        .find(|line| line.contains("user/message"))
        .expect("user line");
    assert!(user_line.contains("src/main.rs"), "path recorded");
    let event: Value = serde_json::from_str(user_line).expect("event json");
    let reference = &event["payload"]["attachments"][0];
    assert!(
        reference.get("id").is_none(),
        "a path attachment has no blob: {reference}"
    );
    assert_eq!(reference["bytes"], 0, "a path names bytes it did not read");
}

/// Standard base64, so the test sends what a real client sends.
fn base64_for(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let word = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((word >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((word >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((word >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(word & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// The capability snapshot is a separate resource from `/api/sessions`: it
/// describes this process, so it answers before any session exists.
#[test]
fn the_capability_report_is_served_before_any_session_exists() {
    let options = Options {
        addr: "127.0.0.1:0".to_string(),
        data_dir: unique_dir("capabilities"),
        version: "feature-test".to_string(),
        capabilities: forge_server::Report {
            mode: "workspace-write".to_string(),
            workspace: "C:/work".to_string(),
            model: "m1".to_string(),
            plugins: vec![
                forge_server::Entry::new("counter", "1.0.0 (node runtime)", true),
                forge_server::Entry::inactive("ghost", "0.1.0 (node runtime), not enabled"),
            ],
            skills: vec![forge_server::Entry::new("deploy", "ship it", true)],
            ..Default::default()
        },
        ..Options::default()
    };
    let fixture = Fixture::start(options);

    let (status, headers, bytes) = fixture.api("GET", "/api/capabilities", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    assert!(headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("content-type")
            && value.starts_with("application/json")));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["mode"], "workspace-write");
    assert_eq!(body["workspace"], "C:/work");
    assert_eq!(body["model"], "m1");
    // The row the browser was missing: installed, listed, and honestly off.
    let ghost = body["plugins"]
        .as_array()
        .expect("plugins")
        .iter()
        .find(|row| row["name"] == "ghost")
        .expect("the disabled plugin is named");
    assert_eq!(ghost["active"], false);
    assert_eq!(ghost["detail"], "0.1.0 (node runtime), not enabled");
    // Groups with nothing in them are present and empty, not absent.
    assert_eq!(body["teams"], json!([]));
    assert_eq!(body["bundles"], json!([]));
    assert_eq!(body["mcp"], json!([]));
}

#[test]
fn the_capability_report_is_read_only() {
    let fixture = Fixture::default();
    let (status, _, bytes) = fixture.api("POST", "/api/capabilities", Some(&json!({})), &[]);
    assert_eq!(status, 405, "{}", String::from_utf8_lossy(&bytes));
    let (status, _, bytes) = fixture.api("DELETE", "/api/capabilities", None, &[]);
    assert_eq!(status, 405, "{}", String::from_utf8_lossy(&bytes));
}

/// `present` hands the operator a path and nothing else; this route is the only
/// reader of the manifest that records it, so without it the tool's whole
/// purpose is invisible in the browser.
#[test]
fn the_present_manifest_is_served_newest_first() {
    let data_dir = unique_dir("presents");
    fs::create_dir_all(&data_dir).expect("data dir");
    let manifest = format!(
        "{}\n{}\n{}\n",
        json!({"ts": 1_700_000_000_000u64, "path": "reports/a.xlsx", "size": 10, "description": "the first"}),
        json!({"ts": 1_700_000_001_000u64, "path": "reports/b.docx", "size": 20, "description": "the second"}),
        "not json at all"
    );
    fs::write(data_dir.join("presented.jsonl"), manifest).expect("manifest");

    let fixture = fixture_for(&data_dir);
    let (status, headers, bytes) = fixture.api("GET", "/api/presents", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    assert!(headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("content-type")
            && value.starts_with("application/json")));

    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let presents = body["presents"].as_array().expect("presents");
    assert_eq!(presents.len(), 2);
    // The newest handoff is what an operator opened this pane to find.
    assert_eq!(presents[0]["path"], "reports/b.docx");
    assert_eq!(presents[0]["size"], 20);
    assert_eq!(presents[1]["path"], "reports/a.xlsx");
    // One torn line is counted, never a 500 and never a reason to hide the rest.
    assert_eq!(body["skipped"], 1);
    assert_eq!(body["truncated"], false);
}

#[test]
fn the_present_route_creates_nothing() {
    // A box that has never presented anything: the answer is an empty list, and
    // the read must not leave a manifest behind for the next reader to find.
    let data_dir = unique_dir("presents-empty");
    fs::create_dir_all(&data_dir).expect("data dir");
    let fixture = fixture_for(&data_dir);

    let (status, _, bytes) = fixture.api("GET", "/api/presents", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["presents"], json!([]));
    assert_eq!(body["skipped"], 0);
    assert!(
        !data_dir.join("presented.jsonl").exists(),
        "a read must not create the manifest it could not find"
    );

    let (status, _, _) = fixture.api("POST", "/api/presents", Some(&json!({})), &[]);
    assert_eq!(status, 405);
}

/// A fixture whose data dir is a path the test already prepared, so a board can
/// be written the way `forge team run` writes one before the server starts.
fn fixture_for(data_dir: &Path) -> Fixture {
    Fixture::start(Options {
        addr: "127.0.0.1:0".to_string(),
        data_dir: data_dir.to_path_buf(),
        version: "feature-test".to_string(),
        poll_interval: Duration::from_millis(2),
        delta_interval: Duration::from_millis(2),
        ..Options::default()
    })
}

/// Writes one run directory: `<dataDir>/teams/<name>/board.json` plus a
/// `workers/<worker>.jsonl` per worker. The board is text, so a test can write
/// one that will not parse as well as one that will.
fn write_board(
    data_dir: &Path,
    name: &str,
    board: Option<&str>,
    workers: &[(&str, Vec<Value>)],
) -> PathBuf {
    let dir = data_dir.join("teams").join(name);
    fs::create_dir_all(dir.join("workers")).expect("workers dir");
    if let Some(text) = board {
        fs::write(dir.join("board.json"), text).expect("board.json");
    }
    for (worker, lines) in workers {
        let mut text = String::new();
        for line in lines {
            text.push_str(&serde_json::to_string(line).expect("transcript line"));
            text.push('\n');
        }
        fs::write(dir.join("workers").join(format!("{worker}.jsonl")), text).expect("transcript");
    }
    dir
}

/// `board.json` as `crates/team/src/board.rs` writes it: camelCase, with the
/// fields a board view does not show still present.
fn board_json(tasks: Value) -> String {
    json!({ "version": 1, "revision": 3, "tasks": tasks }).to_string()
}

/// The transcript `crates/team/src/run.rs` leaves: a start line, an end line.
fn transcript(worker: &str, ok: bool, note: &str) -> Vec<Value> {
    vec![
        json!({
            "kind": "worker/start",
            "ts": 1_764_547_343_000i64,
            "worker": worker,
            "task": "t1",
            "prompt": "review the patch"
        }),
        json!({
            "kind": "worker/end",
            "ts": 1_764_547_350_000i64,
            "worker": worker,
            "ok": ok,
            "exit": if ok { json!(0) } else { Value::Null },
            "note": note,
            "text": "done"
        }),
    ]
}

#[test]
fn a_board_written_by_hand_is_listed() {
    let data_dir = unique_dir("teams-listed");
    let board = board_json(json!([
        {
            "id": "t1",
            "subject": "Add the teams route",
            "status": "completed",
            "owner": "api-reviewer",
            "revision": 2,
            "blockedBy": [],
            "writeScopes": ["crates/server/src"]
        },
        { "id": "t2", "subject": "Write the contract", "status": "pending", "revision": 1 }
    ]));
    write_board(
        &data_dir,
        "release-squad-20261118-002223",
        Some(&board),
        &[(
            "api-reviewer",
            transcript("api-reviewer", true, "1 turn(s), session s_abc"),
        )],
    );
    let fixture = fixture_for(&data_dir);

    let (status, headers, bytes) = fixture.api("GET", "/api/teams", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    assert!(headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("content-type")
            && value.starts_with("application/json")));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let boards = body["boards"].as_array().expect("boards");
    assert_eq!(boards.len(), 1, "{body}");
    let row = &boards[0];
    assert_eq!(row["board"], "release-squad-20261118-002223");
    assert_eq!(row["team"], "release-squad");
    // Chosen by `forge team run` and persisted nowhere, so it is an honest null
    // rather than a number this process would have had to invent.
    assert!(row.get("parallel").is_some(), "{row}");
    assert!(row["parallel"].is_null(), "{row}");
    assert_eq!(row["createdAt"], "2026-11-18T00:22:23Z");
    assert_eq!(
        row["tasks"],
        json!([
            {
                "id": "t1",
                "subject": "Add the teams route",
                "status": "completed",
                "owner": "api-reviewer"
            },
            {
                "id": "t2",
                "subject": "Write the contract",
                "status": "pending",
                "owner": null
            }
        ])
    );
    assert_eq!(
        row["workers"],
        json!([{
            "name": "api-reviewer",
            "ok": true,
            "note": "1 turn(s), session s_abc",
            "events": 2
        }])
    );
    // A board with nothing wrong carries no warning at all.
    assert!(row.get("diagnostics").is_none(), "{row}");
}

#[test]
fn one_board_is_served_unwrapped() {
    let data_dir = unique_dir("teams-detail");
    let board = board_json(json!([{ "id": "t1", "subject": "one", "status": "pending" }]));
    write_board(
        &data_dir,
        "release-squad-20261118-002223",
        Some(&board),
        &[],
    );
    let fixture = fixture_for(&data_dir);

    let (status, _, bytes) =
        fixture.api("GET", "/api/teams/release-squad-20261118-002223", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(body.get("boards").is_none(), "{body}");
    assert_eq!(body["board"], "release-squad-20261118-002223");
    assert_eq!(body["team"], "release-squad");
    assert_eq!(body["tasks"][0]["id"], "t1");

    let (status, _, bytes) = fixture.api("GET", "/api/teams/nobody-ran-this", None, &[]);
    assert_eq!(status, 404, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["error"]["code"], "not_found");
}

#[test]
fn an_unparseable_board_is_reported_without_hiding_the_others() {
    let data_dir = unique_dir("teams-junk");
    let good = board_json(json!([{ "id": "t1", "subject": "fine", "status": "completed" }]));
    write_board(
        &data_dir,
        "healthy-run-20261118-000000",
        Some(&good),
        &[("worker-a", transcript("worker-a", true, "ok"))],
    );
    // A run that died mid-write: truncated JSON, and no transcripts at all.
    let junk = write_board(
        &data_dir,
        "half-written-20261118-000001",
        Some("{ \"version\": 1"),
        &[],
    );
    fs::remove_dir(junk.join("workers")).expect("remove workers");
    // A directory that is not a board name is not a board.
    write_board(&data_dir, ".hidden-20261118-000002", Some(&good), &[]);
    let fixture = fixture_for(&data_dir);

    let (status, _, bytes) = fixture.api("GET", "/api/teams", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let boards = body["boards"].as_array().expect("boards");
    // Both boards are listed, newest first; the hidden directory is not one.
    assert_eq!(boards.len(), 2, "{body}");
    assert_eq!(boards[0]["board"], "half-written-20261118-000001");
    assert_eq!(boards[1]["board"], "healthy-run-20261118-000000");

    // What is known is still reported, and the reason it is all that is known.
    let broken = &boards[0];
    assert_eq!(broken["team"], "half-written");
    assert_eq!(broken["tasks"], json!([]));
    assert_eq!(broken["workers"], json!([]));
    let diagnostics = broken["diagnostics"].as_array().expect("diagnostics");
    let text = diagnostics
        .iter()
        .map(|line| line.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("board.json could not be parsed"), "{text}");
    assert!(text.contains("workers/ is missing"), "{text}");

    // The healthy board next door is untouched.
    assert_eq!(boards[1]["tasks"][0]["id"], "t1");
    assert!(boards[1].get("diagnostics").is_none(), "{}", boards[1]);

    // The same read, one board at a time, agrees with the list.
    let (status, _, bytes) =
        fixture.api("GET", "/api/teams/half-written-20261118-000001", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert!(body["diagnostics"].as_array().expect("diagnostics").len() >= 2);
}

#[test]
fn a_worker_that_has_not_finished_is_not_reported_as_failed() {
    let data_dir = unique_dir("teams-unfinished");
    let board =
        board_json(json!([{ "id": "t1", "subject": "still running", "status": "in_progress" }]));
    // A `worker/start` with no `worker/end`: the board was read mid-run, which
    // is the normal case while a team is working.
    write_board(
        &data_dir,
        "running-20261118-003000",
        Some(&board),
        &[(
            "worker-a",
            vec![json!({
                "kind": "worker/start",
                "ts": 1_764_547_343_000i64,
                "worker": "worker-a",
                "task": "t1",
                "prompt": "review the patch"
            })],
        )],
    );
    let fixture = fixture_for(&data_dir);

    let (status, _, bytes) = fixture.api("GET", "/api/teams", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let workers = body["boards"][0]["workers"].as_array().expect("workers");
    assert_eq!(workers.len(), 1, "{body}");
    // Null, not false. A worker that has not finished has not failed, and a
    // board read mid-run must not make every worker look broken.
    assert_eq!(
        workers[0]["ok"],
        Value::Null,
        "an unfinished worker is unknown, not failed: {}",
        workers[0]
    );
    // The reason it is unknown is still said out loud.
    let diagnostics = body["boards"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    let text = diagnostics
        .iter()
        .map(|line| line.as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("has no worker/end line"), "{text}");
}

#[test]
fn the_team_boards_route_is_read_only() {
    let data_dir = unique_dir("teams-read-only");
    write_board(&data_dir, "release-squad-20261118-002223", None, &[]);
    let fixture = fixture_for(&data_dir);

    for method in ["POST", "DELETE"] {
        let (status, headers, bytes) = fixture.api(method, "/api/teams", None, &[]);
        assert_eq!(status, 405, "{method}: {}", String::from_utf8_lossy(&bytes));
        assert!(headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("allow") && value == "GET"));
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["error"]["code"], "method_not_allowed");
    }
    let (status, _, bytes) = fixture.api(
        "POST",
        "/api/teams/release-squad-20261118-002223",
        None,
        &[],
    );
    assert_eq!(status, 405, "{}", String::from_utf8_lossy(&bytes));
    let (status, _, bytes) = fixture.api(
        "DELETE",
        "/api/teams/release-squad-20261118-002223",
        None,
        &[],
    );
    assert_eq!(status, 405, "{}", String::from_utf8_lossy(&bytes));

    // Nothing was created or removed by any of that.
    let dir = data_dir.join("teams").join("release-squad-20261118-002223");
    assert!(dir.is_dir());
    assert!(!dir.join("board.json").exists());
}

#[test]
fn a_board_name_that_is_a_path_is_not_found() {
    // The data dir holds a `token` file, so `../../token` is a name that would
    // actually resolve to something if it were ever treated as a path.
    let fixture = Fixture::default();
    for path in [
        "/api/teams/../../token",
        "/api/teams/..%2f..%2ftoken",
        "/api/teams/..",
        "/api/teams/.",
        "/api/teams/%2e%2e",
        "/api/teams/C:%5Ctoken",
    ] {
        let (status, _, bytes) = fixture.api("GET", path, None, &[]);
        assert_eq!(status, 404, "{path}: {}", String::from_utf8_lossy(&bytes));
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["error"]["code"], "not_found", "{path}");
    }
    let (status, _, bytes) = fixture.api("GET", "/api/teams/a%2fb", None, &[]);
    assert_eq!(status, 404, "{}", String::from_utf8_lossy(&bytes));
}

#[test]
fn a_machine_that_never_ran_a_team_lists_no_boards() {
    let data_dir = unique_dir("teams-absent");
    let fixture = fixture_for(&data_dir);

    let (status, _, bytes) = fixture.api("GET", "/api/teams", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body, json!({ "boards": [] }));
    // Reading a board list must not look like a run: the directory is still
    // absent, which is what makes the empty list honest.
    assert!(
        !data_dir.join("teams").exists(),
        "GET /api/teams created the teams directory"
    );
}

#[test]
fn a_server_embedded_in_a_test_still_names_every_capability_group() {
    let fixture = Fixture::default();
    let (status, _, bytes) = fixture.api("GET", "/api/capabilities", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    for key in [
        "mode",
        "workspace",
        "model",
        "plugins",
        "experts",
        "teams",
        "bundles",
        "skills",
        "mcp",
    ] {
        assert!(body.get(key).is_some(), "missing {key}: {body}");
        assert!(
            body[key].is_array() || body[key].is_string(),
            "{key}: {body}"
        );
    }
}
