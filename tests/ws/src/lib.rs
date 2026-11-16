#![cfg(test)]
//! Language-neutral golden WebSocket transcript runner for the Rust port.
//!
//! The corpus is a byte-for-byte mirror of the Go oracle's
//! `archive/forge-go/tests/conformance/cases/ws` at
//! `forge-rs/tests/conformance/cases/ws` (SHA256-verified when mirrored).
//! Nothing in this file rewrites a case: when the Rust server disagrees with the
//! frozen contract the assertion fails, and the disagreement is reported to the
//! lead rather than weakened here.
//!
//! Only `std::net` and `serde_json` are used for the client. `forge-server` is a
//! path dependency on the crate under test, not a new external dependency; the
//! runner needs it to set `ping_interval`/`delta_interval` without wall-clock
//! flake.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use forge_core::Result as ForgeResult;
use forge_server::{Options, Server, TurnContext, TurnRunner};
use serde_json::Value;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const WAIT: Duration = Duration::from_secs(20);
const TOKEN: &str = "ws-golden-token";

#[derive(Clone)]
struct DeltaSpec {
    turn: u32,
    step: u32,
    text: String,
}

struct Runner {
    release: Arc<Mutex<Receiver<()>>>,
    delta: Option<DeltaSpec>,
}

impl TurnRunner for Runner {
    fn run_turn(&self, ctx: &TurnContext, _session_id: &str, _text: &str) -> ForgeResult<()> {
        let receiver = self.release.lock().expect("release lock");
        let _ = receiver.recv();
        if let Some(delta) = &self.delta {
            ctx.delta(delta.turn, delta.step, &delta.text);
        }
        Ok(())
    }

    fn cancel(&self) -> ForgeResult<()> {
        Ok(())
    }
}

struct Case {
    name: String,
    session_id: String,
    ping_interval: Duration,
    seed_events: Vec<Value>,
    post: bool,
    live_events: Vec<Value>,
    runner_delta: Option<DeltaSpec>,
    connections: Vec<ConnectionCase>,
    mode: String,
    expect_http_status: u16,
}

struct ConnectionCase {
    after: u64,
    client_frames: Vec<Value>,
    client_expect: Vec<Value>,
    expect_frames: Vec<Value>,
    expect_close: u16,
    no_pong: bool,
}

fn corpus_dir() -> PathBuf {
    // Self-contained: tests/conformance/cases/ws in this tree. The eight files
    // are byte-identical to the Go oracle's corpus (SHA256 verified).
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("conformance")
        .join("cases")
        .join("ws")
}

fn load_cases() -> Vec<Case> {
    let dir = corpus_dir();
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no cases in {}", dir.display());
    paths
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()));
            let obj = value
                .as_object()
                .unwrap_or_else(|| panic!("{} is not an object", path.display()));
            let name = obj.get("name").and_then(Value::as_str).unwrap().to_string();
            let session_id = obj
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap()
                .to_string();
            let ping_ms = obj
                .get("pingIntervalMs")
                .and_then(Value::as_u64)
                .unwrap_or(30_000);
            let seed_events = array(obj, "seedEvents");
            let live_events = array(obj, "liveEvents");
            let runner_delta =
                obj.get("runnerDelta")
                    .and_then(Value::as_object)
                    .map(|d| DeltaSpec {
                        turn: d.get("turn").and_then(Value::as_u64).unwrap_or(0) as u32,
                        step: d.get("step").and_then(Value::as_u64).unwrap_or(0) as u32,
                        text: d
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    });
            let connections = array(obj, "connections")
                .into_iter()
                .map(|c| {
                    let c = c.as_object().unwrap();
                    ConnectionCase {
                        after: c.get("after").and_then(Value::as_u64).unwrap_or(0),
                        client_frames: array(c, "clientFrames"),
                        client_expect: array(c, "clientExpectFrames"),
                        expect_frames: array(c, "expectFrames"),
                        expect_close: c.get("expectClose").and_then(Value::as_u64).unwrap_or(0)
                            as u16,
                        no_pong: c.get("noPong").and_then(Value::as_bool).unwrap_or(false),
                    }
                })
                .collect();
            Case {
                name,
                session_id,
                ping_interval: Duration::from_millis(ping_ms),
                seed_events,
                post: obj.get("post").and_then(Value::as_bool).unwrap_or(false),
                live_events,
                runner_delta,
                connections,
                mode: obj
                    .get("mode")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                expect_http_status: obj
                    .get("expectHTTPStatus")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u16,
            }
        })
        .collect()
}

