//! Language-neutral end-to-end acceptance suite for the Rust `forge` port.
//!
//! Everything runs on loopback with a scripted OpenAI-compatible SSE provider
//! inside this test process: no real network, no API key, no external tools.
//! The real compiled `forge` binary is spawned as a child process; its stdout,
//! stderr, event log and HTTP surface are checked against the Go oracle.

use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const MODEL: &str = "fake";
const API_KEY: &str = "e2e-test-key";
const ANSWER: &str = "e2e streamed answer";
const HTTP_ANSWER: &str = "http turn answer";
const TOOL_MARKER: &str = "E2E_TOOL_MARKER";
const FACT: &str = "ZEBRA42";
const TIMEOUT: Duration = Duration::from_secs(20);
static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Paths and temporary directories
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    fs::canonicalize(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(".."),
    )
    .unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    })
}

fn target_root() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("target"))
}

/// Cargo only sets `CARGO_BIN_EXE_forge` for tests in the crate that owns the
/// binary, so fall back to the shared target directory when it is absent.
fn forge_bin() -> PathBuf {
    if let Ok(value) = env::var("CARGO_BIN_EXE_forge") {
        return PathBuf::from(value);
    }
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let exe = if cfg!(windows) { "forge.exe" } else { "forge" };
    let path = target_root().join(profile).join(exe);
    assert!(
        path.is_file(),
        "forge binary not found at {}; build it first with `cargo build -p forge-cli`",
        path.display()
    );
    path
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = target_root()
            .join("e2e-tmp")
            .join(format!("{tag}-{}-{nonce}-{seq}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A directory that can be passed to `--workspace` / POST /api/sessions.
fn make_workspace(root: &Path) -> PathBuf {
    let path = root.join("workspace");
    fs::create_dir_all(&path).unwrap();
    fs::canonicalize(&path).unwrap()
}

// ---------------------------------------------------------------------------
// Fake OpenAI-compatible SSE provider (std::net only)
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Reply {
    Text(String),
    Tool {
        call_id: String,
        name: String,
        args: String,
    },
    /// Answer using the tool output carried in the provider's second request.
    EchoTool,
}

struct ProviderState {
    requests: Vec<Value>,
    script: VecDeque<Reply>,
}

struct FakeProvider {
    base_url: String,
    state: Arc<Mutex<ProviderState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FakeProvider {
    fn new(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(ProviderState {
            requests: Vec::new(),
            script: script.into(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            thread::spawn(move || loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => serve_provider_stream(&mut stream, &state),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(err) => panic!("fake provider accept failed: {err}"),
                }
            })
        };
        Self {
            base_url: format!("http://{addr}/v1"),
            state,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.state.lock().unwrap().requests.clone()
    }

    fn wait_for_requests(&self, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let requests = self.requests();
            if requests.len() >= count {
                return requests;
            }
            assert!(
                Instant::now() < deadline,
                "provider saw {} requests, wanted {count}",
                requests.len()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Value {
    // Accepted sockets inherit the listener's nonblocking flag on Windows.
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut reader = BufReader::new(stream);
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).unwrap();
        assert!(read > 0, "provider request ended before blank line");
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap();
            }
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap_or(Value::Null)
}

fn serve_provider_stream(stream: &mut TcpStream, state: &Arc<Mutex<ProviderState>>) {
    let request = read_request(stream);
    let reply = {
        let mut guard = state.lock().unwrap();
        guard.requests.push(request.clone());
        guard
            .script
            .pop_front()
            .unwrap_or_else(|| Reply::Text("e2e default reply".to_owned()))
    };
    let reply = match reply {
        Reply::EchoTool => {
            let output = request["messages"]
                .as_array()
                .and_then(|messages| {
                    messages
                        .iter()
                        .rev()
                        .find(|message| message["role"] == "tool")
                })
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default();
            Reply::Text(format!("tool returned: {output}"))
        }
        other => other,
    };

    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    match reply {
        Reply::Text(text) => {
            let parts = fragments(&text);
            for part in parts {
                write_chunk(
                    stream,
                    json!({
                        "id": "chatcmpl-e2e",
                        "object": "chat.completion.chunk",
                        "model": MODEL,
                        "choices": [{"index": 0, "delta": {"content": part}, "finish_reason": null}],
                    }),
                );
            }
            write_chunk(
                stream,
                json!({
                    "id": "chatcmpl-e2e",
                    "object": "chat.completion.chunk",
                    "model": MODEL,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17},
                }),
            );
        }
        Reply::Tool {
            call_id,
            name,
            args,
        } => {
            write_chunk(
                stream,
                json!({
                    "id": "chatcmpl-e2e",
                    "object": "chat.completion.chunk",
                    "model": MODEL,
                    "choices": [{
                        "index": 0,
                        "delta": {"tool_calls": [{
                            "index": 0,
                            "id": call_id,
                            "type": "function",
                            "function": {"name": name, "arguments": ""},
                        }]},
                        "finish_reason": null,
                    }],
                }),
            );
            let half = args.len() / 2;
            for part in [&args[..half], &args[half..]] {
                write_chunk(
                    stream,
                    json!({
                        "id": "chatcmpl-e2e",
                        "object": "chat.completion.chunk",
                        "model": MODEL,
                        "choices": [{
                            "index": 0,
                            "delta": {"tool_calls": [{
                                "index": 0,
                                "function": {"arguments": part},
                            }]},
                            "finish_reason": null,
                        }],
                    }),
                );
            }
            write_chunk(
                stream,
                json!({
                    "id": "chatcmpl-e2e",
                    "object": "chat.completion.chunk",
                    "model": MODEL,
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17},
                }),
            );
        }
        Reply::EchoTool => unreachable!("EchoTool was resolved before writing"),
    }
    stream.write_all(b"data: [DONE]\n\n").unwrap();
    stream.flush().unwrap();
}

fn write_chunk(stream: &mut TcpStream, value: Value) {
    write!(stream, "data: {value}\n\n").unwrap();
}

fn fragments(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let count = chars.len().min(3);
    let size = chars.len().div_ceil(count);
    chars
        .chunks(size)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

// ---------------------------------------------------------------------------
// CLI process helper
// ---------------------------------------------------------------------------

struct CliRun {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    home: PathBuf,
}

impl CliRun {
    fn stderr_line(&self, prefix: &str) -> &str {
        self.stderr
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("missing stderr line {prefix:?} in:\n{}", self.stderr))
    }

    fn session_id(&self) -> String {
        self.stderr_line("forge chat: session=")
            .trim_start_matches("forge chat: session=")
            .to_owned()
    }

    fn log_path(&self) -> PathBuf {
        PathBuf::from(
            self.stderr_line("forge chat: log=")
                .trim_start_matches("forge chat: log="),
        )
    }

    fn events(&self) -> Vec<Value> {
        read_log(&self.log_path())
    }
}

#[allow(clippy::too_many_arguments)]
fn run_chat(
    home: &Path,
    workspace: &Path,
    provider: &FakeProvider,
    flags: &[&str],
    prompt: &str,
) -> CliRun {
    let output = Command::new(forge_bin())
        .arg("chat")
        .arg("--model")
        .arg(MODEL)
        .args(flags)
        .arg(prompt)
        .current_dir(workspace)
        .env("FORGE_HOME", home)
        .env("FORGE_BASE_URL", &provider.base_url)
        .env("FORGE_API_KEY", API_KEY)
        .output()
        .unwrap();
    CliRun {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        home: home.to_path_buf(),
    }
}

fn read_log(path: &Path) -> Vec<Value> {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("cannot read log {}: {err}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("bad JSON line in {}: {err}\n{line}", path.display()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Event assertions
// ---------------------------------------------------------------------------

fn event_types(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| str_field(event, "type"))
        .collect()
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing string field {key:?} in {value}"))
}

fn payload(event: &Value) -> &Value {
    event
        .get("payload")
        .unwrap_or_else(|| panic!("event has no payload: {event}"))
}

fn one_event<'a>(events: &'a [Value], typ: &str) -> &'a Value {
    let found: Vec<&Value> = events
        .iter()
        .filter(|event| str_field(event, "type") == typ)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one {typ} event, found {} in {:?}",
        found.len(),
        event_types(events)
    );
    found[0]
}

fn of_type<'a>(events: &'a [Value], typ: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| str_field(event, "type") == typ)
        .collect()
}

