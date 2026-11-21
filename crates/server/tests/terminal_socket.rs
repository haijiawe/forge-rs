//! The terminal socket: the second WebSocket this server offers.
//!
//! These tests drive the real listener with a stand-in runner, because what is
//! under test is the seam the CLI fills in: validation, the two log lines, the
//! frames, cancellation, and the read-only refusal. Whether a process actually
//! starts is the CLI's test to make, and it does.

use hearth_server::api::{Options, TerminalOutcome, TerminalRequest, TerminalRunner, TerminalSink};
use hearth_server::{Server, WS_SUBPROTOCOL};
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
        "hearth-terminal-{}-{}-{}",
        label,
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A runner that answers immediately, so a test can assert the frames without
/// inventing a process.
struct EchoRunner;

impl TerminalRunner for EchoRunner {
    fn run(
        &self,
        _request: &TerminalRequest,
        sink: Arc<dyn TerminalSink>,
    ) -> hearth_core::Result<TerminalOutcome> {
        sink.output("stdout", b"hello\n");
        sink.output("stderr", b"oops\n");
        Ok(TerminalOutcome {
            exit_code: 0,
            duration_ms: 3,
            cancelled: false,
            stdout_tail: "hello\n".to_string(),
            stderr_tail: "oops\n".to_string(),
            message: String::new(),
        })
    }
}

/// A runner that stays alive until cancelled, so a second run and a cancel have
/// something live to act on.
struct HoldRunner;

impl TerminalRunner for HoldRunner {
    fn run(
        &self,
        request: &TerminalRequest,
        sink: Arc<dyn TerminalSink>,
    ) -> hearth_core::Result<TerminalOutcome> {
        sink.output("stdout", b"started\n");
        while !request.cancel.is_cancelled() {
            thread::sleep(Duration::from_millis(5));
        }
        Ok(TerminalOutcome {
            exit_code: 130,
            duration_ms: 5,
            cancelled: true,
            stdout_tail: "started\n".to_string(),
            stderr_tail: String::new(),
            message: String::new(),
        })
    }
}

/// The factory the server calls once per session — `Options.new_terminal`'s type.
type TerminalFactory = Option<Arc<dyn Fn(&str) -> Arc<dyn TerminalRunner> + Send + Sync>>;

struct Fixture {
    addr: String,
    token: String,
    server: Server,
    stop: Arc<AtomicBool>,
    _temp: TempGuard,
}

impl Fixture {
    fn start(new_terminal: TerminalFactory) -> Self {
        let data_dir = unique_dir("socket");
        // The workspace must exist before a session can be opened in it.
        let workspace = data_dir.join("workspace");
        let _ = fs::create_dir_all(&workspace);
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: data_dir.clone(),
            version: "terminal-test".to_string(),
            poll_interval: Duration::from_millis(2),
            delta_interval: Duration::from_millis(2),
            new_terminal,
            ..Options::default()
        };
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
            stop: Arc::new(AtomicBool::new(false)),
            _temp: TempGuard(data_dir),
        }
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
            ("X-Hearth-Protocol", "1"),
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

    fn create_session(&self) -> String {
        let workspace = self._temp.0.join("workspace");
        let body = json!({ "workspace": workspace.to_string_lossy(), "model": "terminal-model" });
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

    /// Open the terminal socket the way a browser does: cookie credential and
    /// subprotocol, no Authorization and no X-Hearth-Protocol header.
    fn upgrade(&self, session: &str) -> TcpStream {
        let mut stream = TcpStream::connect(&self.addr).expect("connect ws");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let request = format!(
            "GET /api/sessions/{session}/terminal HTTP/1.1\r\n\
             Host: {}\r\n\
             Upgrade: websocket\r\n\
             Connection: keep-alive, Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Protocol: {WS_SUBPROTOCOL}\r\n\
             Origin: http://{}\r\n\
             Cookie: hearth_token={}\r\n\r\n",
            self.addr, self.addr, self.token
        );
        stream.write_all(request.as_bytes()).expect("send upgrade");
        stream
    }

    /// The events page, as the UI would read it.
    fn events(&self, session: &str) -> Vec<Value> {
        let (status, _, bytes) = self.api(
            "GET",
            &format!("/api/sessions/{session}/events?after=0"),
            None,
            &[],
        );
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
        let value: Value = serde_json::from_slice(&bytes).expect("decode events");
        value["events"].as_array().cloned().unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.server.shutdown();
    }
}

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

