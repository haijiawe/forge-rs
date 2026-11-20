//! stdio transport conformance against the Go oracle's fake-server scenarios.

use std::fs;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hearth_mcp::{
    dial, CallResult, Client, Error, Logger, NoopLogger, Server, ServerInfo, Transport,
    PROTOCOL_VERSION,
};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(3);

struct CapturingLogger {
    tx: Mutex<Sender<String>>,
}

impl CapturingLogger {
    fn new() -> (Arc<Self>, Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(Self { tx: Mutex::new(tx) }), rx)
    }
}

impl Logger for CapturingLogger {
    fn debug(&self, message: &str) {
        let _ = self.tx.lock().map(|tx| tx.send(message.to_string()));
    }

    fn warn(&self, message: &str) {
        let _ = self.tx.lock().map(|tx| tx.send(message.to_string()));
    }
}

fn wait_for(rx: &Receiver<String>, needle: &str) {
    let deadline = Instant::now() + WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for {needle:?}");
        match rx.recv_timeout(remaining) {
            Ok(message) if message.contains(needle) => return,
            Ok(_) => continue,
            Err(_) => panic!("timed out waiting for {needle:?}"),
        }
    }
}

fn server(mode: &str, marker: Option<&Path>, timeout_ms: u64) -> Server {
    let mut command = vec![
        env!("CARGO_BIN_EXE_mcp-fake-server").to_string(),
        "--mode".into(),
        mode.into(),
    ];
    if let Some(path) = marker {
        command.push("--marker".into());
        command.push(path.to_string_lossy().into_owned());
    }
    Server {
        name: "stdio-test".into(),
        transport: Transport::Stdio,
        command,
        timeout_ms,
        ..Server::default()
    }
}

fn connect(
    mode: &str,
    timeout_ms: u64,
) -> (Box<dyn Client>, Arc<CapturingLogger>, Receiver<String>) {
    let (logger, rx) = CapturingLogger::new();
    let client = dial(&server(mode, None, timeout_ms), logger.clone()).expect("dial");
    (client, logger, rx)
}

fn text(result: &CallResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.text.clone())
        .unwrap_or_default()
}

#[test]
fn dial_does_not_handshake_but_initialize_negotiates() {
    // A server that will refuse initialize is still connectable: Dial must not
    // exchange any JSON-RPC.
    let (mut client, _logger, _rx) = connect("badversion", 0);
    let error = client.initialize().expect_err("version mismatch");
    assert!(error.is_protocol_version());
    let _ = client.close();

    let client = dial(&server("plain", None, 0), Arc::new(NoopLogger)).expect("dial");
    let mut client = client;
    let info: ServerInfo = client.initialize().expect("initialize");
    assert_eq!(
        info,
        ServerInfo {
            name: "fake".into(),
            version: "1.2.3".into()
        }
    );
    let _ = client.close();
}

#[test]
fn wrong_protocol_version_is_typed_and_not_a_gone_error() {
    let (mut client, _logger, _rx) = connect("badversion", 0);
    let error = client.initialize().expect_err("version mismatch");
    assert!(error.is_protocol_version(), "unexpected error: {error}");
    assert!(format!("{error}").contains(PROTOCOL_VERSION));
    assert!(!error.is_server_gone());
    let _ = client.close();
}

#[test]
fn refused_initialize_is_a_protocol_error() {
    let (mut client, _logger, _rx) = connect("initrefuse", 0);
    let error = client.initialize().expect_err("initialize refused");
    assert!(!error.is_server_gone(), "unexpected error: {error}");
    let _ = client.close();
}

#[test]
fn initialize_twice_is_rejected() {
    let (mut client, _logger, _rx) = connect("plain", 0);
    client.initialize().expect("first initialize");
    let error = client.initialize().expect_err("second initialize");
    assert!(!error.is_server_gone(), "unexpected error: {error}");
    let _ = client.close();
}

#[test]
fn tools_list_single_page() {
    let (mut client, _logger, _rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let tools = client.tools().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].description, "echo");
    assert_eq!(tools[0].input_schema, json!({"type":"object"}));
    let _ = client.close();
}

