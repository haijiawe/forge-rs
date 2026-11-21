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
        Self::start_with(label, false)
    }

    /// `write` turns on `PUT /file`, the way `hearth web` does for a session
    /// whose sandbox is not read-only.
    fn start_with(label: &str, write: bool) -> Self {
        let data_dir = unique_dir(label);
        let options = Options {
            addr: "127.0.0.1:0".to_string(),
            data_dir: data_dir.clone(),
            version: "workspace-route-test".to_string(),
            poll_interval: Duration::from_millis(2),
            delta_interval: Duration::from_millis(2),
            workspace_write: write,
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

/// Commit the seeded files, so a revert has a version to go back to.
///
/// Returns false when git cannot run, and every caller skips rather than failing
/// on a machine that has no git — the point of the test is the route, not the
/// installer.
fn commit_workspace(fixture: &Fixture) -> bool {
    for args in [
        vec!["add", "README.md"],
        vec!["commit", "--quiet", "-m", "seed"],
    ] {
        let done = std::process::Command::new("git")
            .arg("-C")
            .arg(&fixture.workspace)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(&args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output();
        match done {
            Ok(output) if output.status.success() => {}
            _ => return false,
        }
    }
    true
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

    // The same route with a path still answers: a file outside version control
    // is a change with every line added, not an error.
    let (status, body) = fixture.get(&session, "diff?path=README.md");
    assert_eq!(status, 200, "single diff failed: {body}");
    assert_eq!(body["status"], "unchanged");
    assert_eq!(body["text"], "");
}

#[test]
fn one_file_diff_carries_the_lines_and_the_list_carries_the_rows() {
    let fixture = Fixture::start("difflines");
    seed_workspace(&fixture);
    // A repository with one commit, so "modified" means what it says.
    for args in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "add",
            "README.md",
        ],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "first",
        ],
    ] {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(&fixture.workspace)
            .args(&args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git runs");
        assert!(ran.status.success(), "git {args:?} failed");
    }
    fs::write(fixture.workspace.join("README.md"), "# demo\nmore\n").expect("edit");
    let session = fixture.create_session();

    let (status, body) = fixture.get(&session, "diff");
    assert_eq!(status, 200, "diff failed: {body}");
    let files = body["files"].as_array().expect("files");
    // The seeded workspace holds other files, so this looks the row up rather
    // than counting: git reports an untracked *directory* as a single `src/`
    // row, which is correct and not something this test should pin.
    let row = files
        .iter()
        .find(|row| row["path"] == "README.md")
        .unwrap_or_else(|| panic!("no README.md row in {files:?}"));
    assert_eq!(row["status"], "modified");
    assert_eq!(row["additions"], 1);

    let (status, body) = fixture.get(&session, "diff?path=README.md");
    assert_eq!(status, 200, "single diff failed: {body}");
    assert_eq!(body["path"], "README.md");
    assert_eq!(body["status"], "modified");
    assert_eq!(body["truncated"], false);
    let text = body["text"].as_str().expect("text");
    assert!(text.contains("+more"), "text was {text:?}");
    assert!(text.contains("--- a/README.md"), "text was {text:?}");

    // Every way out of the workspace is refused on this route too.
    let (status, _) = fixture.get(&session, "diff?path=../../outside.txt");
    assert_eq!(status, 400);
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

/// The log lines of one session, so a test can prove a refusal appended nothing.
fn log_lines(fixture: &Fixture, session: &str) -> Vec<String> {
    let path = fixture.server.path(session);
    match fs::read_to_string(path) {
        Ok(text) => text.lines().map(|line| line.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn a_save_writes_the_bytes_and_records_which_digest_they_replaced() {
    let fixture = Fixture::start_with("write", true);
    seed_workspace(&fixture);
    let session = fixture.create_session();

    // Read first: the digest that comes back is the token a save must hand in.
    let (status, body) = fixture.get(&session, "file?path=src/main.rs");
    assert_eq!(status, 200, "read failed: {body}");
    let before = body["sha256"].as_str().expect("digest").to_string();
    assert!(!before.is_empty(), "an untruncated text file has a digest");

    let (status, _, body) = fixture.api(
        "PUT",
        &format!("/api/sessions/{session}/file"),
        Some(&json!({
            "path": "src/main.rs",
            "text": "fn main() { println!(\"hi\"); }\n",
            "expectedSha256": before,
        })),
    );
    assert_eq!(
        status,
        200,
        "save failed: {}",
        String::from_utf8_lossy(&body)
    );

    let on_disk = fs::read_to_string(fixture.workspace.join("src/main.rs")).expect("disk");
    assert_eq!(on_disk, "fn main() { println!(\"hi\"); }\n");

    // The record names the path, the size and the digest of what landed, and it
    // is not a tool call: a person saved a buffer, no model was asked.
    let lines = log_lines(&fixture, &session);
    let record = lines
        .iter()
        .find(|line| line.contains("file/write"))
        .unwrap_or_else(|| panic!("no file/write record in {lines:?}"));
    let event: Value = serde_json::from_str(record).expect("record json");
    assert_eq!(event["type"], "file/write");
    assert_eq!(event["payload"]["path"], "src/main.rs");
    assert_eq!(event["payload"]["bytes"], on_disk.len() as u64);
    assert_eq!(
        event["payload"]["sha256"].as_str().expect("digest"),
        hearth_core::sha256_hex(on_disk.as_bytes())
    );
    assert!(
        !lines.iter().any(|line| line.contains("tool/call")),
        "a save is not a tool call"
    );

    // A second read agrees with the save, which is what makes the token usable.
    let (status, body) = fixture.get(&session, "file?path=src/main.rs");
    assert_eq!(status, 200);
    assert_eq!(body["sha256"], event["payload"]["sha256"]);
}

#[test]
fn a_save_without_the_server_permission_is_forbidden_and_changes_nothing() {
    let fixture = Fixture::start_with("readonly", false);
    seed_workspace(&fixture);
    let session = fixture.create_session();
    let before_tree = snapshot(&fixture.workspace);
    let before_log = log_lines(&fixture, &session);

    let (status, _, bytes) = fixture.api(
        "PUT",
        &format!("/api/sessions/{session}/file"),
        Some(&json!({"path": "src/main.rs", "text": "overwritten\n"})),
    );
    assert_eq!(
        status,
        403,
        "read-only save: {}",
        String::from_utf8_lossy(&bytes)
    );
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["error"]["code"], "forbidden");

    assert_eq!(
        snapshot(&fixture.workspace),
        before_tree,
        "the file changed"
    );
    assert_eq!(
        log_lines(&fixture, &session),
        before_log,
        "a refusal was recorded"
    );
}

#[test]
fn a_save_with_a_stale_digest_is_a_conflict_and_leaves_the_file_alone() {
    let fixture = Fixture::start_with("stale", true);
    seed_workspace(&fixture);
    let session = fixture.create_session();
    let before_tree = snapshot(&fixture.workspace);
    let before_log = log_lines(&fixture, &session);

    let (status, _, bytes) = fixture.api(
        "PUT",
        &format!("/api/sessions/{session}/file"),
        Some(&json!({
            "path": "README.md",
            "text": "# mine\n",
            "expectedSha256": "0000000000000000000000000000000000000000000000000000000000000000",
        })),
    );
    assert_eq!(
        status,
        409,
        "stale digest: {}",
        String::from_utf8_lossy(&bytes)
    );
    let value: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(value["error"]["code"], "conflict");

    assert_eq!(snapshot(&fixture.workspace), before_tree);
    assert_eq!(log_lines(&fixture, &session), before_log);
}

#[test]
fn a_save_refuses_every_way_out_of_the_workspace() {
    let fixture = Fixture::start_with("write-escape", true);
    seed_workspace(&fixture);
    let session = fixture.create_session();
    let before = snapshot(&fixture.workspace);

    for path in [
        "../secret.txt",
        "src/../../secret.txt",
        "/etc/passwd",
        "C:/Windows/win.ini",
    ] {
        let (status, _, bytes) = fixture.api(
            "PUT",
            &format!("/api/sessions/{session}/file"),
            Some(&json!({"path": path, "text": "x"})),
        );
        assert_eq!(status, 400, "{path}: {}", String::from_utf8_lossy(&bytes));
    }
    assert_eq!(snapshot(&fixture.workspace), before);
}

#[test]
fn a_save_needs_a_path_and_a_body() {
    let fixture = Fixture::start_with("write-shape", true);
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, _, _) = fixture.api(
        "PUT",
        &format!("/api/sessions/{session}/file"),
        Some(&json!({"text": "no path"})),
    );
    assert_eq!(status, 400, "a save without a path");

    let (status, _, _) = fixture.api("PUT", &format!("/api/sessions/{session}/file"), None);
    assert_eq!(status, 400, "a save without a body");

    let (status, _, _) = fixture.api(
        "PUT",
        &format!("/api/sessions/{session}/files"),
        Some(&json!({"path": "x", "text": "y"})),
    );
    assert_eq!(status, 405, "the listing route is not a save route");
}

#[test]
fn a_revert_restores_the_committed_bytes_and_records_that_it_did() {
    let fixture = Fixture::start_with("revert", true);
    seed_workspace(&fixture);
    if !commit_workspace(&fixture) {
        return;
    }
    let session = fixture.create_session();
    fs::write(fixture.workspace.join("README.md"), "# scribbled\n").expect("edit");

    let (status, _, bytes) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/revert"),
        Some(&json!({"path": "README.md"})),
    );
    assert_eq!(
        status,
        200,
        "revert failed: {}",
        String::from_utf8_lossy(&bytes)
    );
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let on_disk = fs::read(fixture.workspace.join("README.md")).expect("disk");
    assert_eq!(body["bytes"].as_u64().expect("bytes"), on_disk.len() as u64);
    // The digest describes the bytes now on disk, which is what lets a reader
    // match the row against the file later. Comparing against the literal
    // "# demo\n" would be a statement about the developer's autocrlf setting.
    assert_eq!(
        body["sha256"].as_str().expect("digest"),
        hearth_core::sha256_hex(&on_disk)
    );
    assert_eq!(
        String::from_utf8(on_disk).expect("utf8").trim_end(),
        "# demo"
    );

    let lines = log_lines(&fixture, &session);
    let record = lines
        .iter()
        .find(|line| line.contains("file/revert"))
        .unwrap_or_else(|| panic!("no file/revert record in {lines:?}"));
    let event: Value = serde_json::from_str(record).expect("record json");
    assert_eq!(event["type"], "file/revert");
    assert_eq!(event["payload"]["path"], "README.md");
    // A revert is not a save: the log must not claim a person typed this.
    assert!(
        !lines.iter().any(|line| line.contains("file/write")),
        "a revert was recorded as a write"
    );
    assert!(!lines.iter().any(|line| line.contains("tool/call")));
}

#[test]
fn a_revert_refuses_an_untracked_file_and_records_nothing() {
    let fixture = Fixture::start_with("revert-untracked", true);
    seed_workspace(&fixture);
    if !commit_workspace(&fixture) {
        return;
    }
    let session = fixture.create_session();
    fs::write(fixture.workspace.join("draft.txt"), "mine\n").expect("draft");
    let before_tree = snapshot(&fixture.workspace);
    let before_log = log_lines(&fixture, &session);

    let (status, _, bytes) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/revert"),
        Some(&json!({"path": "draft.txt"})),
    );
    assert_eq!(
        status,
        409,
        "untracked revert: {}",
        String::from_utf8_lossy(&bytes)
    );
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["error"]["code"], "conflict");
    // Refused means untouched: reverting is not deleting.
    assert_eq!(snapshot(&fixture.workspace), before_tree);
    assert_eq!(log_lines(&fixture, &session), before_log);
}

