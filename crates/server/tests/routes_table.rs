//! Every row of `hearth_server::routes::ROUTES` must be served.
//!
//! The table in `crates/server/src/routes.rs` is a declaration: it is what
//! `GET /api/capabilities` prints when an agent asks "what can I call here". A
//! declaration that outlives its route is worse than no declaration — the agent
//! trusts it, calls the endpoint, and gets a 404 that reads like a bug in the
//! caller.
//!
//! So this test walks the table and asks the server. The assertion is narrow on
//! purpose: a row fails only when the answer is the *dispatcher's* refusal,
//! `404 no such endpoint`. Every other answer counts as "the route exists":
//! a missing board, a missing attachment, a rejected upgrade and a required
//! header are all correct answers from a route that is there.

use hearth_server::api::Options;
use hearth_server::{Server, ROUTES};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// The dispatcher's answer for a path it never matched. Nothing else in the
/// server says this, so the string is the test's whole notion of "absent".
const ABSENT: &str = "no such endpoint";

struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn unique_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "hearth-routes-table-{}-{}-{}",
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
    workspace: PathBuf,
    _temp: TempGuard,
}

impl Fixture {
    fn start(label: &str) -> Self {
        let data_dir = unique_dir(label);
        let workspace = data_dir.join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(workspace.join("README.md"), "# demo\n").expect("readme");
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: data_dir.clone(),
            version: "routes-table-test".to_string(),
            poll_interval: Duration::from_millis(2),
            delta_interval: Duration::from_millis(2),
            workspace_write: true,
            ..Options::default()
        };
        let stop = Arc::new(AtomicBool::new(false));
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
            workspace,
            _temp: TempGuard(data_dir),
        }
    }

    /// Send one request and return `(status, headers, body)`.
    ///
    /// `extra` carries headers a route insists on (`Idempotency-Key`), because a
    /// missing header is a 428 from the route and that is the answer this test
    /// wants to distinguish from "there is no route".
    fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let bearer = format!("Bearer {}", self.token);
        let origin = format!("http://{}", self.addr);
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nX-Hearth-Protocol: 1\r\nOrigin: {origin}\r\nAuthorization: {bearer}\r\n",
            self.addr
        );
        for (key, value) in extra {
            request.push_str(&format!("{key}: {value}\r\n"));
        }
        let bytes = body.map(|value| serde_json::to_vec(value).expect("encode body"));
        if let Some(bytes) = &bytes {
            request.push_str("Content-Type: application/json; charset=utf-8\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", bytes.len()));
        }
        request.push_str("\r\n");

        let mut stream = TcpStream::connect(&self.addr).expect("connect");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        stream.write_all(request.as_bytes()).expect("write request");
        if let Some(bytes) = &bytes {
            stream.write_all(bytes).expect("write body");
        }
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("header terminator");
        let head = std::str::from_utf8(&response[..split]).expect("utf8 head");
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("status");
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            .collect();
        (status, headers, response[split + 4..].to_vec())
    }

    fn create_session(&self) -> String {
        let body = json!({
            "workspace": self.workspace.to_string_lossy(),
            "model": "routes-table-model",
        });
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.server.shutdown();
    }
}

/// Fill the placeholders with values that make the route reachable.
///
/// A placeholder is not a path segment the dispatcher knows; leaving it in would
/// produce a 404 that has nothing to do with whether the route exists.
fn concrete(path: &str, session: &str) -> String {
    path.replace("{id}", session)
        .replace("{board}", "no-such-board")
        .replace("{approvalId}", "no-such-approval")
        .replace("{attachmentId}", "00000000000000000000000000000000")
}

/// The body a `POST` route needs to get past `prepare_post`.
///
/// A route that answers 400 for a missing body is present; a route that answers
/// 404 `no such endpoint` is not. Sending nothing at all would blur the two for
/// the approval and message routes, so the shapes are filled in.
fn body_for(method: &str, path: &str) -> Option<Value> {
    if method != "POST" && method != "PUT" {
        return None;
    }
    if path.ends_with("/messages") {
        return Some(json!({ "text": "hello" }));
    }
    if path.contains("/approvals/") {
        return Some(json!({ "decision": "deny" }));
    }
    if path.ends_with("/cancel") || path.ends_with("/steer") {
        return Some(json!({ "text": "stop" }));
    }
    if path.ends_with("/file") {
        return Some(json!({ "path": "README.md", "text": "# demo\n" }));
    }
    if path.ends_with("/revert") {
        return Some(json!({ "path": "README.md" }));
    }
    if path.ends_with("/sessions") {
        return None; // built by `create_session`, not by the table walk
    }
    None
}

#[test]
fn every_declared_route_is_served() {
    let fixture = Fixture::start("walk");
    let session = fixture.create_session();

    let mut absent = Vec::new();
    for route in ROUTES {
        let path = concrete(route.path, &session);
        // Comments in the table are prose; a row whose path still holds a brace
        // would be a row this test cannot ask about, so it is reported instead of
        // silently skipped.
        if path.contains('{') {
            absent.push(format!("{} (placeholder left in the path)", route.line()));
            continue;
        }
        let extra: &[(&str, &str)] = if path.ends_with("/messages") {
            &[("Idempotency-Key", "routes-table-1")]
        } else {
            &[]
        };
        let (status, _, bytes) = fixture.api(
            route.method,
            &path,
            body_for(route.method, route.path).as_ref(),
            extra,
        );
        let text = String::from_utf8_lossy(&bytes);
        if status == 404 && text.contains(ABSENT) {
            absent.push(format!("{} -> {status} {text}", route.line()));
        }
    }

    assert!(
        absent.is_empty(),
        "these rows are declared but not served:\n{}\n\n\
         Either the route was removed and the table was not, or the path in the \
         table does not match the dispatcher. Both make \
         `GET /api/capabilities` lie to the agent reading it.",
        absent.join("\n")
    );
}

#[test]
fn the_capability_report_carries_the_same_table() {
    let fixture = Fixture::start("report");
    let (status, _, bytes) = fixture.api("GET", "/api/capabilities", None, &[]);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let report: Value = serde_json::from_slice(&bytes).expect("decode capabilities");

    let declared: Vec<String> = ROUTES.iter().map(|route| route.line()).collect();
    let reported: Vec<String> = report["routes"]
        .as_array()
        .expect("routes array")
        .iter()
        .map(|entry| entry["name"].as_str().expect("name").to_string())
        .collect();

    assert_eq!(
        reported, declared,
        "the report and the table disagree; a reader given one of them is being \
         told a different story than a reader given the other"
    );
}

#[test]
fn the_report_names_every_event_type() {
    let fixture = Fixture::start("vocabulary");
    let (status, _, bytes) = fixture.api("GET", "/api/capabilities", None, &[]);
    assert_eq!(status, 200);
    let report: Value = serde_json::from_slice(&bytes).expect("decode capabilities");

    let events = report["events"].as_array().expect("events array").len();
    assert_eq!(
        events,
        hearth_core::EventType::ALL.len(),
        "the report lists {events} event types, the code knows {}",
        hearth_core::EventType::ALL.len()
    );

    // `tools` is filled by whoever wired the tools, which in this fixture is
    // nobody -- so it is present and empty, and the names are asserted where the
    // names are known (`crates/cli/src/capabilities.rs`).
    assert!(
        report["tools"].is_array(),
        "tools must be an array even when nothing filled it: {}",
        report["tools"]
    );
}
