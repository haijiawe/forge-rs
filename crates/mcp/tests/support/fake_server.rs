//! Test-only MCP server. Its mode is passed on the command line so the
//! environment tests can clear the child environment without losing it.

use std::fs;
use std::io::{self, BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

fn send(out: &mut impl Write, value: &Value) {
    let _ = serde_json::to_writer(&mut *out, value);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

fn result(out: &mut impl Write, id: &Value, value: Value) {
    send(out, &json!({"jsonrpc":"2.0","id":id,"result":value}));
}

fn text_result(text: String) -> Value {
    json!({"content":[{"type":"text","text":text}]})
}

fn error(out: &mut impl Write, id: &Value, code: i64, message: impl AsRef<str>) {
    let message = message.as_ref();
    send(
        out,
        &json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    );
}

fn stderr(text: &str) {
    eprintln!("{text}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = String::new();
    let mut marker = String::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--mode" if index + 1 < args.len() => {
                mode = args[index + 1].clone();
                index += 2;
            }
            "--marker" if index + 1 < args.len() => {
                marker = args[index + 1].clone();
                index += 2;
            }
            _ => index += 1,
        }
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut captured = String::new();

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).unwrap_or(0);
        if read == 0 {
            if mode.contains("ignoreeof") {
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if !marker.is_empty() {
                let _ = fs::write(&marker, b"closed");
            }
            return;
        }
        let Ok(request) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        match method.as_str() {
            "initialize" => {
                if mode.contains("badversion") {
                    result(
                        &mut out,
                        &id,
                        json!({
                            "protocolVersion":"1999-01-01",
                            "serverInfo":{"name":"fake","version":"1"}
                        }),
                    );
                } else if mode.contains("initrefuse") {
                    error(&mut out, &id, -32_000, "initialize refused");
                } else {
                    result(
                        &mut out,
                        &id,
                        json!({
                            "protocolVersion":"2025-06-18",
                            "serverInfo":{"name":"fake","version":"1.2.3"}
                        }),
                    );
                }
            }
            "notifications/initialized" | "notifications/cancelled" => {}
            "tools/list" => {
                let cursor = request
                    .get("params")
                    .and_then(|p| p.get("cursor"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if mode.contains("paged") && cursor.is_empty() {
                    result(
                        &mut out,
                        &id,
                        json!({
                            "tools":[
                                {"name":"echo","description":"echo","inputSchema":{"type":"object"}},
                                {"name":"env","description":"env","inputSchema":{"type":"object"}}
                            ],
                            "nextCursor":"page-2"
                        }),
                    );
                } else if mode.contains("paged") {
                    result(
                        &mut out,
                        &id,
                        json!({
                            "tools":[{"name":"hello","description":"second page","inputSchema":{"type":"object"}}],
                            "nextCursor":""
                        }),
                    );
                } else {
                    result(
                        &mut out,
                        &id,
                        json!({
                            "tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]
                        }),
                    );
                }
            }
            "tools/call" => {
                let params = request.get("params").cloned().unwrap_or(Value::Null);
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                stderr(&format!("tool:{name}"));
                if mode.contains("noise") {
                    stderr("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/noise\"}");
                    stderr("random stderr text that is not JSON at all");
                }
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                match name.as_str() {
                    "echo" => result(&mut out, &id, text_result(arguments.to_string())),
                    "env" => {
                        let mut entries: Vec<String> = std::env::vars()
                            .map(|(key, value)| format!("{key}={value}"))
                            .collect();
                        entries.sort();
                        result(&mut out, &id, text_result(entries.join("\n")));
                    }
                    "boom" => error(&mut out, &id, -32_601, "unknown tool: boom"),
                    "hang" => {
                        // Long enough for a short per-call deadline to expire, short
                        // enough that the single-threaded test server can serve the
                        // next call without making that deadline flaky.
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    "die" => std::process::exit(7),
                    "malformed" => {
                        let _ = out.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":");
                        let _ = out.flush();
                        return;
                    }
                    "oversized" => {
                        let mut huge = vec![b'x'; (8 << 20) + 1024];
                        huge.push(b'\n');
                        let _ = out.write_all(&huge);
                        let _ = out.flush();
                        return;
                    }
                    "notify" => {
                        send(
                            &mut out,
                            &json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"hi"}}),
                        );
                        result(&mut out, &id, text_result("after-notification".to_string()));
                    }
                    "serverrequest" => {
                        send(
                            &mut out,
                            &json!({"jsonrpc":"2.0","id":999,"method":"roots/list"}),
                        );
                        result(&mut out, &id, text_result("after-request".to_string()));
                    }
                    "captured" => result(&mut out, &id, text_result(captured.clone())),
                    "iserror" => result(
                        &mut out,
                        &id,
                        json!({
                            "content":[{"type":"text","text":"tool failed"}],
                            "isError":true
                        }),
                    ),
                    "image" => result(
                        &mut out,
                        &id,
                        json!({
                            "content":[{"type":"image","data":"aGk=","mimeType":"image/png"}]
                        }),
                    ),
                    _ => error(&mut out, &id, -32_601, format!("unknown tool: {name}")),
                }
            }
            "" if !id.is_null() => {
                // The client's answer to a server-initiated request.
                captured = line.trim().to_string();
            }
            other => {
                if !id.is_null() {
                    error(&mut out, &id, -32_601, format!("method not found: {other}"));
                }
            }
        }
        let _ = out.flush();
    }
}