#[test]
fn a_revert_without_the_server_permission_is_forbidden() {
    let fixture = Fixture::start_with("revert-readonly", false);
    seed_workspace(&fixture);
    let session = fixture.create_session();
    let before_tree = snapshot(&fixture.workspace);

    let (status, _, bytes) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/revert"),
        Some(&json!({"path": "README.md"})),
    );
    assert_eq!(
        status,
        403,
        "read-only revert: {}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(snapshot(&fixture.workspace), before_tree);
}

#[test]
fn a_revert_needs_a_path_and_refuses_other_methods() {
    let fixture = Fixture::start_with("revert-shape", true);
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, _, _) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/revert"),
        Some(&json!({"path": "  "})),
    );
    assert_eq!(status, 400, "a blank path");

    let (status, _, _) = fixture.api("POST", &format!("/api/sessions/{session}/revert"), None);
    assert_eq!(status, 400, "a revert without a body");

    let (status, _, _) = fixture.api("GET", &format!("/api/sessions/{session}/revert"), None);
    assert_eq!(status, 405, "a revert is not a read");
}

#[test]
fn a_search_answers_with_lines_across_the_workspace() {
    let fixture = Fixture::start_with("search-live", true);
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, _, bytes) = fixture.api(
        "GET",
        &format!("/api/sessions/{session}/search?q=fn%20main"),
        None,
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(body["query"], "fn main");
    let hits = body["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 1, "{body}");
    assert_eq!(hits[0]["path"], "src/main.rs");
    assert_eq!(hits[0]["line"], 1);
    assert_eq!(hits[0]["column"], 1);
    assert_eq!(hits[0]["text"], "fn main() {}");
    assert_eq!(body["truncated"], false);

    // 二进制文件不该被当成文本搜索的对象：`blob.bin` 里那两个 NUL 之后
    // 的内容不会出现在结果里，命中的只有真正是文本的文件。
    let (status, _, bytes) = fixture.api(
        "GET",
        &format!("/api/sessions/{session}/search?q=demo"),
        None,
    );
    assert_eq!(status, 200);
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let hits = body["hits"].as_array().expect("hits");
    assert_eq!(hits.len(), 1, "{body}");
    assert_eq!(hits[0]["path"], "README.md");
}

#[test]
fn a_search_needs_a_query_and_refuses_other_methods() {
    let fixture = Fixture::start_with("search-shape", true);
    seed_workspace(&fixture);
    let session = fixture.create_session();

    let (status, _, bytes) = fixture.api("GET", &format!("/api/sessions/{session}/search"), None);
    assert_eq!(status, 400, "{}", String::from_utf8_lossy(&bytes));

    let (status, _, _) = fixture.api(
        "GET",
        &format!("/api/sessions/{session}/search?q=%20%20"),
        None,
    );
    assert_eq!(status, 400, "只填空格不是查询");

    let (status, _, _) = fixture.api(
        "POST",
        &format!("/api/sessions/{session}/search?q=x"),
        Some(&json!({})),
    );
    assert_eq!(status, 405, "搜索是读操作");

    // 一个不存在的会话仍然是 404，而不是空结果。
    let (status, _, _) = fixture.api("GET", "/api/sessions/s_nope/search?q=x", None);
    assert_eq!(status, 404);
}
