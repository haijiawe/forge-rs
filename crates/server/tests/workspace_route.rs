//! The three read-only workspace routes against a real loopback listener.
//!
//! Like `goal_route.rs`, this file carries the smallest fixture it needs instead
//! of importing the shared one: the point of these tests is the wire answer, and
//! a fixture that grows to serve every route stops being readable.
//!
//! What these tests are for: the editor pane reads the tree the agent works in.
//! That means the routes must (a) answer the shapes the UI asks for, (b) refuse
//! every way out of the workspace, and (c) create nothing — a GET that writes is
//! a GET that lies about being safe to call.

use hearth_server::api::Options;
use hearth_server::Server;
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
        "hearth-workspace-test-{}-{}-{}",
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
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: data_dir.clone(),
            version: "workspace-route-test".to_string(),
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
        let workspace = data_dir.join("workspace");
        Self {
            addr,
            token: server.token().to_string(),
            server,
            stop,
            workspace,
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
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nX-Hearth-Protocol: 1\r\nOrigin: {origin}\r\nAuthorization: {bearer}\r\n",
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
        let body = json!({
            "workspace": self.workspace.to_string_lossy(),
            "model": "workspace-route-model",
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

    fn get(&self, session: &str, suffix: &str) -> (u16, Value) {
        let (status, _, bytes) =
            self.api("GET", &format!("/api/sessions/{session}/{suffix}"), None);
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
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

/// Every file under `root` with its length, so a test can prove a read created
/// nothing.
fn snapshot(root: &std::path::Path) -> Vec<(String, u64)> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, u64)>) {
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

fn seed_workspace(fixture: &Fixture) {
    fs::create_dir_all(fixture.workspace.join("src")).expect("src dir");
    fs::write(fixture.workspace.join("README.md"), "# demo\n").expect("readme");
    fs::write(fixture.workspace.join("src/main.rs"), "fn main() {}\n").expect("main");
    fs::write(fixture.workspace.join("blob.bin"), [0u8, 1, 2]).expect("blob");
}

#[test]
fn a_directory_listing_names_the_tree_the_agent_was_given() {
    let fixture = Fixture::start("list");
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, body) = fixture.get(&session, "files");
    assert_eq!(status, 200, "listing failed: {body}");
    assert_eq!(body["path"], "");
    let names: Vec<&str> = body["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["name"].as_str().expect("name"))
        .collect();
    // Directories first, then files, each alphabetically.
    assert_eq!(names, vec!["src", "blob.bin", "README.md"]);
    assert_eq!(body["entries"][0]["kind"], "dir");
    assert_eq!(body["entries"][1]["kind"], "file");

    // A subdirectory is addressable, and its path comes back normalised.
    let (status, body) = fixture.get(&session, "files?path=src");
    assert_eq!(status, 200, "subdirectory failed: {body}");
    assert_eq!(body["path"], "src");
    assert_eq!(body["entries"][0]["name"], "main.rs");
}

#[test]
fn a_file_is_answered_as_text_and_a_binary_one_says_so() {
    let fixture = Fixture::start("read");
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, body) = fixture.get(&session, "file?path=src/main.rs");
    assert_eq!(status, 200, "read failed: {body}");
    assert_eq!(body["path"], "src/main.rs");
    assert_eq!(body["text"], "fn main() {}\n");
    assert_eq!(body["size"], 13);
    assert_eq!(body["truncated"], false);
    assert_eq!(body["binary"], false);

    let (status, body) = fixture.get(&session, "file?path=blob.bin");
    assert_eq!(status, 200, "binary read failed: {body}");
    assert_eq!(body["binary"], true);
    assert_eq!(body["text"], "");
}

#[test]
fn every_way_out_of_the_workspace_is_a_bad_path() {
    let fixture = Fixture::start("escape");
    seed_workspace(&fixture);
    let session = fixture.create_session();

    // `..` in any position, an absolute path, and a Windows drive letter.
    for path in [
        "file?path=../secret.txt",
        "file?path=src/../../secret.txt",
        "file?path=/etc/passwd",
        "file?path=C:/Windows/win.ini",
        "files?path=../../",
    ] {
        let (status, body) = fixture.get(&session, path);
        assert_eq!(status, 400, "{path} should be refused, got {body}");
        assert_eq!(body["error"]["code"], "bad_request", "for {path}");
    }
}

#[test]
fn a_path_that_is_not_there_is_a_not_found_and_not_a_creation() {
    let fixture = Fixture::start("missing");
    seed_workspace(&fixture);
    let session = fixture.create_session();
    let before = snapshot(&fixture.workspace);

    let (status, body) = fixture.get(&session, "file?path=nope/missing.txt");
    assert_eq!(status, 404, "missing file should be 404, got {body}");
    assert_eq!(body["error"]["code"], "not_found");

    // The request must not have created the directory it looked in.
    assert_eq!(snapshot(&fixture.workspace), before, "a GET created files");
}

#[test]
fn a_file_needs_a_path_and_a_directory_is_not_a_file() {
    let fixture = Fixture::start("shape");
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, body) = fixture.get(&session, "file");
    assert_eq!(status, 400, "no path should be 400, got {body}");

    let (status, _) = fixture.get(&session, "file?path=src");
    assert_eq!(status, 400, "a directory is not a file");

    let (status, _) = fixture.get(&session, "files?path=README.md");
    assert_eq!(status, 400, "a file is not a directory");
}

#[test]
fn a_workspace_that_is_not_a_repository_reports_no_changes() {
    let fixture = Fixture::start("diff");
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, body) = fixture.get(&session, "diff");
    assert_eq!(status, 200, "diff failed: {body}");
    assert_eq!(body["files"], json!([]));
}

#[test]
fn a_session_that_does_not_exist_is_a_not_found_on_all_three_routes() {
    let fixture = Fixture::start("nosession");
    for suffix in ["files", "file?path=x", "diff"] {
        let (status, _) = fixture.get("s-does-not-exist", suffix);
        assert_eq!(status, 404, "{suffix} on a missing session");
    }
}

#[test]
fn a_write_method_on_a_read_route_is_refused() {
    let fixture = Fixture::start("methods");
    seed_workspace(&fixture);
    let session = fixture.create_session();
    for route in ["files", "file?path=README.md", "diff"] {
        let (status, _, _) = fixture.api(
            "POST",
            &format!("/api/sessions/{session}/{route}"),
            Some(&json!({})),
        );
        assert_eq!(status, 405, "{route} should not accept POST");
    }
}
