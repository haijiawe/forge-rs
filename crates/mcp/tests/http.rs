//! Streamable HTTP conformance. The server is a tiny plain-TCP HTTP/1.1 peer on
//! 127.0.0.1; no real network and no external HTTP client crate are involved.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use forge_mcp::{dial, Client, Error, NoopLogger, Server, Transport};
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    BadVersion,
    Sse,
    ChunkedSse,
    Gone,
    Error500,
    Oversized,
    Malformed,
    IdMismatch,
    TransportGone,
    Timeout,
    ToolError,
}

#[derive(Default)]
struct Captured {
    protocols: Mutex<Vec<String>>,
    sessions: Mutex<Vec<String>>,
    headers: Mutex<Vec<String>>,
    count: AtomicUsize,
}

struct FakeHttp {
    addr: SocketAddr,
    state: Arc<Captured>,
    done: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeHttp {
    fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local fake server");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("local address");
        let state = Arc::new(Captured::default());
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        let worker_state = state.clone();
        let worker = thread::spawn(move || {
            while !worker_done.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => handle(stream, mode, &worker_state),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            state,
            done,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn server(fake: &FakeHttp, timeout_ms: u64) -> Server {
    Server {
        name: "http-test".into(),
        transport: Transport::Http,
        url: fake.url(),
        headers: vec!["X-Forge-Test: yes".into()],
        timeout_ms,
        ..Server::default()
    }
}

fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let header = format!(
        "{status}\r\nContent-Type: {content_type}\r\nMcp-Session-Id: sess-42\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn write_chunked(stream: &mut TcpStream, chunks: &[Vec<u8>]) {
    let header = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: sess-42\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(header);
    for chunk in chunks {
        let _ = stream.write_all(format!("{:X}\r\n", chunk.len()).as_bytes());
        let _ = stream.write_all(chunk);
        let _ = stream.write_all(b"\r\n");
        let _ = stream.flush();
        thread::sleep(Duration::from_millis(1));
    }
    let _ = stream.write_all(b"0\r\n\r\n");
    let _ = stream.flush();
}

fn handle(mut stream: TcpStream, mode: Mode, state: &Captured) {
    // The listener is non-blocking so the accept loop can be stopped, and on
    // Windows an accepted socket *inherits* that flag. Without this line the
    // fixture's writes fail with `WouldBlock` once the send buffer fills (~320 KiB),
    // the handler returns early and closes, and the client then reports a short
    // body instead of the cap it was asked to enforce — the test would be reporting
    // the fixture's failure, not the client's behaviour.
    stream
        .set_nonblocking(false)
        .expect("blocking accepted stream");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut length = 0usize;
    let mut protocol = String::new();
    let mut session = String::new();
    let mut x_test = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "mcp-protocol-version" => protocol = value.to_string(),
                "mcp-session-id" => session = value.to_string(),
                "x-forge-test" => x_test = value.to_string(),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; length.min(16 << 20)];
    let _ = reader.read_exact(&mut body);
    state.count.fetch_add(1, Ordering::AcqRel);
    state.protocols.lock().expect("lock").push(protocol);
    state.sessions.lock().expect("lock").push(session);
    state.headers.lock().expect("lock").push(x_test);

    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let id = request.get("id").cloned().unwrap_or(Value::Null);

    if mode == Mode::TransportGone && method == "tools/call" {
        return;
    }
    if mode == Mode::Gone && method == "tools/call" {
        let _ = stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 3\r\nConnection: close\r\n\r\ngone",
        );
        let _ = stream.flush();
        return;
    }
    if mode == Mode::Timeout && method == "tools/call" {
        let tool = request
            .get("params")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if tool == "hang" {
            thread::sleep(Duration::from_millis(700));
        }
    }

    let result: Value = match method {
        "initialize" => json!({
            "protocolVersion": if mode == Mode::BadVersion { "1999-01-01" } else { "2025-06-18" },
            "serverInfo": {"name":"fake-http","version":"2.0"}
        }),
        "tools/list" => {
            json!({"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]})
        }
        "notifications/initialized" => json!({}),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or(Value::Null);
            let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
            if mode == Mode::ToolError && tool == "boom" {
                let payload =
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"boom"}});
                let body = serde_json::to_vec(&payload).expect("encode error");
                write_response(&mut stream, "HTTP/1.1 200 OK", "application/json", &body);
                return;
            }
            let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
            json!({"content":[{"type":"text","text":arguments.to_string()}]})
        }
        _ => json!({}),
    };

    if method == "tools/call" && matches!(mode, Mode::Sse | Mode::ChunkedSse) {
        let event = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result}))
            .expect("encode result");
        let notification = br#"event: message
data: {"jsonrpc":"2.0","method":"notifications/message","params":{"data":"hello"}}

