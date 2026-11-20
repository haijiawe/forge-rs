//! Behaviour of the two network tools against a real loopback server.
//!
//! The point is not that HTML is stripped — the unit tests in `hearth-net` cover
//! that — but that the tool builds a request a server answers, reports a
//! non-success as data, and turns a broken search endpoint into a code the
//! model can act on instead of a panic or a lie.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use hearth_tool::{Call, SearchConfig, Tool, WebFetchTool, WebSearchTool, WEB_CODE_BAD_CONTENT};
use serde_json::json;

fn serve<F>(responses: usize, handler: F) -> String
where
    F: Fn(usize, &str) -> Vec<u8> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    let handler = Arc::new(handler);
    thread::spawn(move || {
        for index in 0..responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let head = read_head(&mut stream);
            let bytes = handler(index, &head);
            let _ = stream.write_all(&bytes);
            let _ = stream.flush();
        }
    });
    format!("http://{address}")
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    // Drain a JSON body so the client is not blocked writing it.
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));
                    let mut sink = Vec::new();
                    let _ = stream.read_to_end(&mut sink);
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

fn response(status: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    out.push_str(body);
    out.into_bytes()
}

fn client() -> Arc<hearth_net::Client> {
    Arc::new(hearth_net::Client::new(hearth_net::ClientOptions::default()).expect("client"))
}

fn call(name: &str, args: serde_json::Value) -> Call {
    Call::new("c1", name, args, ".")
}

#[test]
fn fetch_turns_a_page_into_text_and_reports_the_status() {
    let base = serve(1, |_, _| {
        response(
            "200 OK",
            &[("Content-Type", "text/html; charset=utf-8")],
            "<html><head><title>t</title><style>p{}</style></head>\
             <body><h1>Heading</h1><p>first&amp;second</p><script>x()</script></body></html>",
        )
    });
    let tool = WebFetchTool::new(hearth_tool::DefaultLimits, client());
    let result = tool.call(call("web_fetch", json!({ "url": base })));
    assert!(!result.is_error, "{}", result.output);
    assert!(result.output.starts_with("200 "), "{}", result.output);
    assert!(result.output.contains("Heading"), "{}", result.output);
    assert!(result.output.contains("first&second"), "{}", result.output);
    assert!(!result.output.contains("x()"), "the script must be dropped");
    assert_eq!(result.meta.get("status"), Some(&json!(200)));
}

#[test]
fn fetch_returns_a_non_success_status_as_data() {
    let base = serve(1, |_, _| {
        response("404 Not Found", &[("Content-Type", "text/plain")], "gone")
    });
    let tool = WebFetchTool::new(hearth_tool::DefaultLimits, client());
    let result = tool.call(call("web_fetch", json!({ "url": base })));
    // A missing page is a fact about the page, not a broken tool.
    assert!(!result.is_error, "{}", result.output);
    assert!(result.output.starts_with("404 "), "{}", result.output);
    assert!(result.output.contains("gone"));
}

#[test]
fn fetch_refuses_a_url_that_is_not_http() {
    let tool = WebFetchTool::new(hearth_tool::DefaultLimits, client());
    let result = tool.call(call("web_fetch", json!({ "url": "file:///etc/passwd" })));
    assert!(result.is_error);
    assert_eq!(result.code, hearth_tool::ERR_BAD_ARGS);
}

#[test]
fn search_parses_a_results_list_and_sends_the_key() {
    let base = serve(1, |_, head| {
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer k3y"),
            "the key must travel in a header, not the URL: {head}"
        );
        assert!(head.starts_with("POST "), "got {head}");
        response(
            "200 OK",
            &[("Content-Type", "application/json")],
            r#"{"results":[{"title":"A","url":"https://a.example","snippet":"aa"},
                           {"name":"B","link":"https://b.example","description":"bb"}]}"#,
        )
    });
    let tool = WebSearchTool::new(
        hearth_tool::DefaultLimits,
        client(),
        SearchConfig {
            endpoint: format!("{base}/search"),
            key: "k3y".to_string(),
        },
    );
    let result = tool.call(call("web_search", json!({ "query": "anything" })));
    assert!(!result.is_error, "{}", result.output);
    assert!(
        result.output.contains("https://a.example"),
        "{}",
        result.output
    );
    // The second hit uses the other field spellings; a parser that only knows
    // one shape silently returns half the answer.
    assert!(
        result.output.contains("https://b.example"),
        "{}",
        result.output
    );
    assert!(result.output.contains("bb"));
}

#[test]
fn search_reports_an_endpoint_that_is_not_json() {
    let base = serve(1, |_, _| {
        response(
            "200 OK",
            &[("Content-Type", "text/html")],
            "<html>nope</html>",
        )
    });
    let tool = WebSearchTool::new(
        hearth_tool::DefaultLimits,
        client(),
        SearchConfig {
            endpoint: format!("{base}/search"),
            key: String::new(),
        },
    );
    let result = tool.call(call("web_search", json!({ "query": "anything" })));
    assert!(result.is_error);
    assert_eq!(result.code, WEB_CODE_BAD_CONTENT);
    assert!(
        !result.output.is_empty(),
        "the model must see what came back"
    );
}

#[test]
fn search_turns_an_error_status_into_an_error_result() {
    let base = serve(1, |_, _| {
        response("429 Too Many Requests", &[], "slow down")
    });
    let tool = WebSearchTool::new(
        hearth_tool::DefaultLimits,
        client(),
        SearchConfig {
            endpoint: format!("{base}/search"),
            key: String::new(),
        },
    );
    let result = tool.call(call("web_search", json!({ "query": "anything" })));
    assert!(result.is_error);
    assert_eq!(result.code, "rate_limited");
    assert!(result.output.contains("429"), "{}", result.output);
}