#[test]
fn tools_list_follows_cursor() {
    let (mut client, _logger, _rx) = connect("paged", 0);
    client.initialize().expect("initialize");
    let names: Vec<_> = client
        .tools()
        .expect("tools")
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["echo", "env", "hello"]);
    let _ = client.close();
}

#[test]
fn call_echo_and_empty_arguments_default() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let result = client.call("echo", json!({"a":1})).expect("call");
    wait_for(&rx, "tool:echo");
    assert_eq!(text(&result), "{\"a\":1}");
    let result = client.call("echo", Value::Null).expect("call");
    wait_for(&rx, "tool:echo");
    assert_eq!(text(&result), "{}");
    let _ = client.close();
}

#[test]
fn tool_error_is_a_protocol_failure() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let error = client.call("boom", json!({})).expect_err("tool error");
    wait_for(&rx, "tool:boom");
    assert!(!error.is_server_gone(), "unexpected error: {error}");
    let _ = client.close();
}

#[test]
fn tool_level_is_error_is_data_not_rust_error() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let result = client.call("iserror", json!({})).expect("tool result");
    wait_for(&rx, "tool:iserror");
    assert!(result.is_error);
    assert_eq!(text(&result), "tool failed");
    let _ = client.close();
}

#[test]
fn image_content_round_trips() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let result = client.call("image", json!({})).expect("call");
    wait_for(&rx, "tool:image");
    let image = &result.content[0];
    assert_eq!(image.type_, "image");
    assert_eq!(image.data.as_deref(), Some("aGk="));
    assert_eq!(image.mime_type.as_deref(), Some("image/png"));
    let _ = client.close();
}

#[test]
fn notification_before_reply_is_not_a_response() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let result = client.call("notify", json!({})).expect("call");
    wait_for(&rx, "tool:notify");
    assert_eq!(text(&result), "after-notification");
    let _ = client.close();
}

#[test]
fn server_request_gets_method_not_found_and_call_succeeds() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    client.call("serverrequest", json!({})).expect("call");
    wait_for(&rx, "tool:serverrequest");
    let reply = client.call("captured", json!({})).expect("captured");
    wait_for(&rx, "tool:captured");
    let reply = text(&reply);
    assert!(reply.contains("\"id\":999"), "reply was {reply}");
    assert!(reply.contains("-32601"), "reply was {reply}");
    let _ = client.close();
}

#[test]
fn per_call_timeout_is_not_sticky() {
    let (mut client, _logger, rx) = connect("plain", 200);
    client.initialize().expect("initialize");
    let error = client.call("hang", json!({})).expect_err("timeout");
    wait_for(&rx, "tool:hang");
    assert!(
        matches!(error, Error::Timeout(_)),
        "unexpected error: {error}"
    );
    let result = client
        .call("echo", json!({"next":true}))
        .expect("later call");
    wait_for(&rx, "tool:echo");
    assert_eq!(text(&result), "{\"next\":true}");
    let _ = client.close();
}

#[test]
fn server_exit_mid_call_is_server_gone_not_panic_or_hang() {
    let (mut client, _logger, rx) = connect("plain", 2_000);
    client.initialize().expect("initialize");
    let error = client.call("die", json!({})).expect_err("server exits");
    wait_for(&rx, "tool:die");
    assert!(error.is_server_gone(), "unexpected error: {error}");
    let later = client.call("echo", json!({})).expect_err("sticky gone");
    assert!(later.is_server_gone(), "unexpected error: {later}");
    let _ = client.close();
}

#[test]
fn close_during_in_flight_call_unblocks_it() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let mut caller = client.clone_handle();
    let handle = std::thread::spawn(move || caller.call("hang", json!({})));
    wait_for(&rx, "tool:hang");
    let mut closer = client.clone_handle();
    closer.close().expect("close");
    let result = handle.join().expect("call thread");
    let error = result.expect_err("in-flight call must fail");
    assert!(error.is_server_gone(), "unexpected error: {error}");
    let _ = client.close();
}