fn read_upgrade_head(stream: &mut TcpStream) -> String {
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("handshake byte");
        response.push(byte[0]);
    }
    String::from_utf8(response).expect("handshake utf8")
}

fn send_json(stream: &mut TcpStream, value: &Value) {
    let body = serde_json::to_vec(value).expect("encode frame");
    write_masked_frame(stream, 0x1, &body);
}

fn read_json(stream: &mut TcpStream) -> Value {
    let (opcode, payload) = read_ws_frame(stream);
    assert_eq!(opcode, 0x1, "expected a text frame");
    serde_json::from_slice(&payload).expect("decode frame")
}

fn echo() -> TerminalFactory {
    Some(Arc::new(|_session: &str| {
        Arc::new(EchoRunner) as Arc<dyn TerminalRunner>
    }))
}

#[test]
fn a_read_only_session_has_no_terminal() {
    // None is what "hearth web --sandbox read-only" passes: the refusal happens
    // before the upgrade, so a client learns why instead of watching a socket
    // open and immediately close.
    let fixture = Fixture::start(None);
    let session = fixture.create_session();
    let (status, headers, bytes) = fixture.api(
        "GET",
        &format!("/api/sessions/{session}/terminal"),
        None,
        &[("Upgrade", "websocket")],
    );
    assert_eq!(status, 403, "{}", String::from_utf8_lossy(&bytes));
    assert!(
        headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("Content-Type")),
        "a refusal is a JSON error, not a bare 403"
    );
    let value: Value = serde_json::from_slice(&bytes).expect("decode refusal");
    assert_eq!(value["error"]["code"], "forbidden");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("read-only"),
        "{value}"
    );
}

#[test]
fn a_command_streams_output_and_writes_both_lines() {
    let fixture = Fixture::start(echo());
    let session = fixture.create_session();
    let mut stream = fixture.upgrade(&session);
    let head = read_upgrade_head(&mut stream);
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(
        head.contains(&format!("Sec-WebSocket-Protocol: {WS_SUBPROTOCOL}")),
        "{head}"
    );

    send_json(
        &mut stream,
        &json!({ "kind": "run", "command": "echo hello", "cwd": "" }),
    );

    let start = read_json(&mut stream);
    assert_eq!(start["kind"], "start");
    assert_eq!(start["command"], "echo hello");
    assert_eq!(start["protocol"], 1);

    let mut streams: Vec<(String, String)> = Vec::new();
    let exit;
    loop {
        let frame = read_json(&mut stream);
        match frame["kind"].as_str().unwrap_or("") {
            "output" => streams.push((
                frame["stream"].as_str().unwrap_or("").to_string(),
                frame["text"].as_str().unwrap_or("").to_string(),
            )),
            "exit" => {
                exit = frame;
                break;
            }
            other => panic!("unexpected frame kind {other}: {frame}"),
        }
    }
    assert!(
        streams
            .iter()
            .any(|(stream, text)| stream == "stdout" && text == "hello\n"),
        "{streams:?}"
    );
    assert!(
        streams
            .iter()
            .any(|(stream, text)| stream == "stderr" && text == "oops\n"),
        "{streams:?}"
    );
    assert_eq!(exit["exitCode"], 0);
    assert_eq!(exit["cancelled"], false);
    assert_eq!(exit["stdoutTail"], "hello\n");

    // And the log: one line before the process, one after, both carrying the
    // command, so a reader who was never on the socket can tell what ran.
    let events = fixture.events(&session);
    let runs: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "terminal/run")
        .collect();
    let ends: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "terminal/end")
        .collect();
    assert_eq!(runs.len(), 1, "{events:#?}");
    assert_eq!(ends.len(), 1, "{events:#?}");
    assert_eq!(runs[0]["payload"]["command"], "echo hello");
    assert_eq!(ends[0]["payload"]["exitCode"], 0);
    assert_eq!(ends[0]["payload"]["stdoutTail"], "hello\n");
}