fn assert_dense_log(events: &[Value]) {
    let session = str_field(&events[0], "session");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event["seq"].as_u64(),
            Some(index as u64 + 1),
            "seq is not dense at index {index}"
        );
        assert_eq!(event["v"].as_u64(), Some(1), "wrong event version");
        assert_eq!(str_field(event, "session"), session, "session drifted");
        let id = str_field(event, "id");
        assert_eq!(id.len(), 32, "event id is not 32 hex chars: {id}");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "event id is not hex: {id}"
        );
        assert!(
            event["ts"].as_i64().unwrap_or(0) > 0,
            "event has no timestamp"
        );
    }
}

fn assert_plain_turn(events: &[Value], prompt: &str, answer: &str, model: &str) {
    assert_eq!(
        event_types(events),
        vec![
            "session/start",
            "turn/start",
            "user/message",
            "step/start",
            "llm/call",
            "assistant/message",
            "step/end",
            "turn/end",
        ]
    );
    assert_dense_log(events);
    let turn_start = one_event(events, "turn/start");
    let user = one_event(events, "user/message");
    let step_start = one_event(events, "step/start");
    let llm = one_event(events, "llm/call");
    let assistant = one_event(events, "assistant/message");
    let step_end = one_event(events, "step/end");
    let turn_end = one_event(events, "turn/end");
    assert_eq!(turn_start["turn"].as_u64(), Some(1));
    assert_eq!(user["turn"].as_u64(), Some(1));
    assert_eq!(
        user["seq"].as_u64(),
        Some(turn_start["seq"].as_u64().unwrap() + 1)
    );
    assert_eq!(str_field(payload(user), "text"), prompt);
    assert_eq!(payload(step_start)["index"].as_u64(), Some(0));
    assert_eq!(str_field(payload(step_start), "model"), model);
    assert_eq!(str_field(payload(llm), "model"), model);
    assert!(payload(llm)["messages"].as_u64().unwrap() >= 1);
    assert_eq!(str_field(payload(assistant), "text"), answer);
    assert_eq!(str_field(payload(assistant), "stopReason"), "end_turn");
    assert_eq!(assistant["step"].as_u64(), Some(1));
    assert_eq!(str_field(payload(step_end), "reason"), "end_turn");
    assert_eq!(str_field(payload(turn_end), "reason"), "completed");
    assert_eq!(payload(turn_end)["steps"].as_u64(), Some(1));
}

