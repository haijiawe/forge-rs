//! Rust mirror of the Go `internal/llm/openai` test suite.
//!
//! The source modules are included by path so the provider can be tested in
//! isolation. `use forge_llm::*` brings the frozen contract into this test
//! crate's root, where the included modules' `use crate::{Delta, …}` resolves.
//! It is deliberately private: a `pub use` here would now shadow the real
//! re-exports from `src/lib.rs` and trip `-D hidden_glob_reexports`.

use forge_llm::*;

#[path = "../src/openai.rs"]
mod openai;
#[path = "../src/sse.rs"]
mod sse;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use forge_core::Error;
use openai::{
    parse_retry_after, transport_for_scheme, Client, Options, Transport, CODE_BAD_EVENT,
    CODE_CLOSED, CODE_INVALID_REQUEST, CODE_STREAM, CODE_TRANSPORT, MAX_ERROR_BODY,
};

const TEST_KEY: &str = "sk-test-KEY-0123456789abcdef";

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn transport_selection_uses_tls_for_https_only() {
    assert_eq!(transport_for_scheme("https"), Transport::Tls);
    assert_eq!(transport_for_scheme("HTTPS"), Transport::Tls);
    assert_eq!(transport_for_scheme("http"), Transport::Plain);
}

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_client(options: Options) -> Client {
    let _guard = env_lock();
    Client::new(options).unwrap()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A local, single-request HTTP server.
struct Server {
    url: String,
    captured: Arc<Mutex<Vec<u8>>>,
    handle: Option<JoinHandle<()>>,
}

impl Server {
    fn request(&self) -> Vec<u8> {
        self.captured.lock().unwrap().clone()
    }

    fn url(&self) -> String {
        self.url.clone()
    }

    fn finish(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.join().unwrap();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn raw_server<F>(writer: F) -> Server
where
    F: FnOnce(TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        // Drain the request before replying: closing a Windows socket with
        // unread input sends RST, which would erase the response body.
        let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
        let mut sink = [0u8; 8192];
        while socket.read(&mut sink).map(|n| n > 0).unwrap_or(false) {}
        let _ = socket.set_read_timeout(None);
        writer(socket);
    });
    Server {
        url: format!("http://{addr}/v1"),
        captured: Arc::new(Mutex::new(Vec::new())),
        handle: Some(handle),
    }
}

fn read_request(socket: &mut TcpStream) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut body_start = None;
    let mut content_length = 0usize;
    loop {
        let n = socket.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..n]);
        if body_start.is_none() {
            if let Some(position) = find_subslice(&data, b"\r\n\r\n") {
                body_start = Some(position + 4);
                let head = String::from_utf8_lossy(&data[..position]);
                for line in head.lines() {
                    let lower = line.to_ascii_lowercase();
                    if let Some(value) = lower.strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                }
            }
        }
        if let Some(start) = body_start {
            if data.len() >= start + content_length {
                break;
            }
        }
    }
    data
}

fn http_response(status: u16, body: &[u8], extra: &str, chunked: bool) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Status",
    };
    let mut head = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n");
    if chunked {
        head.push_str("Transfer-Encoding: chunked\r\n");
    } else {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str(extra);
    head.push_str("Connection: close\r\n\r\n");
    let mut bytes = head.into_bytes();
    if chunked {
        bytes.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\r\n0\r\n\r\n");
    } else {
        bytes.extend_from_slice(body);
    }
    bytes
}

fn sse_body(events: &[&str]) -> Vec<u8> {
    let mut body = Vec::new();
    for event in events {
        body.extend_from_slice(b"data: ");
        body.extend_from_slice(event.as_bytes());
        body.extend_from_slice(b"\n\n");
    }
    body.extend_from_slice(b"data: [DONE]\n\n");
    body
}

fn scripted(events: &[&str]) -> Server {
    let response = http_response(200, &sse_body(events), "", false);
    let captured = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured2 = captured.clone();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        *captured2.lock().unwrap() = read_request(&mut socket);
        socket.write_all(&response).unwrap();
    });
    Server {
        url: format!("http://{addr}/v1"),
        captured,
        handle: Some(handle),
    }
}

fn request(model: &str) -> Request {
    Request {
        model: model.to_string(),
        messages: vec![Message::user("hello")],
        tools: vec![],
        temperature: None,
        max_tokens: None,
        stop: vec![],
    }
}