"#;
        let mut data = Vec::new();
        data.extend_from_slice(notification);
        data.extend_from_slice(b"event: message\ndata: ");
        data.extend_from_slice(&event);
        data.extend_from_slice(b"\n\n");
        if mode == Mode::ChunkedSse {
            write_chunked(&mut stream, &[data[..3].to_vec(), data[3..].to_vec()]);
        } else {
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: sess-42\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            );
            let _ = stream.write_all(header.as_bytes());
            for chunk in data.chunks(3) {
                let _ = stream.write_all(chunk);
                let _ = stream.flush();
                thread::sleep(Duration::from_millis(1));
            }
        }
        return;
    }

    let payload = match mode {
        Mode::Malformed if method == "tools/call" => b"not json at all".to_vec(),
        Mode::IdMismatch if method == "tools/call" => {
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":999,"result":result})).expect("encode")
        }
        Mode::Oversized if method == "tools/call" => {
            let oversized = vec![b'x'; (8 << 20) + 100];
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                oversized.len()
            );
            let _ = stream.write_all(header.as_bytes());
            // Hand the whole body over: the client stops reading at the cap, so this
            // write legitimately ends in a reset once its receive buffer is full.
            // That reset is part of what an oversized body looks like on the wire;
            // it must not turn into a write error that aborts the handler first.
            let _ = stream.write_all(&oversized);
            let _ = stream.flush();
            return;
        }
        _ => serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result})).expect("encode"),
    };
    if mode == Mode::Error500 && method == "tools/call" {
        write_response(
            &mut stream,
            "HTTP/1.1 500 Internal Server Error",
            "text/plain",
            b"internal boom",
        );
        return;
    }
    write_response(&mut stream, "HTTP/1.1 200 OK", "application/json", &payload);
}

fn connect(mode: Mode, timeout_ms: u64) -> (FakeHttp, Box<dyn Client>) {
    let fake = FakeHttp::start(mode);
    let config = server(&fake, timeout_ms);
    let client = dial(&config, Arc::new(NoopLogger)).expect("dial");
    (fake, client)
}

fn text(result: &forge_mcp::CallResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.text.clone())
        .unwrap_or_default()
}

#[test]
fn dial_does_not_handshake_and_initialize_sends_headers() {
    let (fake, mut client) = connect(Mode::Normal, 0);
    assert_eq!(
        fake.state.count.load(Ordering::Acquire),
        0,
        "Dial sent a request"
    );
    let info = client.initialize().expect("initialize");
    assert_eq!(info.name, "fake-http");
    let protocols = fake.state.protocols.lock().expect("lock");
    assert_eq!(protocols.len(), 2, "requests: {protocols:?}");
    assert!(
        protocols[0].is_empty(),
        "protocol header before initialize: {protocols:?}"
    );
    assert_eq!(protocols[1], "2025-06-18");
}

#[test]
fn protocol_header_and_session_are_sent_after_initialize() {
    let (fake, mut client) = connect(Mode::Normal, 0);
    client.initialize().expect("initialize");
    let result = client.call("echo", json!({"x":1})).expect("call");
    assert_eq!(text(&result), "{\"x\":1}");
    let protocols = fake.state.protocols.lock().expect("lock");
    assert_eq!(protocols.last().map(String::as_str), Some("2025-06-18"));
    let sessions = fake.state.sessions.lock().expect("lock");
    assert_eq!(sessions.last().map(String::as_str), Some("sess-42"));
    assert!(sessions.first().map(String::is_empty).unwrap_or(false));
    let headers = fake.state.headers.lock().expect("lock");
    assert!(
        headers.iter().all(|h| h == "yes"),
        "configured headers missing: {headers:?}"
    );
}

#[test]
fn configured_headers_are_rejected_when_malformed() {
    let fake = FakeHttp::start(Mode::Normal);
    let mut config = server(&fake, 0);
    config.headers.push("broken".into());
    let error = match dial(&config, Arc::new(NoopLogger)) {
        Err(error) => error,
        Ok(_) => panic!("malformed header was accepted"),
    };
    assert!(
        matches!(error, Error::Config(_)),
        "unexpected error: {error}"
    );
}

#[test]
fn wrong_protocol_version_is_typed() {
    let (fake, mut client) = connect(Mode::BadVersion, 0);
    let error = client.initialize().expect_err("version mismatch");
    assert!(error.is_protocol_version(), "unexpected error: {error}");
    let _ = client.close();
    drop(fake);
}

#[test]
fn sse_stream_notification_is_skipped() {
    let (fake, mut client) = connect(Mode::Sse, 3_000);
    client.initialize().expect("initialize");
    let result = client.call("echo", json!({"sse":true})).expect("call");
    assert_eq!(text(&result), "{\"sse\":true}");
    let _ = client.close();
    drop(fake);
}

