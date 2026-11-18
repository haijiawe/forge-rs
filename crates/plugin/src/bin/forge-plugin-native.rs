//! A native plugin: the `runtime: "exec"` path, with no interpreter in front.
//!
//! This exists for two reasons. The first is coverage: every other integration
//! test spawns Node or Python, so `Runtime::Exec` had only ever been tested as a
//! string that parses (`manifest.rs`), never as a process that answers. The
//! second is that "write it in Rust if you want it fast" is a claim about cold
//! start, and a claim nobody has run is a guess.
//!
//! It is deliberately the smallest thing that is still a real plugin: one tool
//! that succeeds, one that fails, and the four protocol rules that decide whether
//! a plugin works at all —
//!
//! 1. stdout carries protocol frames and nothing else. Diagnostics go to stderr.
//! 2. Every reply echoes the request's `id` verbatim, whatever JSON type it is.
//! 3. A tool that ran and refused is a *result* with `isError: true`. A JSON-RPC
//!    error means the call was never attempted.
//! 4. `shutdown` is answered and flushed before the process exits.
//!
//! Build it with `cargo build -p forge-plugin --bin forge-plugin-native`, then
//! point a `plugin.json` at the produced executable with `"runtime": "exec"`.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

const NAME: &str = "native";
const VERSION: &str = "0.1.0";

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut lines = stdin.lock().lines();

    while let Some(Ok(line)) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                // No id is recoverable from a line that did not parse, so the
                // spec's null is the only honest answer.
                let _ = send(
                    &mut out,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {err}") }
                    }),
                );
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // A notification has no id and must not be answered.
        let is_notification = request.get("id").is_none();
        let reply = match method {
            "initialize" => ok(&id, handshake()),
            "tools/list" => ok(&id, json!({ "tools": tools() })),
            "tools/call" => match call(&params) {
                Ok(result) => ok(&id, result),
                Err(message) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": message }
                }),
            },
            "shutdown" => {
                let _ = send(&mut out, ok(&id, json!({})));
                let _ = out.flush();
                return;
            }
            "initialized" => Value::Null,
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("no method named {other:?}") }
            }),
        };
        if is_notification || reply.is_null() {
            continue;
        }
        if send(&mut out, reply).is_err() {
            // The host closed the pipe. There is nobody left to tell.
            return;
        }
    }
}

fn send(out: &mut impl Write, frame: Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_string());
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn handshake() -> Value {
    json!({
        "protocolVersion": 1,
        "name": NAME,
        "version": VERSION,
        "capabilities": { "tools": true, "prompts": false, "hooks": [] }
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "echo",
            "description": "Return the given text byte for byte.",
            "schema": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }
        },
        {
            "name": "fail",
            "description": "Always refuses, to show that a refusal is a result.",
            "schema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "fill",
            "description": "Return a payload of the requested size, built here.",
            "schema": {
                "type": "object",
                "properties": { "bytes": { "type": "number" } },
                "required": ["bytes"]
            }
        }
    ])
}

/// A tool that ran returns `Ok`, even when it refused. A call that could not be
/// attempted returns `Err`, which the caller turns into a JSON-RPC error — the
/// distinction the protocol exists to preserve.
fn call(params: &Value) -> std::result::Result<Value, String> {
    let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    match tool {
        "echo" => Ok(match arguments.get("text").and_then(Value::as_str) {
            Some(text) => json!({ "output": text, "isError": false }),
            None => json!({
                "output": "echo needs arguments.text to be a string",
                "isError": true
            }),
        }),
        "fail" => Ok(json!({ "output": "this tool always refuses", "isError": true })),
        // Built in the child, like the Node measurement plugin, so a comparison
        // of the two is a comparison of the boundary and not of who was handed
        // the payload.
        "fill" => {
            let bytes = arguments
                .get("bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(8 << 20) as usize;
            Ok(json!({ "output": "x".repeat(bytes), "isError": false }))
        }
        other => Err(format!("no tool named {other:?}")),
    }
}