fn array(obj: &serde_json::Map<String, Value>, key: &str) -> Vec<Value> {
    obj.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn canonical(value: &Value) -> String {
    serde_json::to_string(value).expect("canonical JSON")
}

fn canonical_bytes(bytes: &[u8]) -> String {
    let value: Value = serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "frame is not JSON: {e}; raw: {}",
            String::from_utf8_lossy(bytes)
        )
    });
    canonical(&value)
}

fn temp_root() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("forge-rs-target"))
        .join("forge-ws-tmp")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = temp_root().join(format!("{tag}-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Harness {
    server: Server,
    addr: String,
    data_dir: PathBuf,
    _temp: TempDir,
}

impl Harness {
    fn start(case: &Case) -> (Self, Sender<()>) {
        let temp = TempDir::new("ws-golden");
        let data_dir = temp.0.join("data");
        fs::create_dir_all(data_dir.join("sessions")).unwrap();
        let (sender, receiver) = channel();
        let release = Arc::new(Mutex::new(receiver));
        let delta = case.runner_delta.clone();
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: data_dir.clone(),
            version: "conformance".to_string(),
            token: TOKEN.to_string(),
            new_runner: Some(Arc::new(move |_sid: &str| {
                Arc::new(Runner {
                    release: Arc::clone(&release),
                    delta: delta.clone(),
                }) as Arc<dyn TurnRunner>
            })),
            poll_interval: Duration::from_millis(2),
            delta_interval: Duration::from_millis(2),
            ping_interval: case.ping_interval,
            ..Options::default()
        };
        let server = Server::new(options).expect("start server");
        let worker = server.clone();
        thread::spawn(move || {
            let _ = worker.listen_and_serve();
        });
        let deadline = Instant::now() + WAIT;
        let addr = loop {
            if let Some(addr) = server.addr() {
                break addr;
            }
            assert!(Instant::now() < deadline, "server did not bind");
            thread::sleep(Duration::from_millis(2));
        };
        (
            Self {
                server,
                addr,
                data_dir,
                _temp: temp,
            },
            sender,
        )
    }

    fn log_path(&self, session_id: &str) -> PathBuf {
        self.data_dir
            .join("sessions")
            .join(format!("{session_id}.jsonl"))
    }

    fn write_seed(&self, case: &Case) {
        let mut text = String::new();
        for event in &case.seed_events {
            text.push_str(&canonical(event));
            text.push('\n');
        }
        fs::write(self.log_path(&case.session_id), text).expect("write seed");
    }

    fn append_live(&self, case: &Case) {
        if case.live_events.is_empty() {
            return;
        }
        let path = self.log_path(&case.session_id);
        let data = fs::read(&path).expect("read log after POST");
        let lines = data.iter().filter(|b| **b == b'\n').count();
        assert_eq!(
            lines,
            4,
            "expected 4 log lines after POST:\n{}",
            String::from_utf8_lossy(&data)
        );
        let mut text = String::new();
        for event in &case.live_events {
            text.push_str(&canonical(event));
            text.push('\n');
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open log");
        file.write_all(text.as_bytes()).expect("append live events");
    }

    fn post_message(&self, session_id: &str, text: &str) {
        let body = format!("{{\"text\":{}}}", serde_json::to_string(text).unwrap());
        let request = format!(
            "POST /api/sessions/{session_id}/messages HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer {TOKEN}\r\nX-Forge-Protocol: 1\r\nOrigin: http://{addr}\r\nIdempotency-Key: ws-golden-{session_id}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            body.len(),
            addr = self.addr
        );
        let mut stream = TcpStream::connect(&self.addr).expect("connect POST");
        stream.set_read_timeout(Some(WAIT)).unwrap();
        stream.write_all(request.as_bytes()).expect("write POST");
        let response = read_http_response(&mut stream).expect("read POST response");
        assert_eq!(response.status, 202, "POST messages: {}", response.body);
    }
}

struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

fn read_http_response(stream: &mut TcpStream) -> io::Result<HttpResponse> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
        if header.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "header too large",
            ));
        }
    }
    let text = String::from_utf8_lossy(&header).into_owned();
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        stream.read_exact(&mut body)?;
    }
    Ok(HttpResponse {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

struct WsClient {
    stream: TcpStream,
    no_pong: bool,
}

struct Incoming {
    op: u8,
    payload: Vec<u8>,
    code: u16,
}

fn dial(addr: &str, session_id: &str, after: u64, token: &str) -> (Option<WsClient>, HttpResponse) {
    let mut stream = TcpStream::connect(addr).expect("ws connect");
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let key = format!("ws-golden-{session_id}-{after}");
    let request = format!(
        "GET /api/sessions/{session_id}/stream?after={after} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nX-Forge-Protocol: 1\r\nOrigin: http://{addr}\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write handshake");
    let response = read_http_response(&mut stream).expect("read handshake");
    if response.status != 101 {
        return (None, response);
    }
    assert_eq!(
        response.headers.get("x-forge-protocol").map(String::as_str),
        Some("1")
    );
    let accept = response
        .headers
        .get("sec-websocket-accept")
        .cloned()
        .unwrap_or_default();
    let want_accept = base64(&sha1(format!("{key}{WS_GUID}").as_bytes()));
    // Byte for byte, padding included. RFC 6455 §4.2.2 defines the accept as the
    // base64 (padded) digest, and a browser compares it exactly: trimming the
    // `=` here once let a server that dropped it pass every test and fail every
    // real browser.
    assert_eq!(accept, want_accept, "bad Sec-WebSocket-Accept");
    (
        Some(WsClient {
            stream,
            no_pong: false,
        }),
        response,
    )
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut message = data.to_vec();
    let bits = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    for chunk in message.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

impl WsClient {
    fn write(&mut self, op: u8, payload: &[u8]) -> io::Result<()> {
        let mask = [0x11u8, 0x22, 0x33, 0x44];
        let mut frame = Vec::new();
        frame.push(0x80 | op);
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        let mut masked = Vec::with_capacity(len);
        for (i, b) in payload.iter().enumerate() {
            masked.push(b ^ mask[i % 4]);
        }
        frame.extend_from_slice(&masked);
        self.stream.write_all(&frame)
    }

    fn read(&mut self) -> io::Result<Incoming> {
        let mut head = [0u8; 2];
        self.stream.read_exact(&mut head)?;
        let op = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let mut len = (head[1] & 0x7f) as usize;
        if len == 126 {
            let mut b = [0u8; 2];
            self.stream.read_exact(&mut b)?;
            len = u16::from_be_bytes(b) as usize;
        } else if len == 127 {
            let mut b = [0u8; 8];
            self.stream.read_exact(&mut b)?;
            len = u64::from_be_bytes(b) as usize;
        }
        let mut mask = [0u8; 4];
        if masked {
            self.stream.read_exact(&mut mask)?;
        }
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload)?;
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        let code = if op == 8 && payload.len() >= 2 {
            u16::from_be_bytes([payload[0], payload[1]])
        } else {
            0
        };
        Ok(Incoming { op, payload, code })
    }

    fn read_text(&mut self) -> io::Result<String> {
        loop {
            let incoming = self.read()?;
            match incoming.op {
                1 => return Ok(canonical_bytes(&incoming.payload)),
                9 => {
                    if !self.no_pong {
                        self.write(10, &incoming.payload)?;
                    }
                }
                8 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("unexpected close {}", incoming.code),
                    ))
                }
                _ => {}
            }
        }
    }

    fn expect_control_ping(&mut self) -> io::Result<()> {
        loop {
            let incoming = self.read()?;
            if incoming.op == 9 {
                return self.write(10, &incoming.payload);
            }
            if incoming.op == 10 {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected control ping, got opcode {}: {}",
                    incoming.op,
                    String::from_utf8_lossy(&incoming.payload)
                ),
            ));
        }
    }

    fn await_close(&mut self) -> io::Result<u16> {
        loop {
            let incoming = self.read()?;
            if incoming.op == 8 {
                return Ok(incoming.code);
            }
            if incoming.op == 9 && !self.no_pong {
                self.write(10, &incoming.payload)?;
            }
        }
    }
}