#[test]
fn chunked_sse_stream_is_parsed() {
    let (fake, mut client) = connect(Mode::ChunkedSse, 3_000);
    client.initialize().expect("initialize");
    let result = client.call("echo", json!({"chunk":true})).expect("call");
    assert_eq!(text(&result), "{\"chunk\":true}");
    let _ = client.close();
    drop(fake);
}

#[test]
fn stream_ending_before_reply_is_server_gone() {
    let (fake, mut client) = connect(Mode::TransportGone, 1_000);
    client.initialize().expect("initialize");
    let error = client.call("echo", json!({})).expect_err("no reply");
    assert!(error.is_server_gone(), "unexpected error: {error}");
    let _ = client.close();
    drop(fake);
}

#[test]
fn missing_session_is_server_gone_and_sticky() {
    let (fake, mut client) = connect(Mode::Gone, 1_000);
    client.initialize().expect("initialize");
    let error = client.call("echo", json!({})).expect_err("session gone");
    assert!(error.is_server_gone(), "unexpected error: {error}");
    let later = client.call("echo", json!({})).expect_err("still gone");
    assert!(later.is_server_gone(), "unexpected error: {later}");
    let _ = client.close();
    drop(fake);
}

#[test]
fn non_2xx_is_a_typed_error() {
    let (fake, mut client) = connect(Mode::Error500, 1_000);
    client.initialize().expect("initialize");
    let error = client.call("echo", json!({})).expect_err("500");
    match &error {
        Error::HttpStatus { status, detail } => {
            assert_eq!(*status, 500);
            assert!(detail.contains("internal boom"), "detail was {detail}");
        }
        other => panic!("unexpected error: {other}"),
    }
    let _ = client.close();
    drop(fake);
}

#[test]
fn malformed_json_document_is_a_protocol_error() {
    let (fake, mut client) = connect(Mode::Malformed, 1_000);
    client.initialize().expect("initialize");
    let error = client.call("echo", json!({})).expect_err("malformed");
    assert!(
        matches!(error, Error::Protocol(_)),
        "unexpected error: {error}"
    );
    let _ = client.close();
    drop(fake);
}

#[test]
fn mismatched_response_id_is_rejected() {
    let (fake, mut client) = connect(Mode::IdMismatch, 1_000);
    client.initialize().expect("initialize");
    let error = client.call("echo", json!({})).expect_err("wrong id");
    assert!(
        matches!(error, Error::Protocol(_)),
        "unexpected error: {error}"
    );
    let _ = client.close();
    drop(fake);
}

#[test]
fn oversized_body_is_capped() {
    let (fake, mut client) = connect(Mode::Oversized, 1_000);
    client.initialize().expect("initialize");
    let error = client.call("echo", json!({})).expect_err("oversized");
    let message = error.to_string();
    assert!(
        message.contains("response body exceeds 8388608 bytes"),
        "unexpected error: {error}"
    );
    let _ = client.close();
    drop(fake);
}

#[test]
fn http_timeout_is_not_sticky() {
    let (fake, mut client) = connect(Mode::Timeout, 500);
    client.initialize().expect("initialize");
    let error = client.call("hang", json!({})).expect_err("timeout");
    assert!(
        matches!(error, Error::Timeout(_)),
        "unexpected error: {error}"
    );
    let result = client.call("echo", json!({"ok":true})).expect("later call");
    assert_eq!(text(&result), "{\"ok\":true}");
    let _ = client.close();
    drop(fake);
}

#[test]
fn tool_error_is_a_protocol_failure() {
    let (fake, mut client) = connect(Mode::ToolError, 1_000);
    client.initialize().expect("initialize");
    let error = client.call("boom", json!({})).expect_err("tool error");
    assert!(
        matches!(error, Error::Protocol(_)),
        "unexpected error: {error}"
    );
    let _ = client.close();
    drop(fake);
}

#[test]
fn close_is_idempotent() {
    let (fake, mut client) = connect(Mode::Normal, 0);
    client.initialize().expect("initialize");
    client.close().expect("close");
    client.close().expect("close again");
    let error = client.call("echo", json!({})).expect_err("closed");
    assert!(error.is_server_gone(), "unexpected error: {error}");
    drop(fake);
}

#[test]
fn dial_rejects_missing_or_bad_url() {
    let config = Server {
        name: "bad".into(),
        transport: Transport::Http,
        ..Server::default()
    };
    let error = match dial(&config, Arc::new(NoopLogger)) {
        Err(error) => error,
        Ok(_) => panic!("missing url was accepted"),
    };
    assert!(
        matches!(error, Error::Config(_)),
        "unexpected error: {error}"
    );
    let config = Server {
        name: "bad".into(),
        transport: Transport::Http,
        url: "ftp://example.com".into(),
        ..Server::default()
    };
    let error = match dial(&config, Arc::new(NoopLogger)) {
        Err(error) => error,
        Ok(_) => panic!("bad scheme was accepted"),
    };
    assert!(
        matches!(error, Error::Config(_)),
        "unexpected error: {error}"
    );
}