fn client(url: &str) -> Client {
    new_client(Options {
        base_url: url.to_string(),
        api_key: TEST_KEY.to_string(),
        ca_file: None,
    })
}

fn drain(stream: &mut dyn Stream) -> Vec<Delta> {
    let mut deltas = Vec::new();
    while let Some(delta) = stream.recv().unwrap() {
        deltas.push(delta);
    }
    deltas
}

fn texts(deltas: &[Delta]) -> String {
    deltas.iter().map(|d| d.text.as_str()).collect()
}

fn provider_error(err: &Error) -> ProviderError {
    openai::provider_error(err)
        .expect("provider error missing")
        .clone()
}

// ---------------------------------------------------------------------------
// SSE framing
// ---------------------------------------------------------------------------

#[test]
fn sse_reader_byte_at_a_time_with_crlf() {
    wire_reads(
        |mut socket| {
            let body = b": comment\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"h\xc3\xa9llo \xe4\xb8\x96\xe7\x95\x8c \xf0\x9f\x9a\x80\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n";
            let response = http_response(200, body, "", false);
            socket.write_all(&response).unwrap();
        },
        |client| {
            let mut stream = client.stream(&request("gpt-4o")).unwrap();
            let deltas = drain(&mut *stream);
            assert_eq!(texts(&deltas), "héllo 世界 🚀");
        },
    );
}

#[test]
fn sse_reader_skips_comments_keepalives_and_foreign_fields() {
    let noise = b":keep-alive\n\nevent: message\nid: 42\nretry: 10000\ndata:{\"choices\":[{\"index\":0,\"delta\":{\"content\":\"no-space\"}}]}\n\ndata: \n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
    let custom = raw_server(move |mut socket| {
        let response = http_response(200, noise, "", false);
        socket.write_all(&response).unwrap();
    });
    let custom_client = client(&custom.url());
    let mut stream = custom_client.stream(&request("gpt-4o")).unwrap();
    let deltas = drain(&mut *stream);
    assert_eq!(texts(&deltas), "no-spaceok");
}

#[test]
fn sse_reader_trailing_event_without_blank_line() {
    let raw =
        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"tail\"}}]}\n\ndata: [DONE]";
    let server = raw_server(move |mut socket| {
        socket
            .write_all(&http_response(200, raw, "", false))
            .unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let deltas = drain(&mut *stream);
    // No [DONE] => error, but the trailing event is delivered first.
    assert_eq!(texts(&deltas), "tail");
}

#[test]
fn oversize_line_errors() {
    let mut line = vec![b'x'; sse::MAX_EVENT_BYTES + 1];
    line.extend_from_slice(b"\n\n");
    let server = raw_server(move |mut socket| {
        socket
            .write_all(&http_response(200, &line, "", false))
            .unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let err = stream.recv().unwrap_err();
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_STREAM);
    assert!(!pe.retryable);
}

#[test]
fn payloads_single_joined_and_separate() {
    let single = vec!["{}".to_string()];
    assert_eq!(sse::payloads(&single), single);
    let joined = vec!["{".to_string(), "\"a\": 1".to_string(), "}".to_string()];
    assert_eq!(sse::payloads(&joined), vec!["{\n\"a\": 1\n}"]);
    let separate = vec!["{}".to_string(), "{}".to_string()];
    assert_eq!(sse::payloads(&separate), separate);
}

#[test]
fn decode_usage_precedence() {
    let payload = r#"{"choices":[{"index":0,"delta":{}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":99,"prompt_tokens_details":{"cached_tokens":64},"prompt_cache_hit_tokens":30,"cache_read_input_tokens":7,"cache_creation_input_tokens":8}}"#;
    let deltas = sse::decode_event(payload).unwrap();
    let usage = deltas.iter().find_map(|d| d.usage.as_ref()).unwrap();
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.cache_read_tokens, 64);
    assert_eq!(usage.cache_write_tokens, 8);
    assert_eq!(usage.total(), 87);
}

#[test]
fn decode_reasoning_before_text_and_never_mixed() {
    let payload =
        r#"{"choices":[{"index":0,"delta":{"content":"answer","reasoning_content":"think"}}]}"#;
    let deltas = sse::decode_event(payload).unwrap();
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0].reasoning, "think");
    assert!(deltas[0].text.is_empty());
    assert_eq!(deltas[1].text, "answer");
    assert!(deltas[1].reasoning.is_empty());

    let legacy = r#"{"choices":[{"index":0,"delta":{"reasoning":"legacy"}}]}"#;
    let deltas = sse::decode_event(legacy).unwrap();
    assert_eq!(deltas[0].reasoning, "legacy");
}