fn run_case(case: &Case) {
    let (harness, release) = Harness::start(case);
    harness.write_seed(case);

    if case.mode == "policy" {
        let (client, response) = dial(&harness.addr, &case.session_id, 0, "wrong-token");
        assert!(client.is_none(), "policy handshake unexpectedly upgraded");
        assert_eq!(
            response.status, case.expect_http_status,
            "policy status; body: {}",
            response.body
        );
        return;
    }

    if case.post {
        harness.post_message(&case.session_id, "golden prompt");
        harness.append_live(case);
    }
    let mut released = false;
    let mut release_runner = || {
        if !released && case.runner_delta.is_some() {
            released = true;
            let _ = release.send(());
        }
    };
    for (ci, connection) in case.connections.iter().enumerate() {
        let (client, response) = dial(&harness.addr, &case.session_id, connection.after, TOKEN);
        let Some(mut client) = client else {
            panic!(
                "connection {ci}: handshake status {}: {}",
                response.status, response.body
            );
        };
        client.no_pong = connection.no_pong;
        for (fi, expected) in connection.expect_frames.iter().enumerate() {
            let want = canonical(expected);
            if want == "\"$controlPing\"" {
                client
                    .expect_control_ping()
                    .unwrap_or_else(|e| panic!("connection {ci} frame {fi}: {e}"));
                continue;
            }
            if want.contains("\"kind\":\"delta\"") {
                release_runner();
            }
            let got = client
                .read_text()
                .unwrap_or_else(|e| panic!("connection {ci} frame {fi}: {e}"));
            assert_eq!(got, want, "connection {ci} frame {fi}");
        }
        for (i, frame) in connection.client_frames.iter().enumerate() {
            client
                .write(1, canonical(frame).as_bytes())
                .unwrap_or_else(|e| panic!("send client frame {i}: {e}"));
            if let Some(expected) = connection.client_expect.get(i) {
                let got = client
                    .read_text()
                    .unwrap_or_else(|e| panic!("client frame {i} reply: {e}"));
                assert_eq!(got, canonical(expected), "client frame {i} reply");
            }
        }
        if connection.expect_close != 0 {
            if !connection.no_pong {
                client.write(8, &[0x03, 0xE8]).unwrap();
            }
            let code = client
                .await_close()
                .unwrap_or_else(|e| panic!("connection {ci} close: {e}"));
            assert_eq!(code, connection.expect_close, "connection {ci} close code");
        }
    }
    release_runner();
    let _ = harness.server.shutdown();
}

#[test]
fn golden_ws_transcripts() {
    let cases = load_cases();
    let mut failures = Vec::new();
    for case in &cases {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_case(case)));
        if let Err(payload) = result {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".to_string());
            failures.push(format!("{}\n{message}", case.name));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} golden WS cases failed:\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}
