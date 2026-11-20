//! `GET /api/sessions/{id}/goal` against a real loopback listener.
//!
//! The shared `Fixture` lives in `server_features.rs` and is deliberately not
//! imported or edited here (the lead owns that file and its wiring test), so
//! this file carries the smallest fixture it needs: one temp data dir, one
//! server on an ephemeral port, one authenticated JSON request helper.

use forge_core::Event;
use forge_goal::StopReason;
use forge_server::api::Options;
use forge_server::Server;
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
        "forge-goal-route-{}-{}-{}",
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
    data_dir: PathBuf,
    _temp: TempGuard,
}

impl Fixture {
    fn start(label: &str) -> Self {
        let data_dir = unique_dir(label);
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: data_dir.clone(),
            version: "goal-route-test".to_string(),
            poll_interval: Duration::from_millis(2),
            delta_interval: Duration::from_millis(2),
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
            data_dir: data_dir.clone(),
            _temp: TempGuard(data_dir),
        }
    }

    fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let bearer = format!("Bearer {}", self.token);
        let origin = format!("http://{}", self.addr);
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nX-Forge-Protocol: 1\r\nOrigin: {origin}\r\nAuthorization: {bearer}\r\n",
            self.addr
        );
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
        let workspace = self.data_dir.join("workspace");
        if let Some(parent) = workspace.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let body = json!({
            "workspace": workspace.to_string_lossy(),
            "model": "goal-route-model",
        });
        let (status, _, bytes) = self.api("POST", "/api/sessions", Some(&body));
        assert_eq!(
            status,
            201,
            "create failed: {}",
            String::from_utf8_lossy(&bytes)
        );
        let value: Value = serde_json::from_slice(&bytes).expect("decode create");
        value["session"]["id"].as_str().expect("id").to_string()
    }

    fn append(&self, session: &str, event: Event) {
        self.server
            .append_event(session, &event)
            .expect("append goal event");
    }

    fn goal(&self, session: &str) -> (u16, Value) {
        let (status, _, bytes) = self.api("GET", &format!("/api/sessions/{session}/goal"), None);
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("decode goal")
        };
        (status, value)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.server.shutdown();
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Every file under `root` with its length, so a test can prove a read created
/// nothing. A directory that does not exist is an empty listing.
fn snapshot(root: &Path) -> Vec<(String, u64)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => walk(root, &path, out),
                Ok(metadata) => out.push((name, metadata.len())),
                Err(_) => out.push((name, 0)),
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn a_session_with_no_goal_answers_null() {
    let fixture = Fixture::start("none");
    let session = fixture.create_session();

    let (status, body) = fixture.goal(&session);
    assert_eq!(status, 200);
    assert_eq!(body, json!({ "goal": null }));

    let (status, headers, bytes) =
        fixture.api("GET", &format!("/api/sessions/{session}/goal"), None);
    assert_eq!(status, 200);
    assert_eq!(
        header(&headers, "Content-Type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(String::from_utf8_lossy(&bytes), r#"{"goal":null}"#);
}

#[test]
fn an_active_goal_is_reported_with_the_round_that_comes_next() {
    let fixture = Fixture::start("active");
    let session = fixture.create_session();
    let goal = forge_goal::Goal::new("g1", "ship the goal route", 10);
    fixture.append(&session, forge_goal::goal_created(&session, &goal));
    fixture.append(&session, forge_goal::goal_continued(&session, &goal, 1));

    let (status, body) = fixture.goal(&session);
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({
            "goal": {
                "id": "g1",
                "text": "ship the goal route",
                "maxRounds": 10,
                "rounds": 1,
                "status": "active",
                // Round 1 was the creating turn, round 2 is this continuation,
                // so the next round is round 3 -- a round number, not the
                // `Action::Continue` index the loop keeps internally.
                "nextRound": 3,
            }
        })
    );
}

#[test]
fn a_finished_goal_has_no_next_round() {
    let fixture = Fixture::start("blocked");
    let session = fixture.create_session();
    let goal = forge_goal::Goal::new("g1", "ship the goal route", 10);
    fixture.append(&session, forge_goal::goal_created(&session, &goal));
    fixture.append(&session, forge_goal::goal_continued(&session, &goal, 1));
    fixture.append(
        &session,
        forge_goal::goal_ended(&session, &goal, StopReason::Blocked, Some("no token")),
    );

    let (status, body) = fixture.goal(&session);
    assert_eq!(status, 200);
    assert_eq!(body["goal"]["status"], "blocked");
    assert_eq!(body["goal"]["rounds"], 1);
    assert!(
        body["goal"].get("nextRound").is_none(),
        "an inactive goal must not carry a number: {body}"
    );
}

#[test]
fn an_unknown_session_is_not_found() {
    let fixture = Fixture::start("unknown");
    let (status, body) = fixture.goal("s_01jya7c9q3r5t7v9w1x3y5zz");
    assert_eq!(status, 404);
    assert_eq!(body["error"]["code"], "not_found");
}

#[test]
fn any_method_but_get_is_not_allowed() {
    let fixture = Fixture::start("method");
    let session = fixture.create_session();
    for method in ["POST", "PUT", "DELETE"] {
        let (status, headers, bytes) = fixture.api(
            method,
            &format!("/api/sessions/{session}/goal"),
            Some(&json!({})),
        );
        assert_eq!(status, 405, "{method} should not be allowed");
        assert_eq!(header(&headers, "Allow"), Some("GET"));
        let body: Value = serde_json::from_slice(&bytes).expect("decode 405");
        assert_eq!(body["error"]["code"], "method_not_allowed");
    }
}

#[test]
fn the_route_creates_nothing() {
    let fixture = Fixture::start("readonly");
    let session = fixture.create_session();
    let before = snapshot(&fixture.data_dir);

    for _ in 0..3 {
        let (status, _) = fixture.goal(&session);
        assert_eq!(status, 200);
        let (status, _) = fixture.goal("s_01jya7c9q3r5t7v9w1x3y5zz");
        assert_eq!(status, 404);
    }

    assert_eq!(
        snapshot(&fixture.data_dir),
        before,
        "a read-only route changed the data dir"
    );
    let events_dir = fixture.data_dir.join("sessions");
    assert!(events_dir.is_dir(), "the seeded session's log must exist");
}