#[test]
fn decode_tool_calls_then_finish_reason() {
    let payload = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get","arguments":"{\"x\":"}},{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"tool_calls"}]}"#;
    let deltas = sse::decode_event(payload).unwrap();
    assert_eq!(deltas.len(), 3);
    assert_eq!(deltas[0].tool_call.as_ref().unwrap().id, "call_1");
    assert_eq!(deltas[1].tool_call.as_ref().unwrap().args_fragment, "1}");
    assert_eq!(deltas[2].stop_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn decode_empty_tool_fragment_skipped() {
    let payload = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":""}}]}}]}"#;
    assert!(sse::decode_event(payload).unwrap().is_empty());
}

#[test]
fn decode_broken_json_is_bad_event() {
    let payload = "{not json";
    let err = sse::decode_event(payload).unwrap_err();
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_BAD_EVENT);
    assert!(!pe.retryable);
    assert!(pe.body.contains("{not json"));
}

#[test]
fn decode_midstream_api_error() {
    let payload = r#"{"error":{"message":"too long","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
    let err = sse::decode_event(payload).unwrap_err();
    let pe = provider_error(&err);
    assert_eq!(pe.code, "context_length_exceeded");
    assert_eq!(pe.message, "too long");
    assert!(!pe.retryable);
    assert!(pe.body.contains("context_length_exceeded"));
}

#[test]
fn first_choice_prefers_index_zero() {
    let payload = r#"{"choices":[{"index":1,"delta":{"content":"second"}},{"index":0,"delta":{"content":"first"}}]}"#;
    let deltas = sse::decode_event(payload).unwrap();
    assert_eq!(deltas[0].text, "first");
}

// ---------------------------------------------------------------------------
// Client / stream behaviour
// ---------------------------------------------------------------------------

fn wire_reads<F, G>(writer: F, check: G)
where
    F: FnOnce(TcpStream) + Send + 'static,
    G: FnOnce(Client),
{
    let server = raw_server(writer);
    let client = client(&server.url());
    check(client);
}

#[test]
fn plain_text_with_usage_and_request_wire() {
    let events = [
        r#"{"choices":[{"index":0,"delta":{"content":"hello"}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":" world"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
    ];
    let server = scripted(&events);
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o-mini")).unwrap();
    let deltas = drain(&mut *stream);
    assert_eq!(texts(&deltas), "hello world");
    assert_eq!(
        deltas.iter().find_map(|d| d.stop_reason.as_deref()),
        Some("stop")
    );
    assert_eq!(
        deltas
            .iter()
            .find_map(|d| d.usage.as_ref())
            .unwrap()
            .input_tokens,
        3
    );

    let raw = server.request();
    let split = find_subslice(&raw, b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]);
    assert!(
        head.contains("POST /v1/chat/completions HTTP/1.1"),
        "{head}"
    );
    assert!(head
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {TEST_KEY}").to_ascii_lowercase()));
    assert!(head.contains("Content-Type: application/json"));
    assert!(head.contains("Accept: text/event-stream"));
    let body: serde_json::Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
    assert_eq!(body["model"], "gpt-4o-mini");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["messages"][0]["content"], "hello");
    assert!(body.get("temperature").is_none());
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("stop").is_none());
    assert!(body.get("tools").is_none());
    server.finish();
}

