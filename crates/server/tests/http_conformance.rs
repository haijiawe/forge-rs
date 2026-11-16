//! Language-neutral HTTP conformance cases, driven through a real loopback
//! `Server`. The case files live in `tests/conformance/cases/http` and are the
//! shared oracle between the Go and Rust implementations; this file never
//! edits them. A scripted `TurnRunner` replaces the model so no network or
//! credentials are needed.

use forge_core::{Event, EventType};
use forge_server::api::{CancelToken, Options, TurnContext, TurnRunner};
use forge_server::Server;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Case file schema. `deny_unknown_fields` mirrors the Go runner's
// DisallowUnknownFields: a case with a typo must fail, not pass by being
// silently ignored.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseFile {
    name: String,
    description: String,
    #[serde(default)]
    setup: String,
    request: CaseRequest,
    expect: CaseExpect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseRequest {
    method: String,
    path: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct CaseExpect {
    status: u16,
    headers: HashMap<String, String>,
    json_contains: Option<Value>,
    json_exact: Option<Value>,
    json_absent: Vec<String>,
    not_empty: Vec<String>,
    at_least: HashMap<String, f64>,
    not_equal: HashMap<String, String>,
    max_elapsed_ms: u64,
    verbatim_log: bool,
}

fn load_cases() -> Vec<(String, CaseFile)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/cases/http");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no conformance cases in {}",
        dir.display()
    );
    paths
        .into_iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .and_then(|x| x.to_str())
                .expect("case file stem")
                .to_string();
            let text = fs::read_to_string(&path).expect("read case file");
            let case: CaseFile = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("invalid case {}: {e}", path.display()));
            (stem, case)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client. The server replies with Content-Length and closes
// the connection, so a single read_to_end is sufficient.
// ---------------------------------------------------------------------------

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn parse_response(buf: &[u8]) -> Result<HttpResponse, String> {
    let split = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "response missing header terminator".to_string())?;
    let head = std::str::from_utf8(&buf[..split]).map_err(|e| e.to_string())?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "response missing status code".to_string())?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    Ok(HttpResponse {
        status,
        headers,
        body: buf[split + 4..].to_vec(),
    })
}

fn http_request(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<HttpResponse, String> {
    let mut stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\n");
    for (key, value) in headers {
        request.push_str(key);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("Connection: close\r\n");
    if !body.is_empty() {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    parse_response(&response)
}

// ---------------------------------------------------------------------------
// JSON assertion helpers, mirroring the Go harness exactly. Numbers compare
// by value so an integer written as 1.0 still equals 1.
// ---------------------------------------------------------------------------

fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| json_equal(value, other)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| json_equal(x, y))
        }
        _ => left == right,
    }
}

fn subset_match(want: &Value, got: &Value) -> bool {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => w
            .iter()
            .all(|(key, value)| g.get(key).is_some_and(|other| subset_match(value, other))),
        (Value::Array(w), Value::Array(g)) => {
            w.len() == g.len() && w.iter().zip(g).all(|(x, y)| subset_match(x, y))
        }
        _ => json_equal(want, got),
    }
}