fn tool_stream(call_id: &str) -> Reply {
    Reply::Tool {
        call_id: call_id.to_owned(),
        name: "shell".to_owned(),
        args: json!({"command": format!("echo {TOOL_MARKER}")}).to_string(),
    }
}

fn tool_reply_script(call_id: &str) -> Vec<Reply> {
    vec![tool_stream(call_id), Reply::EchoTool]
}

fn assert_no_approval_events(events: &[Value]) {
    assert!(
        of_type(events, "approval/request").is_empty(),
        "approval/request must not be written by --approve=never"
    );
    assert!(
        of_type(events, "approval/decision").is_empty(),
        "approval/decision must not be written by --approve=never"
    );
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client (loopback, one request per connection)
// ---------------------------------------------------------------------------

struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|err| panic!("bad response JSON: {err}\n{}", self.body))
    }

    fn error_code(&self) -> String {
        str_field(&self.json()["error"], "code").to_owned()
    }
}

fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(TIMEOUT)).unwrap();
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw);
    let (head, body_text) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no header boundary in response:\n{text}"));
    let mut lines = head.lines();
    let status_line = lines.next().unwrap();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut response_headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            response_headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    HttpResponse {
        status,
        headers: response_headers,
        body: body_text.to_owned(),
    }
}

struct WebServer {
    child: Child,
}

impl Drop for WebServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct WebInfo {
    // Kept alive for its Drop impl, which terminates the server.
    _server: WebServer,
    port: u16,
    token: String,
    session_id: String,
}