#[test]
fn request_encoding_full() {
    let mut req = request("gpt-4o");
    let mut assistant = Message::assistant("answer");
    assistant.tool_calls = vec![ToolCall {
        id: "call_1".to_string(),
        name: "lookup".to_string(),
        args: serde_json::json!({"q": 1}),
    }];
    req.messages = vec![
        Message::system("sys"),
        Message::user("question"),
        assistant,
        Message::tool_result("call_1", "result"),
    ];
    req.temperature = Some(0.25);
    req.max_tokens = Some(128);
    req.stop = vec!["\n\n".to_string()];
    req.tools = vec![ToolSpec {
        name: "lookup".to_string(),
        description: "look it up".to_string(),
        schema: serde_json::json!({"type":"object","properties":{"q":{"type":"integer"}}}),
    }];

    let server = scripted(&[]);
    let client = client(&server.url());
    let mut stream = client.stream(&req).unwrap();
    let _ = drain(&mut *stream);

    let raw = server.request();
    let split = find_subslice(&raw, b"\r\n\r\n").unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
    assert_eq!(body["temperature"], 0.25);
    assert_eq!(body["max_tokens"], 128);
    assert_eq!(body["stop"][0], "\n\n");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][2]["tool_calls"][0]["type"], "function");
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"]["name"],
        "lookup"
    );
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"]["arguments"],
        "{\"q\":1}"
    );
    assert_eq!(body["messages"][3]["tool_call_id"], "call_1");
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    server.finish();
}

#[test]
fn interleaved_tool_indices_aggregate() {
    let events = [
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"one","arguments":"{\"x\":"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"two","arguments":"{\"y\":"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"2}"}}]}}]}"#,
    ];
    let server = scripted(&events);
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let mut aggregator = Aggregator::new();
    for delta in drain(&mut *stream) {
        if let Some(call) = delta.tool_call {
            aggregator.add(call);
        }
    }
    let calls = aggregator.finish().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "a");
    assert_eq!(calls[0].name, "one");
    assert_eq!(calls[0].args, serde_json::json!({"x":1}));
    assert_eq!(calls[1].id, "b");
    assert_eq!(calls[1].name, "two");
    assert_eq!(calls[1].args, serde_json::json!({"y":2}));
}

#[test]
fn arguments_256kib_beyond_default_scanner_limit() {
    let fragment = "a".repeat(256 * 1024);
    let payload = format!(
        "{{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"big\",\"function\":{{\"name\":\"f\",\"arguments\":\"{fragment}\"}}}}]}}}}]}}"
    );
    let server = scripted(&[&payload]);
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let deltas = drain(&mut *stream);
    assert_eq!(deltas.len(), 1);
    assert_eq!(
        deltas[0].tool_call.as_ref().unwrap().args_fragment.len(),
        256 * 1024
    );
}