#[test]
fn a_second_run_while_one_is_live_is_denied_and_cancel_stops_it() {
    let fixture = Fixture::start(Some(Arc::new(|_session: &str| {
        Arc::new(HoldRunner) as Arc<dyn TerminalRunner>
    })));
    let session = fixture.create_session();
    let mut stream = fixture.upgrade(&session);
    let head = read_upgrade_head(&mut stream);
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");

    send_json(&mut stream, &json!({ "kind": "run", "command": "sleep 5" }));
    let start = read_json(&mut stream);
    assert_eq!(start["kind"], "start");

    // A second run while the first is still in flight is refused, and refusing
    // it does not interrupt what is running: its output keeps arriving.
    send_json(&mut stream, &json!({ "kind": "run", "command": "sleep 5" }));
    let mut saw_output = false;
    let mut denied = Value::Null;
    while !saw_output || denied["kind"] != "denied" {
        let frame = read_json(&mut stream);
        match frame["kind"].as_str().unwrap_or("") {
            "output" => saw_output = true,
            "denied" => denied = frame,
            other => panic!("unexpected frame kind {other}: {frame}"),
        }
    }
    assert!(saw_output, "the live command must keep streaming");
    assert!(
        denied["reason"]
            .as_str()
            .unwrap_or("")
            .contains("already running"),
        "{denied}"
    );

    send_json(&mut stream, &json!({ "kind": "cancel" }));
    let exit = loop {
        let frame = read_json(&mut stream);
        if frame["kind"] == "exit" {
            break frame;
        }
    };
    assert_eq!(exit["cancelled"], true);

    // A refused frame writes nothing: only the one command that ran has lines.
    let events = fixture.events(&session);
    let runs = events
        .iter()
        .filter(|event| event["type"] == "terminal/run");
    assert_eq!(runs.count(), 1, "{events:#?}");
}

#[test]
fn a_working_directory_outside_the_workspace_is_refused() {
    let fixture = Fixture::start(echo());
    let session = fixture.create_session();
    let mut stream = fixture.upgrade(&session);
    let _ = read_upgrade_head(&mut stream);

    send_json(
        &mut stream,
        &json!({ "kind": "run", "command": "dir", "cwd": "../../etc" }),
    );
    let denied = read_json(&mut stream);
    assert_eq!(denied["kind"], "denied");
    assert!(
        denied["reason"].as_str().unwrap_or("").contains("relative"),
        "{denied}"
    );
}

#[test]
fn an_unknown_frame_kind_is_refused_without_closing() {
    let fixture = Fixture::start(echo());
    let session = fixture.create_session();
    let mut stream = fixture.upgrade(&session);
    let _ = read_upgrade_head(&mut stream);

    send_json(&mut stream, &json!({ "kind": "watch" }));
    let denied = read_json(&mut stream);
    assert_eq!(denied["kind"], "denied");

    // And the socket survives doing nothing for a moment. A terminal is idle
    // between commands, and this listener accepts sockets in non-blocking mode,
    // so a panel that opened and waited before typing anything is the ordinary
    // case: an empty read here once ended the whole session.
    thread::sleep(Duration::from_millis(50));

    send_json(&mut stream, &json!({ "kind": "run", "command": "echo hi" }));
    let start = read_json(&mut stream);
    assert_eq!(start["kind"], "start");
}