fn json_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in path.split('.') {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn is_empty_json(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn substitute_text(text: &str, vars: &HashMap<String, Value>) -> String {
    if let Some(key) = text.strip_prefix('$') {
        if !key.contains('$') {
            if let Some(value) = vars.get(key) {
                return value_to_text(value);
            }
        }
    }
    let mut result = text.to_string();
    for (key, value) in vars {
        result = result.replace(&format!("${key}"), &value_to_text(value));
    }
    result
}

fn substitute_any(value: Value, vars: &HashMap<String, Value>) -> Value {
    match value {
        Value::String(text) => {
            // The Go oracle returns the raw typed value for a whole "$KEY"
            // token so a numeric substitution stays numeric; only embedded
            // tokens are flattened to text.
            if let Some(key) = text.strip_prefix('$') {
                if !key.contains('$') {
                    if let Some(raw) = vars.get(key) {
                        return raw.clone();
                    }
                }
            }
            Value::String(substitute_text(&text, vars))
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, substitute_any(item, vars)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| substitute_any(item, vars))
                .collect(),
        ),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Scripted runner. The case setups need three behaviours: finish the turn,
// hold it open (status "running"), and raise an approval. A runner may append
// facts only through the Server's EventWriter, the same way the real runner
// does. The release flag lets teardown unblock a held turn.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Complete,
    Hold,
    Approval,
}

#[derive(Clone)]
struct RunnerControl {
    script: Script,
    release: Arc<AtomicBool>,
}

struct FakeRunner {
    server: Arc<Mutex<Option<Server>>>,
    controls: Arc<Mutex<HashMap<String, RunnerControl>>>,
}

impl FakeRunner {
    fn append(&self, server: &Server, session: &str, typ: EventType, turn: u32, payload: Value) {
        let mut event = Event::new(session, typ, Some(payload));
        event.turn = turn;
        if let Err(error) = server.append_event(session, &event) {
            eprintln!("fake runner could not append {typ}: {error}");
        }
    }

    fn hold(&self, release: &AtomicBool, cancel: &CancelToken) {
        while !release.load(Ordering::Relaxed) && !cancel.is_cancelled() {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl TurnRunner for FakeRunner {
    fn run_turn(
        &self,
        context: &TurnContext,
        session_id: &str,
        _text: &str,
    ) -> forge_core::Result<()> {
        let control = self
            .controls
            .lock()
            .expect("runner control lock")
            .get(session_id)
            .cloned();
        let server = self.server.lock().expect("server ref lock").clone();
        let Some(server) = server else {
            return Ok(());
        };
        let script = control
            .as_ref()
            .map_or(Script::Complete, |item| item.script);
        match script {
            Script::Complete => {
                self.append(
                    &server,
                    session_id,
                    EventType::ASSISTANT_MESSAGE,
                    1,
                    json!({ "text": "conformance reply" }),
                );
                self.append(
                    &server,
                    session_id,
                    EventType::TURN_END,
                    1,
                    json!({ "reason": "completed", "steps": 1 }),
                );
            }
            Script::Hold => {
                if let Some(control) = control {
                    self.hold(&control.release, &context.cancel);
                }
            }
            Script::Approval => {
                // No timeoutMs: an expiring approval is a separate behaviour
                // and would race the decisions these cases make by hand.
                self.append(
                    &server,
                    session_id,
                    EventType::APPROVAL_REQUEST,
                    1,
                    json!({
                        "approvalId": "apr_conf",
                        "callId": "call_conf",
                        "tool": "shell",
                        "args": { "command": "echo conformance", "description": "conformance probe" },
                        "reason": "probe finished"
                    }),
                );
                if let Some(control) = control {
                    self.hold(&control.release, &context.cancel);
                }
            }
        }
        Ok(())
    }

    fn cancel(&self) -> forge_core::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness: one real server on 127.0.0.1:0, one fake runner factory. Temp dirs
// use pid + counter because the crate has no tempfile dependency.
// ---------------------------------------------------------------------------

struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct World {
    addr: String,
    token: String,
    server: Server,
    data_dir: PathBuf,
    base: PathBuf,
    vars: HashMap<String, Value>,
    controls: Arc<Mutex<HashMap<String, RunnerControl>>>,
    releases: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    serve_thread: Option<JoinHandle<forge_core::Result<()>>>,
    _temp: TempGuard,
}

impl World {
    fn new(label: &str) -> Self {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let index = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "forge-http-conf-{}-{index}-{label}",
            std::process::id()
        ));
        let data_dir = base.join("data");
        fs::create_dir_all(base.join("ws")).expect("create temp workspace root");
        let _temp = TempGuard(base.clone());

        let server_ref: Arc<Mutex<Option<Server>>> = Arc::new(Mutex::new(None));
        let controls: Arc<Mutex<HashMap<String, RunnerControl>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let factory_server = Arc::clone(&server_ref);
        let factory_controls = Arc::clone(&controls);
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: data_dir.clone(),
            version: "conformance".to_string(),
            poll_interval: Duration::from_millis(2),
            delta_interval: Duration::from_millis(2),
            ping_interval: Duration::from_secs(30),
            new_runner: Some(Arc::new(move |_session_id: &str| {
                Arc::new(FakeRunner {
                    server: Arc::clone(&factory_server),
                    controls: Arc::clone(&factory_controls),
                })
            })),
            ..Options::default()
        };

        let server = Server::new(options).expect("start conformance server");
        *server_ref.lock().expect("server ref lock") = Some(server.clone());
        let worker = server.clone();
        let serve_thread = thread::spawn(move || worker.listen_and_serve());

        let deadline = Instant::now() + Duration::from_secs(20);
        let addr = loop {
            if let Some(addr) = server.addr() {
                break addr;
            }
            assert!(Instant::now() < deadline, "server did not bind in time");
            thread::sleep(Duration::from_millis(2));
        };
        let token = server.token().to_string();
        Self {
            addr,
            token,
            server,
            data_dir,
            base,
            vars: HashMap::new(),
            controls,
            releases: Arc::new(Mutex::new(Vec::new())),
            serve_thread: Some(serve_thread),
            _temp,
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        overrides: &[(String, String)],
        body: Option<&[u8]>,
    ) -> (HttpResponse, Duration) {
        let mut headers = vec![
            ("X-Forge-Protocol".to_string(), "1".to_string()),
            ("Origin".to_string(), format!("http://{}", self.addr)),
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.token),
            ),
        ];
        if body.is_some_and(|bytes| !bytes.is_empty()) {
            headers.push((
                "Content-Type".to_string(),
                "application/json; charset=utf-8".to_string(),
            ));
        }
        for (key, value) in overrides {
            headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(key));
            if !value.is_empty() {
                headers.push((key.clone(), value.clone()));
            }
        }
        let started = Instant::now();
        let response = http_request(&self.addr, method, path, &headers, body.unwrap_or(&[]))
            .unwrap_or_else(|error| panic!("{method} {path} failed: {error}"));
        (response, started.elapsed())
    }

    fn create_session(&self, workspace: &str) -> String {
        let body =
            serde_json::to_vec(&json!({ "workspace": workspace, "model": "conformance-model" }))
                .expect("encode create session");
        let (response, _) = self.request("POST", "/api/sessions", &[], Some(&body));
        assert_eq!(
            response.status,
            201,
            "create session failed: {}",
            String::from_utf8_lossy(&response.body)
        );
        let value: Value = serde_json::from_slice(&response.body).expect("decode create session");
        value["session"]["id"]
            .as_str()
            .expect("session id in response")
            .to_string()
    }

    fn append_event(&self, session: &str, typ: EventType, turn: u32, payload: Value) {
        let mut event = Event::new(session, typ, Some(payload));
        event.turn = turn;
        self.server
            .append_event(session, &event)
            .unwrap_or_else(|error| panic!("seed {typ}: {error}"));
    }

    fn post_message(&self, session: &str, key: &str, text: &str) -> HttpResponse {
        let body = serde_json::to_vec(&json!({ "text": text })).expect("encode message");
        let overrides = vec![("Idempotency-Key".to_string(), key.to_string())];
        let path = format!("/api/sessions/{session}/messages");
        let (response, _) = self.request("POST", &path, &overrides, Some(&body));
        response
    }

    fn wait_detail(&self, session: &str, mut predicate: impl FnMut(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        let path = format!("/api/sessions/{session}");
        loop {
            let (response, _) = self.request("GET", &path, &[], None);
            if response.status == 200 {
                if let Ok(value) = serde_json::from_slice::<Value>(&response.body) {
                    if predicate(&value) {
                        return value;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for session detail"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn hold_for(&self, session: &str, script: Script) {
        let release = Arc::new(AtomicBool::new(false));
        self.controls.lock().expect("controls lock").insert(
            session.to_string(),
            RunnerControl {
                script,
                release: Arc::clone(&release),
            },
        );
        self.releases
            .lock()
            .expect("releases lock")
            .push(Arc::clone(&release));
    }

    fn setup(&mut self, setup: &str, case_name: &str) {
        let workspace = self.base.join("ws").join(case_name);
        let workspace_text = workspace.to_string_lossy().to_string();
        self.vars.insert(
            "WORKSPACE".to_string(),
            Value::String(workspace_text.clone()),
        );
        match setup {
            "" | "none" => {}
            "session" => {
                let session = self.create_session(&workspace_text);
                self.vars.insert("SESSION".to_string(), json!(session));
            }
            "sessions3" => {
                for index in 0..3 {
                    let path = workspace.join(format!("w{index}"));
                    self.create_session(&path.to_string_lossy());
                }
                let (response, _) = self.request("GET", "/api/sessions?limit=1", &[], None);
                assert_eq!(response.status, 200, "sessions3 list failed");
                let value: Value = serde_json::from_slice(&response.body).expect("decode list");
                self.vars
                    .insert("CURSOR".to_string(), value["nextCursor"].clone());
                self.vars.insert(
                    "FIRST_SESSION".to_string(),
                    value["sessions"][0]["id"].clone(),
                );
            }
            "events" => {
                let session = self.create_session(&workspace_text);
                self.vars.insert("SESSION".to_string(), json!(session));
                self.append_event(&session, EventType::TURN_START, 1, json!({ "number": 1 }));
                self.append_event(
                    &session,
                    EventType::USER_MESSAGE,
                    1,
                    json!({ "text": "seed message", "idempotencyKey": "seed-1" }),
                );
                self.append_event(
                    &session,
                    EventType::TURN_END,
                    1,
                    json!({ "reason": "completed", "steps": 0 }),
                );
            }
            "message_sent" => {
                let session = self.create_session(&workspace_text);
                self.vars.insert("SESSION".to_string(), json!(session));
                let key = format!("conf-{case_name}-key");
                let response = self.post_message(&session, &key, "seed message");
                assert_eq!(response.status, 202, "message_sent failed");
                let value: Value = serde_json::from_slice(&response.body).expect("decode message");
                self.vars
                    .insert("MESSAGE_SEQ".to_string(), value["seq"].clone());
            }
            "running" => {
                let session = self.create_session(&workspace_text);
                self.vars.insert("SESSION".to_string(), json!(session));
                self.hold_for(&session, Script::Hold);
                let key = format!("conf-{case_name}-key-running");
                let response = self.post_message(&session, &key, "hold this turn open");
                assert_eq!(response.status, 202, "running setup message failed");
                self.wait_detail(&session, |detail| detail["status"] == "running");
            }
            "pending_approval" => {
                let path = workspace.join("pending");
                let workspace_text = path.to_string_lossy().to_string();
                let session = self.create_session(&workspace_text);
                self.vars
                    .insert("PENDING_WORKSPACE".to_string(), json!(workspace_text));
                self.vars.insert("SESSION".to_string(), json!(session));
                self.hold_for(&session, Script::Approval);
                let key = format!("conf-{case_name}-key-pending");
                let response = self.post_message(&session, &key, "please run a shell command");
                assert_eq!(response.status, 202, "approval setup message failed");
                let detail =
                    self.wait_detail(&session, |value| value.get("pendingApproval").is_some());
                self.vars.insert(
                    "APPROVAL".to_string(),
                    detail["pendingApproval"]["approvalId"].clone(),
                );
            }
            "answered_approval" => {
                self.setup("pending_approval", case_name);
                let session = self.vars["SESSION"]
                    .as_str()
                    .expect("session var")
                    .to_string();
                let approval = self.vars["APPROVAL"]
                    .as_str()
                    .expect("approval var")
                    .to_string();
                let body =
                    serde_json::to_vec(&json!({ "decision": "allow" })).expect("encode decision");
                let path = format!("/api/sessions/{session}/approvals/{approval}");
                let (response, _) = self.request("POST", &path, &[], Some(&body));
                assert_eq!(response.status, 200, "first approval decision failed");
            }
            "ended" => {
                let session = self.create_session(&workspace_text);
                self.vars.insert("SESSION".to_string(), json!(session));
                self.append_event(
                    &session,
                    EventType::SESSION_END,
                    0,
                    json!({ "reason": "completed" }),
                );
            }
            "healthz_no_disk" => {
                // The dedicated harness removes its data directory before the
                // case runs; see run_case's caller.
            }
            other => panic!("unknown setup {other:?}"),
        }
    }

    fn check(&self, expect: &CaseExpect, response: &HttpResponse, elapsed: Duration) {
        assert_eq!(
            response.status,
            expect.status,
            "status mismatch; body={}",
            String::from_utf8_lossy(&response.body)
        );
        assert_eq!(
            response.header("X-Forge-Protocol"),
            Some("1"),
            "every response carries the negotiated protocol header"
        );
        for (key, raw) in &expect.headers {
            let want = substitute_text(raw, &self.vars);
            assert_eq!(
                response.header(key),
                Some(want.as_str()),
                "header {key} mismatch"
            );
        }
        if expect.max_elapsed_ms > 0 {
            assert!(
                elapsed.as_millis() <= u128::from(expect.max_elapsed_ms),
                "request took {:?}, limit {}ms",
                elapsed,
                expect.max_elapsed_ms
            );
        }
        let body: Value = if response.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&response.body).expect("response body must be JSON")
        };
        if let Some(want) = &expect.json_contains {
            let want = substitute_any(want.clone(), &self.vars);
            assert!(
                subset_match(&want, &body),
                "jsonContains {want} not found in {body}"
            );
        }
        if let Some(want) = &expect.json_exact {
            let want = substitute_any(want.clone(), &self.vars);
            assert!(json_equal(&want, &body), "jsonExact {want} != {body}");
        }
        for path in &expect.json_absent {
            assert!(
                json_path(&body, path).is_none(),
                "jsonAbsent path {path} unexpectedly present"
            );
        }
        for path in &expect.not_empty {
            let value = json_path(&body, path)
                .unwrap_or_else(|| panic!("notEmpty path {path} missing in {body}"));
            assert!(!is_empty_json(value), "notEmpty path {path} is empty");
        }
        for (path, minimum) in &expect.at_least {
            let value = json_path(&body, path)
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("atLeast path {path} missing in {body}"));
            assert!(value >= *minimum, "atLeast {path}: {value} < {minimum}");
        }
        for (path, raw) in &expect.not_equal {
            let got =
                json_path(&body, path).unwrap_or_else(|| panic!("notEqual path {path} missing"));
            let want = Value::String(substitute_text(raw, &self.vars));
            assert!(
                !json_equal(got, &want),
                "notEqual {path} should not be {want}"
            );
        }
        if expect.verbatim_log {
            let session = self.vars["SESSION"].as_str().expect("session var");
            let log_path = self.server.path(session);
            let text = fs::read_to_string(&log_path).expect("read verbatim session log");
            let mut by_seq: HashMap<u64, &str> = HashMap::new();
            for line in text.trim_end_matches('\n').split('\n') {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(line) {
                    if let Some(seq) = value.get("seq").and_then(Value::as_u64) {
                        by_seq.insert(seq, line);
                    }
                }
            }
            let events = body["events"].as_array().expect("verbatim events array");
            for event in events {
                let seq = event["seq"].as_u64().expect("event seq");
                let logged = by_seq
                    .get(&seq)
                    .unwrap_or_else(|| panic!("no log line for seq {seq}"));
                let returned = serde_json::to_string(event).expect("serialize returned event");
                assert_eq!(&returned, logged, "event {seq} differs from its log line");
            }
        }
    }

    fn run_case(&mut self, case: &CaseFile) {
        self.vars.insert(
            "CASE_KEY".to_string(),
            Value::String(format!("conf-{}-key", case.name)),
        );
        self.vars.insert(
            "OVERSIZED_DATA".to_string(),
            Value::String("x".repeat((1 << 20) + 1)),
        );
        self.setup(&case.setup, &case.name);

        let path = substitute_text(&case.request.path, &self.vars);
        let overrides: Vec<(String, String)> = case
            .request
            .headers
            .iter()
            .map(|(key, value)| (key.clone(), substitute_text(value, &self.vars)))
            .collect();
        let body = match &case.request.body {
            Some(value) if !value.is_null() => Some(
                serde_json::to_vec(&substitute_any(value.clone(), &self.vars))
                    .expect("encode case body"),
            ),
            _ => None,
        };
        let (response, elapsed) =
            self.request(&case.request.method, &path, &overrides, body.as_deref());
        self.check(&case.expect, &response, elapsed);
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for release in self.releases.lock().expect("releases lock").iter() {
            release.store(true, Ordering::Relaxed);
        }
        let _ = self.server.shutdown();
        if let Some(handle) = self.serve_thread.take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests. Well-formedness is checked independently so a malformed case file is
// reported as a schema failure, not as an obscure assertion later.
// ---------------------------------------------------------------------------

#[test]
fn case_files_are_well_formed() {
    const SETUPS: &[&str] = &[
        "",
        "none",
        "session",
        "sessions3",
        "events",
        "message_sent",
        "running",
        "pending_approval",
        "answered_approval",
        "ended",
        "healthz_no_disk",
    ];
    let cases = load_cases();
    assert!(
        cases.len() >= 20,
        "expected at least 20 cases, got {}",
        cases.len()
    );
    let mut names = HashSet::new();
    for (stem, case) in &cases {
        assert_eq!(stem, &case.name, "case name must match file stem");
        assert!(
            case.description.len() >= 10,
            "{}: description too short",
            case.name
        );
        assert!(
            SETUPS.contains(&case.setup.as_str()),
            "{}: unknown setup",
            case.name
        );
        assert!(
            !case.request.method.is_empty(),
            "{}: missing method",
            case.name
        );
        assert!(!case.request.path.is_empty(), "{}: missing path", case.name);
        assert!(
            case.expect.status > 0,
            "{}: missing expected status",
            case.name
        );
        assert!(
            names.insert(case.name.clone()),
            "duplicate case {}",
            case.name
        );
    }
}

#[test]
fn http_conformance_cases() {
    let cases = load_cases();
    let mut shared = World::new("shared");
    for (_, case) in &cases {
        if case.setup == "healthz_no_disk" {
            let mut dedicated = World::new("nodisk");
            fs::remove_dir_all(&dedicated.data_dir).expect("remove data dir for healthz case");
            dedicated.run_case(case);
        } else {
            shared.run_case(case);
        }
    }
}