#[test]
fn done_ignores_deltas_queued_after_it() {
    // Append trailing data after [DONE] in the raw response.
    let body = sse_body(&[r#"{"choices":[{"index":0,"delta":{"content":"kept"}}]}"#]);
    let mut body = body;
    body.extend_from_slice(
        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"leaked\"}}]}\n\n",
    );
    let response = http_response(200, &body, "", false);
    let server = raw_server(move |mut socket| {
        socket.write_all(&response).unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let deltas = drain(&mut *stream);
    assert_eq!(texts(&deltas), "kept");
    assert!(!texts(&deltas).contains("leaked"));
}

#[test]
fn comments_keepalive_crlf_and_unknown_fields() {
    let raw = b": ping\r\n\r\nevent: x\r\nid: 7\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"fine\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n";
    let server = raw_server(move |mut socket| {
        socket
            .write_all(&http_response(200, raw, "", false))
            .unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    assert_eq!(texts(&drain(&mut *stream)), "fine");
}

#[test]
fn clean_eof_without_done_is_error_and_sticky() {
    let raw = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half\"}}]}\n\n";
    let server = raw_server(move |mut socket| {
        socket
            .write_all(&http_response(200, raw, "", false))
            .unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let first = stream.recv().unwrap().unwrap();
    assert_eq!(first.text, "half");
    let err = stream.recv().unwrap_err();
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_STREAM);
    assert!(!pe.retryable);
    assert!(err.to_string().contains("[DONE]"));
    let again = stream.recv().unwrap_err();
    assert_eq!(provider_error(&again).code, CODE_STREAM);
}

#[test]
fn utf8_split_inside_emoji_across_writes() {
    let full = "hello 🚀 world";
    let bytes = full.as_bytes();
    let emoji = "🚀".as_bytes();
    let cut = bytes
        .windows(emoji.len())
        .position(|window| window == emoji)
        .unwrap()
        + 2;
    let first = bytes[..cut].to_vec();
    let second = bytes[cut..].to_vec();
    let mut payload = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"".to_vec();
    payload.extend_from_slice(&first);
    let mut body = payload.clone();
    body.extend_from_slice(&second);
    body.extend_from_slice(b"\"}}]}\n\ndata: [DONE]\n\n");
    let header = http_response(200, &body, "", false);
    let server = raw_server(move |mut socket| {
        socket.write_all(&header).unwrap();
        socket.write_all(&payload).unwrap();
        thread::sleep(Duration::from_millis(80));
        socket.write_all(&body[payload.len()..]).unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    assert_eq!(texts(&drain(&mut *stream)), full);
}

#[test]
fn multi_line_data_event_joined() {
    let raw = b"data: {\ndata: \"choices\":[{\"index\":0,\"delta\":{\"content\":\"joined\"}}]\ndata: }\n\ndata: [DONE]\n\n";
    let server = raw_server(move |mut socket| {
        socket
            .write_all(&http_response(200, raw, "", false))
            .unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    assert_eq!(texts(&drain(&mut *stream)), "joined");
}

#[test]
fn status_error_table() {
    let cases = [
        (
            500,
            "{\"error\":{\"message\":\"boom\",\"type\":\"internal_error\"}}",
            true,
            "internal_error",
        ),
        (
            429,
            "{\"error\":{\"message\":\"slow\",\"code\":\"rate_limit_error\"}}",
            true,
            "rate_limit_error",
        ),
        (
            502,
            "{\"error\":{\"message\":\"bad\",\"type\":\"server_error\"}}",
            true,
            "server_error",
        ),
        (
            400,
            "{\"error\":{\"message\":\"bad\",\"type\":\"invalid_request_error\"}}",
            false,
            "invalid_request_error",
        ),
        (
            401,
            "{\"error\":{\"message\":\"no\",\"type\":\"unauthorized\"}}",
            false,
            "unauthorized",
        ),
        (403, "", false, "forbidden"),
        (
            404,
            "{\"error\":{\"message\":\"missing\",\"type\":\"not_found\"}}",
            false,
            "not_found",
        ),
    ];
    for (status, body, retryable, code) in cases {
        let response = http_response(status, body.as_bytes(), "", false);
        let server = raw_server(move |mut socket| {
            socket.write_all(&response).unwrap();
        });
        let client = client(&server.url());
        let err = client
            .stream(&request("gpt-4o"))
            .err()
            .expect("expected error");
        let pe = provider_error(&err);
        assert_eq!(pe.status, status as i32, "status {status}");
        assert_eq!(pe.retryable, retryable, "status {status}");
        assert_eq!(pe.code, code, "status {status}");
    }
}

#[test]
fn error_body_is_clipped() {
    let body = "x".repeat(64 * 1024);
    let response = http_response(400, body.as_bytes(), "", false);
    let server = raw_server(move |mut socket| {
        socket.write_all(&response).unwrap();
    });
    let client = client(&server.url());
    let err = client
        .stream(&request("gpt-4o"))
        .err()
        .expect("expected error");
    let pe = provider_error(&err);
    assert!(pe.body.len() <= MAX_ERROR_BODY + 32);
    assert!(pe.body.ends_with("(truncated)"));
}

#[test]
fn retry_after_seconds_and_http_date() {
    assert_eq!(parse_retry_after("7"), Duration::from_secs(7));
    assert_eq!(parse_retry_after("soon-ish"), Duration::ZERO);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let future = now + Duration::from_secs(30);
    let secs = future.as_secs();
    let date = "Thu, 01 Jan 1970 00:00:00 GMT";
    assert_eq!(parse_retry_after(date), Duration::ZERO);
    // A dynamic HTTP date roughly 30 seconds out.
    let dynamic = http_date_from_epoch(secs);
    let parsed = parse_retry_after(&dynamic);
    assert!(
        parsed >= Duration::from_secs(25) && parsed <= Duration::from_secs(30),
        "{dynamic} -> {parsed:?}"
    );
}

fn http_date_from_epoch(epoch: u64) -> String {
    let total_days = (epoch / 86_400) as i64;
    let seconds = epoch % 86_400;
    let (year, month, day) = civil_from_days(total_days);
    let weekday =
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][(total_days + 4).rem_euclid(7) as usize];
    let month_name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][month as usize - 1];
    format!(
        "{weekday}, {day:02} {month_name} {year:04} {:02}:{:02}:{:02} GMT",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

#[test]
fn retry_after_header_on_429_is_retryable() {
    let response = http_response(
        429,
        b"{\"error\":{\"message\":\"slow\",\"type\":\"rate_limited\"}}",
        "Retry-After: 7\r\n",
        false,
    );
    let server = raw_server(move |mut socket| {
        socket.write_all(&response).unwrap();
    });
    let client = client(&server.url());
    let err = client
        .stream(&request("gpt-4o"))
        .err()
        .expect("expected error");
    let pe = provider_error(&err);
    assert!(pe.retryable);
    assert_eq!(pe.retry_after, Duration::from_secs(7));
}

#[test]
fn transport_error_on_closed_port() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    let client = client(&url);
    let err = client
        .stream(&request("gpt-4o"))
        .err()
        .expect("expected error");
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_TRANSPORT);
    assert!(pe.retryable);
    assert_eq!(pe.status, 0);
}

#[test]
fn close_unblocks_blocked_recv() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let _request = read_request(&mut socket);
        // Write headers with a Content-Length but no body, then stay open.
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
            .unwrap();
        thread::sleep(Duration::from_secs(2));
    });
    let client = client(&url);
    let mut stream = client.stream_open(&request("gpt-4o")).unwrap();
    let canceller = stream.canceller();
    let started = Instant::now();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        canceller.close();
    });
    let err = stream.recv().unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(provider_error(&err).code, CODE_CLOSED);
    stream.close(); // idempotent
    assert_eq!(
        provider_error(&stream.recv().unwrap_err()).code,
        CODE_CLOSED
    );
    handle.join().unwrap();
}

#[test]
fn close_is_safe_after_an_error() {
    let server = scripted(&[r#"{"error":{"message":"boom","type":"server_busy"}}"#]);
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let err = stream.recv().unwrap_err();
    assert_eq!(provider_error(&err).code, "server_busy");
    stream.close();
    assert_eq!(stream.recv().unwrap_err().to_string(), err.to_string());
}

#[test]
fn midstream_error_keeps_partial_text() {
    let events = [
        r#"{"choices":[{"index":0,"delta":{"content":"partial"}}]}"#,
        r#"{"error":{"message":"busy","type":"server_busy"}}"#,
    ];
    let server = scripted(&events);
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let first = stream.recv().unwrap().unwrap();
    assert_eq!(first.text, "partial");
    let err = stream.recv().unwrap_err();
    let pe = provider_error(&err);
    assert_eq!(pe.code, "server_busy");
    assert!(!pe.retryable);
    assert!(!err.to_string().contains(TEST_KEY));
}

#[test]
fn invalid_request_fails_before_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let client = client(&url);
    let bad = Request {
        model: String::new(),
        messages: vec![],
        tools: vec![],
        temperature: None,
        max_tokens: None,
        stop: vec![],
    };
    let err = client.stream(&bad).err().expect("expected error");
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_INVALID_REQUEST);
    assert_eq!(pe.status, 0);
    assert!(!pe.retryable);
}

#[test]
fn new_defaults_precedence_and_bad_urls() {
    let _guard = env_lock();
    std::env::remove_var("FORGE_BASE_URL");
    std::env::remove_var("FORGE_API_KEY");
    let client = Client::new(Options::default()).unwrap();
    assert_eq!(client.base_url(), "https://api.openai.com/v1");

    for bad in ["api.openai.com/v1", "ftp://example.com", "https://", "://"] {
        assert!(
            Client::new(Options {
                base_url: bad.to_string(),
                ..Default::default()
            })
            .is_err(),
            "{bad}"
        );
    }

    // Environment fallback: observe both values through a local server.
    let server = scripted(&[]);
    std::env::set_var("FORGE_BASE_URL", format!("{}/", server.url()));
    std::env::set_var("FORGE_API_KEY", "env-key");
    let client = Client::new(Options::default()).unwrap();
    assert_eq!(client.base_url(), server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let _ = drain(&mut *stream);
    let raw = server.request();
    let head = String::from_utf8_lossy(&raw[..find_subslice(&raw, b"\r\n\r\n").unwrap()]);
    assert!(head.contains("Bearer env-key"), "{head}");

    // Options override the environment, including trailing-slash trimming.
    let server2 = scripted(&[]);
    let client = Client::new(Options {
        base_url: format!("{}/", server2.url()),
        api_key: "option-key".to_string(),
        ca_file: None,
    })
    .unwrap();
    assert_eq!(client.base_url(), server2.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let _ = drain(&mut *stream);
    let raw = server2.request();
    let head = String::from_utf8_lossy(&raw[..find_subslice(&raw, b"\r\n\r\n").unwrap()]);
    assert!(head.contains("Bearer option-key"), "{head}");
    assert!(!head.contains("env-key"), "{head}");
    server.finish();
    server2.finish();

    std::env::remove_var("FORGE_BASE_URL");
    std::env::remove_var("FORGE_API_KEY");
}

#[test]
fn omits_auth_header_without_key() {
    let server = scripted(&[]);
    let client = new_client(Options {
        base_url: server.url(),
        api_key: String::new(),
        ca_file: None,
    });
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let _ = drain(&mut *stream);
    let raw = server.request();
    let head = String::from_utf8_lossy(&raw[..find_subslice(&raw, b"\r\n\r\n").unwrap()]);
    assert!(!head.to_ascii_lowercase().contains("authorization"));
    server.finish();
}

#[test]
fn errors_never_leak_the_api_key() {
    let response = http_response(401, b"{\"error\":{\"message\":\"nope\"}}", "", false);
    let server = raw_server(move |mut socket| {
        socket.write_all(&response).unwrap();
    });
    let client = client(&server.url());
    let err = client
        .stream(&request("gpt-4o"))
        .err()
        .expect("expected error");
    assert!(!err.to_string().contains(TEST_KEY));
    assert!(!provider_error(&err).body.contains(TEST_KEY));
    assert!(!format!("{:?}", provider_error(&err)).contains(TEST_KEY));
}

#[test]
fn truncated_chunked_body_is_stream_error_with_partial_text() {
    let mut raw = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    let chunk = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"half\"}}]}\n\n";
    raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
    raw.extend_from_slice(chunk);
    raw.extend_from_slice(b"\r\n");
    // Stop mid-stream: no zero chunk, socket closes.
    let server = raw_server(move |mut socket| {
        socket.write_all(&raw).unwrap();
    });
    let client = client(&server.url());
    let mut stream = client.stream(&request("gpt-4o")).unwrap();
    let first = stream.recv().unwrap().unwrap();
    assert_eq!(first.text, "half");
    let err = stream.recv().unwrap_err();
    assert_eq!(provider_error(&err).code, CODE_STREAM);
    assert_eq!(provider_error(&err).status, 0);
    assert!(!provider_error(&err).retryable);
}

// ---------------------------------------------------------------------------
// Private-CA trust store. These tests never touch a socket: they exercise the
// root-store builder directly, so they stay deterministic and offline.
// ---------------------------------------------------------------------------

/// A fixed, public self-signed CA certificate (basicConstraints CA:TRUE,
/// keyUsage keyCertSign). The matching private key was discarded; only the
/// certificate is needed to grow a trust store.
const TEST_CA_PEM: &str = "\
-----BEGIN CERTIFICATE-----
MIIC/zCCAeegAwIBAgIIEwKZJtVQYs4wDQYJKoZIhvcNAQELBQAwLTETMBEGA1UEChMKRm9yZ2Ug\
VGVzdDEWMBQGA1UEAxMNZm9yZ2UtdGVzdC1jYTAeFw0yNjExMTUxMzE1NTVaFw0zNjExMTYxMzE1\
NTVaMC0xEzARBgNVBAoTCkZvcmdlIFRlc3QxFjAUBgNVBAMTDWZvcmdlLXRlc3QtY2EwggEiMA0G\
CSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDmuD1WbHXnY0CWEbST+1S189WsFuqYXrnlriy57yv0\
2QuM3bp8D6U4SgejrTPu/YPuXk3oA/6bziuvADKhanqV6jv75MwEfaZ2+Y5JQ2jUwoCRquTbURFJ\
pketgcG4fVT4y1rTfsWOUKT9a65+FEAaellKoPbZp10MVh2yM+3xxaiQaAS5i74vCDbYd/QZyp5S\
dIYhd6EIJDOkKG1gyGzLIgxG3lwGrPTnwp75N0+RGvwov+gTiM0RrSS+tjREmJ7op8xrlYcQ8A5j\
Ul/NrGd5P/sCbKMauFxY0lrC7oCu25F1h3TKQyfmP/DetPyx4tOsl2lVf0ORNJEeK38S+iGlAgMB\
AAGjIzAhMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgEGMA0GCSqGSIb3DQEBCwUAA4IB\
AQA/5iDto4+ao19jvfUXj3wsl4NZBBLT//JhF6KrNZ6moYTw+xbD6Ay/gvDu30XMliu38VdeCMKY\
zrPXIYHkbEJQ/k8ZTyTBKasn/GEtq0MSkXLdJyf78x/ZfPWF/Rr8TMjkNbOhy6HA8mX0e2Qklrdq\
dudgfmBCdDxDWZkn86KtngcYXt3tXnyoexkme1cdmIHm/lniLxtoueYJT9uJ4xLw4trHus1XdZBz\
VjSgpWaI+pcdiCEXVH7KMOl4SfVzD+x1jsq5q5u6sLuyTmNcjWGxb1zV3KtfE3BG5f54KLvMPqY+\
ay0BY4Kmh4QFxaX/6qD1/L8xQ+Jh3ATMzgvQNjl8
-----END CERTIFICATE-----
";

/// A unique scratch directory per call, so parallel tests never collide.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("forge-llm-ca-{}-{}-{}", std::process::id(), tag, n));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn default_tls_roots_are_only_the_webpki_bundle() {
    let roots = openai::build_root_store(None).unwrap();
    assert_eq!(roots.len(), webpki_roots::TLS_SERVER_ROOTS.len());
    assert!(!roots.is_empty());
}

#[test]
fn configured_ca_is_added_to_the_public_roots() {
    let dir = unique_temp_dir("ca-ok");
    let path = dir.join("ca.pem");
    std::fs::write(&path, TEST_CA_PEM).unwrap();

    let roots = openai::build_root_store(Some(&path)).unwrap();
    // The private CA is added *on top of* the public bundle, not swapped in.
    assert_eq!(roots.len(), webpki_roots::TLS_SERVER_ROOTS.len() + 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_ca_file_is_a_typed_io_error() {
    let dir = unique_temp_dir("ca-missing");
    let path = dir.join("does-not-exist.pem");

    let err = openai::build_root_store(Some(&path)).unwrap_err();
    assert_eq!(err.kind(), forge_core::ErrorKind::Io);
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_TRANSPORT);
    assert!(!pe.retryable);
    let text = err.to_string();
    assert!(text.contains("CA file"), "{text}");
    assert!(!text.contains("-----BEGIN"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn client_new_reports_a_bad_ca_file_before_any_connection() {
    let dir = unique_temp_dir("ca-client-new");
    let path = dir.join("absent.pem");

    let err = Client::new(Options {
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: String::new(),
        ca_file: Some(path),
    })
    .err()
    .expect("expected a CA configuration error");
    assert_eq!(err.kind(), forge_core::ErrorKind::Io);
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_TRANSPORT);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_ca_pem_is_a_typed_invalid_error() {
    let dir = unique_temp_dir("ca-bad");
    let path = dir.join("bad.pem");
    std::fs::write(
        &path,
        "-----BEGIN CERTIFICATE-----\nnot base64!!!\n-----END CERTIFICATE-----\n",
    )
    .unwrap();

    let err = openai::build_root_store(Some(&path)).unwrap_err();
    assert_eq!(err.kind(), forge_core::ErrorKind::Invalid);
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_TRANSPORT);
    // The message names the file but never echoes its bytes.
    let text = err.to_string();
    assert!(
        text.contains("CA PEM") || text.contains("CA file"),
        "{text}"
    );
    assert!(!text.contains("not base64"), "{text}");
    assert!(!text.contains("-----BEGIN"), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ca_file_without_certificates_is_a_typed_invalid_error() {
    let dir = unique_temp_dir("ca-empty");
    let path = dir.join("empty.pem");
    std::fs::write(&path, "").unwrap();

    let err = openai::build_root_store(Some(&path)).unwrap_err();
    assert_eq!(err.kind(), forge_core::ErrorKind::Invalid);
    let pe = provider_error(&err);
    assert_eq!(pe.code, CODE_TRANSPORT);
    assert!(!err.to_string().contains("-----BEGIN"));

    let _ = std::fs::remove_dir_all(&dir);
}