fn start_web(home: &Path, workspace: &Path, provider: &FakeProvider) -> WebInfo {
    let mut child = Command::new(forge_bin())
        .arg("web")
        .arg("--model")
        .arg(MODEL)
        .arg("--no-browser")
        .arg("--addr")
        .arg("127.0.0.1:0")
        .current_dir(workspace)
        .env("FORGE_HOME", home)
        .env("FORGE_BASE_URL", &provider.base_url)
        .env("FORGE_API_KEY", API_KEY)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if sender.send(line.unwrap_or_default()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + TIMEOUT;
    let open_line = loop {
        let line = receiver
            .recv_timeout(Duration::from_millis(500))
            .unwrap_or_default();
        if line.contains("forge web: open ") {
            break line;
        }
        assert!(
            Instant::now() < deadline,
            "web did not print its open URL; last stderr line: {line}"
        );
    };
    let open = open_line.trim_start_matches("forge web: open ");
    let (base, tail) = open
        .split_once("/?token=")
        .unwrap_or_else(|| panic!("bad open URL: {open}"));
    let (token, session_id) = tail
        .split_once("#/s/")
        .unwrap_or_else(|| panic!("bad open URL: {open}"));
    let port = base.rsplit(':').next().unwrap().parse().unwrap();
    WebInfo {
        _server: WebServer { child },
        port,
        token: token.to_owned(),
        session_id: session_id.to_owned(),
    }
}

fn auth_headers(token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Bearer {token}")),
        ("X-Forge-Protocol", "1".to_owned()),
    ]
}