#[test]
fn close_is_idempotent() {
    let (mut client, _logger, _rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    client.close().expect("first close");
    let mut clone = client.clone_handle();
    clone.close().expect("second close via clone");
    client.close().expect("third close");
    let error = client.call("echo", json!({})).expect_err("closed client");
    assert!(error.is_server_gone(), "unexpected error: {error}");
}

#[test]
fn stderr_noise_never_enters_the_protocol_stream() {
    let (mut client, _logger, rx) = connect("noise", 0);
    client.initialize().expect("initialize");
    let result = client.call("echo", json!({"clean":true})).expect("call");
    wait_for(&rx, "tool:echo");
    assert_eq!(text(&result), "{\"clean\":true}");
    let _ = client.close();
}

#[test]
fn malformed_server_line_ends_connection() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let error = client.call("malformed", json!({})).expect_err("malformed");
    wait_for(&rx, "tool:malformed");
    assert!(error.is_server_gone(), "unexpected error: {error}");
    let _ = client.close();
}

#[test]
fn oversized_server_line_ends_connection() {
    let (mut client, _logger, rx) = connect("plain", 2_000);
    client.initialize().expect("initialize");
    let error = client.call("oversized", json!({})).expect_err("oversized");
    wait_for(&rx, "tool:oversized");
    assert!(error.is_server_gone(), "unexpected error: {error}");
    let _ = client.close();
}

#[test]
fn minimal_environment_is_used_by_default() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let result = client.call("env", json!({})).expect("env");
    wait_for(&rx, "tool:env");
    let env = text(&result);
    assert!(env.contains("PATH="), "env was {env}");
    assert!(!env.contains("HEARTH_SECRET"), "env was {env}");
    let _ = client.close();
}

#[test]
fn explicit_environment_is_exact() {
    let mut config = server("plain", None, 0);
    config.env = vec!["HEARTH_SECRET=only".into(), "EMPTY=".into()];
    let (logger, rx) = CapturingLogger::new();
    let mut client = dial(&config, logger).expect("dial");
    client.initialize().expect("initialize");
    let result = client.call("env", json!({})).expect("env");
    wait_for(&rx, "tool:env");
    let env = text(&result);
    assert!(env.contains("HEARTH_SECRET=only"), "env was {env}");
    assert!(!env.contains("PATH="), "env was {env}");
    let _ = client.close();
}

#[test]
fn graceful_close_writes_marker() {
    let marker =
        std::env::temp_dir().join(format!("hearth-mcp-graceful-{}.marker", std::process::id()));
    let _ = fs::remove_file(&marker);
    let (logger, _rx) = CapturingLogger::new();
    let mut client = dial(&server("plain", Some(&marker), 0), logger).expect("dial");
    client.initialize().expect("initialize");
    client.close().expect("close");
    let deadline = Instant::now() + WAIT;
    while !marker.exists() {
        assert!(Instant::now() < deadline, "marker never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_file(&marker);
}

#[test]
fn close_kills_a_child_that_ignores_eof() {
    let (mut client, _logger, _rx) = connect("ignoreeof", 0);
    client.initialize().expect("initialize");
    let start = Instant::now();
    client.close().expect("close");
    assert!(start.elapsed() < WAIT, "close took {:?}", start.elapsed());
}

#[test]
fn dial_rejects_missing_command() {
    let config = Server {
        name: "bad".into(),
        transport: Transport::Stdio,
        ..Server::default()
    };
    let error = match dial(&config, Arc::new(NoopLogger)) {
        Err(error) => error,
        Ok(_) => panic!("missing command was accepted"),
    };
    assert!(!error.is_server_gone(), "unexpected error: {error}");
}

#[test]
fn concurrent_calls_use_unique_ids() {
    let (mut client, _logger, rx) = connect("plain", 0);
    client.initialize().expect("initialize");
    let mut handles = Vec::new();
    for index in 0..8u64 {
        let mut handle = client.clone_handle();
        handles.push(std::thread::spawn(move || {
            handle.call("echo", json!({"index":index}))
        }));
    }
    for _ in 0..8 {
        wait_for(&rx, "tool:echo");
    }
    for (index, handle) in handles.into_iter().enumerate() {
        let result = handle.join().expect("call thread").expect("call");
        assert_eq!(text(&result), format!("{{\"index\":{index}}}"));
    }
    let _ = client.close();
}