fn as_str_pairs<'a>(headers: &'a [(&'static str, String)]) -> Vec<(&'a str, &'a str)> {
    headers.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

fn poll_events(port: u16, token: &str, session: &str, want_type: &str) -> Value {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let headers = auth_headers(token);
        let response = http(
            port,
            "GET",
            &format!("/api/sessions/{session}/events?after=0&limit=1000"),
            &as_str_pairs(&headers),
            None,
        );
        assert_eq!(response.status, 200, "events: {}", response.body);
        let body = response.json();
        let events = body["events"].as_array().unwrap();
        if events.iter().any(|event| event["type"] == want_type) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "no {want_type} event within {TIMEOUT:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// Acceptance tests
// ---------------------------------------------------------------------------

#[test]
fn cli_plain_turn_streams_text_and_writes_dense_event_log() {
    let tmp = TempDir::new("plain");
    let home = tmp.path().join("home");
    let workspace = make_workspace(tmp.path());
    fs::create_dir_all(&home).unwrap();
    let provider = FakeProvider::new(vec![Reply::Text(ANSWER.to_owned())]);

    let run = run_chat(&home, &workspace, &provider, &[], "say something");
    assert!(run.status.success(), "forge chat failed:\n{}", run.stderr);
    assert_eq!(
        run.stdout,
        format!("{ANSWER}\n"),
        "stdout must carry only the assistant text"
    );
    assert!(
        run.stderr.contains("forge chat: model=fake sandbox="),
        "missing model diagnostic:\n{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("approve=never"),
        "wrong approval diagnostic:\n{}",
        run.stderr
    );
    assert!(
        run.stderr
            .lines()
            .any(|line| line.contains("usage in=12 out=5 total=17")),
        "missing usage diagnostics:\n{}",
        run.stderr
    );
    assert!(run.log_path().is_file(), "log file not written");

    let events = run.events();
    assert_plain_turn(&events, "say something", ANSWER, MODEL);
    assert_eq!(
        payload(one_event(&events, "assistant/message"))["usage"]["inputTokens"].as_u64(),
        Some(12)
    );
    assert_eq!(
        payload(one_event(&events, "assistant/message"))["usage"]["outputTokens"].as_u64(),
        Some(5)
    );

    let requests = provider.wait_for_requests(1);
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(str_field(request, "model"), MODEL);
    assert_eq!(request["stream"].as_bool(), Some(true));
    let messages = request["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(str_field(&messages[0], "role"), "user");
    assert_eq!(str_field(&messages[0], "content"), "say something");
}

#[test]
fn cli_tool_call_with_always_approval_runs_shell_and_final_answer_uses_output() {
    let tmp = TempDir::new("tool-always");
    let home = tmp.path().join("home");
    let workspace = make_workspace(tmp.path());
    fs::create_dir_all(&home).unwrap();
    let call_id = "call_e2e_always";
    let provider = FakeProvider::new(tool_reply_script(call_id));

    let run = run_chat(
        &home,
        &workspace,
        &provider,
        &["--approve=always"],
        "run a command",
    );
    assert!(run.status.success(), "forge chat failed:\n{}", run.stderr);
    assert!(
        run.stdout.contains(TOOL_MARKER),
        "final answer lacks tool output: {}",
        run.stdout
    );

    let events = run.events();
    assert_dense_log(&events);
    assert_eq!(one_event(&events, "session/start")["seq"].as_u64(), Some(1));
    assert_eq!(of_type(&events, "turn/start").len(), 1);
    assert_eq!(of_type(&events, "user/message").len(), 1);
    assert_eq!(of_type(&events, "step/start").len(), 2);
    assert_eq!(of_type(&events, "llm/call").len(), 2);
    assert_eq!(of_type(&events, "assistant/message").len(), 2);
    assert_eq!(of_type(&events, "step/end").len(), 2);

    let tool_call = one_event(&events, "tool/call");
    let request = one_event(&events, "approval/request");
    let decision = one_event(&events, "approval/decision");
    let result = one_event(&events, "tool/result");
    assert_eq!(str_field(payload(tool_call), "tool"), "shell");
    assert_eq!(str_field(payload(tool_call), "callId"), call_id);
    let approval_id = str_field(payload(request), "approvalId").to_owned();
    assert!(!approval_id.is_empty());
    assert_eq!(str_field(payload(request), "callId"), call_id);
    assert_eq!(str_field(payload(request), "tool"), "shell");
    assert_eq!(str_field(payload(decision), "approvalId"), approval_id);
    assert_eq!(str_field(payload(decision), "callId"), call_id);
    assert_eq!(str_field(payload(decision), "decision"), "allow");
    assert_eq!(
        str_field(payload(decision), "decidedBy"),
        "flag:--approve=always"
    );
    assert_eq!(str_field(payload(result), "callId"), call_id);
    assert_eq!(str_field(payload(result), "tool"), "shell");
    assert_eq!(payload(result)["ok"].as_bool(), Some(true));
    assert!(str_field(payload(result), "output").contains(TOOL_MARKER));
    assert!(request["seq"].as_u64().unwrap() < decision["seq"].as_u64().unwrap());
    assert!(decision["seq"].as_u64().unwrap() < result["seq"].as_u64().unwrap());
    assert!(
        result["seq"].as_u64().unwrap() < one_event(&events, "turn/end")["seq"].as_u64().unwrap()
    );
    let last_answer = of_type(&events, "assistant/message")
        .last()
        .map(|event| str_field(payload(event), "text").to_owned())
        .unwrap();
    assert!(last_answer.contains(TOOL_MARKER));

    let requests = provider.wait_for_requests(2);
    assert_eq!(requests.len(), 2);
    let second = &requests[1];
    let messages = second["messages"].as_array().unwrap();
    let tool_message = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap_or_else(|| panic!("no tool message in second provider request: {second}"));
    assert_eq!(str_field(tool_message, "tool_call_id"), call_id);
    assert!(str_field(tool_message, "content").contains(TOOL_MARKER));
}

#[test]
fn cli_tool_call_with_never_approval_has_no_approval_events_and_fails_tool() {
    let tmp = TempDir::new("tool-never");
    let home = tmp.path().join("home");
    let workspace = make_workspace(tmp.path());
    fs::create_dir_all(&home).unwrap();
    let call_id = "call_e2e_never";
    let provider = FakeProvider::new(tool_reply_script(call_id));

    let run = run_chat(
        &home,
        &workspace,
        &provider,
        &["--approve=never"],
        "run a command",
    );
    assert!(run.status.success(), "forge chat failed:\n{}", run.stderr);

    let events = run.events();
    assert_dense_log(&events);
    assert_no_approval_events(&events);
    let result = one_event(&events, "tool/result");
    assert_eq!(payload(result)["ok"].as_bool(), Some(false));
    assert_eq!(str_field(payload(result), "code"), "no_approver");
    assert_eq!(str_field(payload(result), "callId"), call_id);
    assert_eq!(
        one_event(&events, "turn/end")["seq"].as_u64(),
        Some(events.len() as u64)
    );
    assert!(
        run.stderr
            .contains("a tool call was denied because --approve=never"),
        "missing no-approver hint:\n{}",
        run.stderr
    );

    let requests = provider.wait_for_requests(2);
    let second = &requests[1];
    let tool_message = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    assert_eq!(
        str_field(tool_message, "content"),
        "approval required but no approver is available"
    );
}

#[test]
fn cli_two_turn_resume_replays_history_and_uses_the_remembered_fact() {
    let tmp = TempDir::new("resume");
    let home = tmp.path().join("home");
    let workspace = make_workspace(tmp.path());
    fs::create_dir_all(&home).unwrap();
    let provider = FakeProvider::new(vec![
        Reply::Text(format!("I will remember {FACT}")),
        Reply::Text(format!("your secret is {FACT}")),
    ]);

    let first = run_chat(
        &home,
        &workspace,
        &provider,
        &[],
        &format!("Remember this fact: my secret code is {FACT}."),
    );
    assert!(
        first.status.success(),
        "first turn failed:\n{}",
        first.stderr
    );
    let session = first.session_id();
    let second = run_chat(
        &home,
        &workspace,
        &provider,
        &["--resume", &session],
        "What is my secret code?",
    );
    assert!(
        second.status.success(),
        "resumed turn failed:\n{}",
        second.stderr
    );
    assert_eq!(second.session_id(), session);
    assert!(
        second.stdout.contains(FACT),
        "second answer forgot the fact: {}",
        second.stdout
    );

    let events = second.events();
    assert_dense_log(&events);
    let calls = of_type(&events, "llm/call");
    assert_eq!(
        calls.len(),
        2,
        "resume should add exactly one further llm/call"
    );
    let first_count = payload(calls[0])["messages"].as_u64().unwrap();
    let second_count = payload(calls[1])["messages"].as_u64().unwrap();
    assert_eq!(
        first_count, 1,
        "first turn should send only the user message"
    );
    assert_eq!(
        second_count, 3,
        "resumed turn should send user, assistant and new user messages"
    );
    let second_turns: Vec<u64> = of_type(&events, "turn/start")
        .iter()
        .map(|event| event["turn"].as_u64().unwrap())
        .collect();
    assert_eq!(second_turns, vec![1, 2]);

    let requests = provider.wait_for_requests(2);
    let replayed = requests[1]["messages"].as_array().unwrap();
    assert_eq!(replayed.len(), 3);
    assert_eq!(str_field(&replayed[0], "role"), "user");
    assert!(str_field(&replayed[0], "content").contains(FACT));
    assert_eq!(str_field(&replayed[1], "role"), "assistant");
    assert_eq!(str_field(&replayed[2], "role"), "user");
    assert_eq!(
        str_field(&replayed[2], "content"),
        "What is my secret code?"
    );
    let _ = &first.home; // keep the first run alive for its log-path diagnostic
}

#[test]
fn http_surface_matches_oracle_protocol_and_event_contract() {
    let tmp = TempDir::new("http");
    let home = tmp.path().join("home");
    let workspace = make_workspace(tmp.path());
    fs::create_dir_all(&home).unwrap();
    let provider = FakeProvider::new(vec![Reply::Text(HTTP_ANSWER.to_owned())]);
    let web = start_web(&home, &workspace, &provider);
    let port = web.port;
    let token = &web.token;

    let health = http(port, "GET", "/healthz", &[], None);
    assert_eq!(health.status, 200, "healthz: {}", health.body);
    assert_eq!(health.header("x-forge-protocol"), Some("1"));
    let health_json = health.json();
    assert_eq!(str_field(&health_json, "status"), "ok");
    assert_eq!(health_json["protocol"].as_u64(), Some(1));
    assert_eq!(str_field(&health_json, "version"), "0.0.0-dev");

    let list_headers = auth_headers(token);
    let list = http(
        port,
        "GET",
        "/api/sessions",
        &as_str_pairs(&list_headers),
        None,
    );
    assert_eq!(list.status, 200, "sessions: {}", list.body);
    assert_eq!(list.header("x-forge-protocol"), Some("1"));
    assert!(list.json()["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["id"] == web.session_id));

    let create_body =
        json!({"workspace": workspace.to_string_lossy(), "model": MODEL, "profile": "default"})
            .to_string();
    let create_headers = {
        let mut headers = auth_headers(token);
        headers.push(("Content-Type", "application/json".to_owned()));
        headers.push(("Origin", format!("http://127.0.0.1:{port}")));
        headers
    };
    let created = http(
        port,
        "POST",
        "/api/sessions",
        &as_str_pairs(&create_headers),
        Some(&create_body),
    );
    assert_eq!(created.status, 201, "create session: {}", created.body);
    assert_eq!(created.header("x-forge-protocol"), Some("1"));
    let created_json = created.json();
    assert_eq!(created_json["lastSeq"].as_u64(), Some(1));
    let session = str_field(&created_json["session"], "id").to_owned();

    let message_body = r#"{"text":"hello over http"}"#;
    let message_headers = {
        let mut headers = auth_headers(token);
        headers.push(("Content-Type", "application/json".to_owned()));
        headers.push(("Origin", format!("http://127.0.0.1:{port}")));
        headers.push(("Idempotency-Key", "e2e-idempotency-1".to_owned()));
        headers
    };
    let posted = http(
        port,
        "POST",
        &format!("/api/sessions/{session}/messages"),
        &as_str_pairs(&message_headers),
        Some(message_body),
    );
    assert_eq!(posted.status, 202, "post message: {}", posted.body);
    let posted_json = posted.json();
    assert_eq!(posted_json["accepted"].as_bool(), Some(true));
    assert_eq!(posted_json["deduplicated"].as_bool(), Some(false));
    let accepted_seq = posted_json["seq"].as_u64().unwrap();
    assert!(accepted_seq >= 2);

    let page = poll_events(port, token, &session, "turn/end");
    let events = page["events"].as_array().unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "session/start",
            "turn/start",
            "user/message",
            "step/start",
            "llm/call",
            "assistant/message",
            "step/end",
            "turn/end",
        ]
    );
    let turn_start_seq = events[1]["seq"].as_u64().unwrap();
    let user_seq = events[2]["seq"].as_u64().unwrap();
    assert!(
        turn_start_seq < user_seq,
        "turn/start must precede user/message"
    );
    assert_eq!(user_seq, accepted_seq);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event["seq"].as_u64(),
            Some(index as u64 + 1),
            "events page seq is not dense"
        );
    }
    assert_eq!(page["lastSeq"].as_u64(), Some(events.len() as u64));
    assert_eq!(str_field(&events[5]["payload"], "text"), HTTP_ANSWER);
    assert_eq!(str_field(&events[2]["payload"], "text"), "hello over http");

    let replay = http(
        port,
        "POST",
        &format!("/api/sessions/{session}/messages"),
        &as_str_pairs(&message_headers),
        Some(message_body),
    );
    assert_eq!(replay.status, 200, "replay: {}", replay.body);
    let replay_json = replay.json();
    assert_eq!(
        replay_json["deduplicated"].as_bool(),
        Some(true),
        "replay was not deduplicated"
    );
    assert_eq!(replay_json["seq"].as_u64(), Some(accepted_seq));
    assert_eq!(replay_json["accepted"].as_bool(), Some(true));

    // Missing protocol header must be rejected before anything else on /api/*.
    let no_protocol = [("Authorization", format!("Bearer {token}"))];
    let protocol_error = http(
        port,
        "GET",
        "/api/sessions",
        &as_str_pairs(&no_protocol),
        None,
    );
    assert_eq!(
        protocol_error.status, 400,
        "missing protocol header: {}",
        protocol_error.body
    );
    assert_eq!(protocol_error.header("x-forge-protocol"), Some("1"));
    assert_eq!(protocol_error.error_code(), "protocol_required");

    // A wrong protocol version is a separate 400 contract error.
    let unsupported = [
        ("Authorization", format!("Bearer {token}")),
        ("X-Forge-Protocol", "2".to_owned()),
    ];
    let unsupported_error = http(
        port,
        "GET",
        "/api/sessions",
        &as_str_pairs(&unsupported),
        None,
    );
    assert_eq!(
        unsupported_error.status, 400,
        "unsupported protocol version: {}",
        unsupported_error.body
    );
    assert_eq!(unsupported_error.error_code(), "protocol_unsupported");

    // A bad token is rejected before protocol checking.
    let bad_token = [
        ("Authorization", "Bearer wrong-token".to_owned()),
        ("X-Forge-Protocol", "1".to_owned()),
    ];
    let unauthorized = http(
        port,
        "GET",
        "/api/sessions",
        &as_str_pairs(&bad_token),
        None,
    );
    assert_eq!(unauthorized.status, 401, "bad token: {}", unauthorized.body);
    assert_eq!(unauthorized.header("x-forge-protocol"), Some("1"));
    assert_eq!(unauthorized.error_code(), "unauthorized");

    provider.wait_for_requests(1);
    assert_eq!(
        provider.requests().len(),
        1,
        "one message must cause exactly one provider call"
    );
}
