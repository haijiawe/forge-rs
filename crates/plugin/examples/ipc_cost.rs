//! What the out-of-process plugin decision costs, measured on this host.
//!
//! ADR-0003 chose a child process speaking JSON-RPC over stdio instead of an
//! embedded scripting engine. That choice is defended with a number, and the
//! number has to come from this stack — a Node-to-Node round trip measures Node's
//! pipes, not ours, and the `durationMs` field in a tool result is millisecond
//! resolution, so every sub-millisecond call reads as 0 or 1.
//!
//! Run it with:
//!
//! ```text
//! cargo run --release -p forge-plugin --example ipc_cost --offline
//! ```
//!
//! It writes a throwaway plugin, starts it once, and then times three payload
//! sizes plus the same JSON work done in-process. The in-process row is the
//! control: it is the encode/decode both paths must do, so the difference between
//! it and the pipe rows is what the process boundary actually adds.
//!
//! Nothing here is a gate. It allocates a temp directory, needs `node` on PATH,
//! and prints a table; it does not write a baseline and does not fail a build.

use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use forge_plugin::Host;
use serde_json::{json, Value};

/// Calls per payload size. Enough that a scheduler hiccup shows up in p95 rather
/// than deciding the answer.
const CALLS: usize = 2000;
/// Warm-up calls discarded before timing: the first few pay for the child's own
/// JIT and for pipe buffers that have not reached steady state.
const WARMUP: usize = 100;
/// Startup samples for the native comparison. One sample of a 40 ms event is
/// mostly scheduler noise; the claim being tested is about the distribution.
const SPAWNS: usize = 20;

const PLUGIN_JS: &str = r#"
const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (part) => {
  buffer += part;
  let cut;
  while ((cut = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, cut);
    buffer = buffer.slice(cut + 1);
    if (line.trim()) handle(JSON.parse(line));
  }
});
function handle(req) {
  const id = req.id;
  switch (req.method) {
    case 'initialize':
      return send({ jsonrpc: '2.0', id, result: {
        protocolVersion: 1, name: 'ipc-cost', version: '0.0.0',
        capabilities: { tools: true, prompts: false, hooks: [] },
      }});
    case 'tools/list':
      return send({ jsonrpc: '2.0', id, result: { tools: [
        { name: 'echo', description: 'returns a payload of the requested size',
          inputSchema: { type: 'object', properties: { bytes: { type: 'number' } } } },
      ]}});
    case 'tools/call': {
      const bytes = Number((req.params.arguments || {}).bytes || 0);
      return send({ jsonrpc: '2.0', id, result: {
        output: bytes > 0 ? 'x'.repeat(bytes) : '', isError: false,
      }});
    }
    case 'shutdown':
      return process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result: null }) + '\n',
        () => process.exit(0));
    default:
      if (id !== undefined && id !== null) {
        return send({ jsonrpc: '2.0', id, error: { code: -32601, message: 'no method' } });
      }
  }
}
"#;

struct Row {
    label: String,
    samples: Vec<f64>,
}

impl Row {
    fn report(&self) {
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let at = |q: f64| -> f64 {
            if sorted.is_empty() {
                return 0.0;
            }
            let index = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[index]
        };
        println!(
            "{:<34} {:>9.1} {:>9.1} {:>9.1} {:>9.1}",
            self.label,
            sorted.first().copied().unwrap_or(0.0),
            at(0.50),
            at(0.95),
            sorted.last().copied().unwrap_or(0.0),
        );
    }
}

fn temp_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("forge-ipc-cost-{}-{nanos}", std::process::id()))
}

fn main() {
    let root = temp_root();
    let directory = root.join("ipc-cost");
    if let Err(err) = std::fs::create_dir_all(&directory) {
        eprintln!("ipc_cost: cannot create {}: {err}", directory.display());
        std::process::exit(1);
    }
    let manifest = json!({
        "name": "ipc-cost",
        "version": "0.0.0",
        "description": "a plugin that returns a payload of a requested size",
        "runtime": "node",
        "entry": "index.js",
    });
    for (name, body) in [
        ("plugin.json", manifest.to_string()),
        ("index.js", PLUGIN_JS.to_string()),
    ] {
        if let Err(err) = std::fs::write(directory.join(name), body) {
            eprintln!("ipc_cost: cannot write {name}: {err}");
            std::process::exit(1);
        }
    }

    let mut host = Host::start(&root);
    // The default 10 s budget is far above anything measured here; leaving it at
    // the default keeps the measurement on the shipped configuration.
    let started = Instant::now();
    let loaded = match host.load("ipc-cost") {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!(
                "ipc_cost: cannot start the plugin ({}): {}",
                err.code(),
                err.message()
            );
            let _ = std::fs::remove_dir_all(&root);
            std::process::exit(1);
        }
    };
    let spawn_us = started.elapsed().as_secs_f64() * 1e6;
    let _ = loaded;
    host.set_max_bytes(8 << 20);

    println!(
        "plugin IPC cost — {} calls per size, {} discarded",
        CALLS, WARMUP
    );
    println!(
        "host: rustc {} / {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    );
    println!(
        "spawn + handshake: {:.1} us (paid once per plugin, not per call)",
        spawn_us
    );
    println!();
    println!(
        "{:<34} {:>9} {:>9} {:>9} {:>9}",
        "measurement", "min us", "p50 us", "p95 us", "max us"
    );

    let mut rows: Vec<Row> = Vec::new();
    for (label, bytes) in [
        ("pipe: empty result", 0usize),
        ("pipe: 4 KiB result", 4 << 10),
        ("pipe: 64 KiB result", 64 << 10),
    ] {
        let args = json!({ "bytes": bytes });
        for _ in 0..WARMUP {
            if let Err(err) = host.call_tool("ipc-cost", "echo", args.clone()) {
                eprintln!("ipc_cost: warm-up call failed: {}", err.message());
                let _ = std::fs::remove_dir_all(&root);
                std::process::exit(1);
            }
        }
        let mut samples = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            let started = Instant::now();
            let outcome = host.call_tool("ipc-cost", "echo", args.clone());
            let elapsed = started.elapsed().as_secs_f64() * 1e6;
            match outcome {
                Ok(output) => {
                    if output.output.len() != bytes {
                        eprintln!(
                            "ipc_cost: asked for {bytes} bytes, got {}",
                            output.output.len()
                        );
                        let _ = std::fs::remove_dir_all(&root);
                        std::process::exit(1);
                    }
                    samples.push(elapsed);
                }
                Err(err) => {
                    eprintln!("ipc_cost: call failed: {}", err.message());
                    let _ = std::fs::remove_dir_all(&root);
                    std::process::exit(1);
                }
            }
        }
        rows.push(Row {
            label: label.to_string(),
            samples,
        });
    }

    // The control: the same encode/decode with no process boundary. This is not
    // "what an embedded engine would cost" — an engine adds its own call
    // machinery — it is the floor both designs share, so the pipe rows can be
    // read as floor + boundary.
    for (label, bytes) in [
        ("in-process: empty result", 0usize),
        ("in-process: 4 KiB result", 4 << 10),
        ("in-process: 64 KiB result", 64 << 10),
    ] {
        let payload = "x".repeat(bytes);
        let response =
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "output": payload, "isError": false } });
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "bytes": bytes } } });
        let mut samples = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            let started = Instant::now();
            let wire = serde_json::to_vec(&request).expect("serialize");
            let parsed: Value = serde_json::from_slice(&wire).expect("parse");
            let encoded = serde_json::to_vec(&response).expect("serialize");
            let decoded: Value = serde_json::from_slice(&encoded).expect("parse");
            let elapsed = started.elapsed().as_secs_f64() * 1e6;
            // Touch the results so the optimiser cannot delete the work.
            if parsed["method"] != "tools/call"
                || decoded["result"]["output"].as_str().map(str::len) != Some(bytes)
            {
                eprintln!("ipc_cost: the control did not do the work it claims");
                std::process::exit(1);
            }
            samples.push(elapsed);
        }
        rows.push(Row {
            label: label.to_string(),
            samples,
        });
    }

    for row in &rows {
        row.report();
    }
    println!();
    println!("For scale: one model round trip on this project measured 1,203,000 us.");
    host.unload_all();
    let _ = std::fs::remove_dir_all(&root);

    native_comparison();
}

/// The same measurement against a native plugin, so "write it in Rust if you
/// want it fast" is a number rather than a slogan.
///
/// The headline is startup, not the call: an interpreter costs tens of
/// milliseconds to begin, a native binary does not. Per-call work is dominated by
/// the pipe both share, so it should barely move — and if it does move, that is
/// worth seeing rather than assuming.
fn native_comparison() {
    let Some(exe) = native_plugin_path() else {
        println!();
        println!("native comparison skipped: build it first with");
        println!("  cargo build --release -p forge-plugin --bin forge-plugin-native");
        return;
    };
    let root = std::env::temp_dir().join(format!("forge-ipc-native-{}", std::process::id()));
    let plugin = root.join("native");
    if std::fs::create_dir_all(&plugin).is_err() {
        return;
    }
    let entry = if cfg!(windows) {
        "native.exe"
    } else {
        "native"
    };
    if std::fs::copy(&exe, plugin.join(entry)).is_err() {
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    let manifest = json!({
        "name": "native", "version": "0.0.0", "description": "native measurement plugin",
        "runtime": "exec", "entry": entry,
    });
    if std::fs::write(
        plugin.join("plugin.json"),
        serde_json::to_string(&manifest).unwrap_or_default(),
    )
    .is_err()
    {
        let _ = std::fs::remove_dir_all(&root);
        return;
    }

    println!();
    println!("--- native plugin (runtime: exec), same host, same pipe ---");

    // Startup is sampled repeatedly: it is the number that differs, and one
    // sample of a 40 ms event is mostly scheduler noise.
    let mut spawns = Vec::new();
    let mut host = Host::start(&root);
    host.set_max_bytes(8 << 20);
    for _ in 0..SPAWNS {
        let started = Instant::now();
        if host.load("native").is_err() {
            eprintln!("ipc_cost: cannot load the native plugin");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        spawns.push(started.elapsed().as_secs_f64() * 1e6);
        host.unload_all();
    }
    Row {
        label: "native: spawn + handshake".to_string(),
        samples: spawns,
    }
    .report();

    if host.load("native").is_err() {
        let _ = std::fs::remove_dir_all(&root);
        return;
    }
    for (label, bytes) in [
        ("native: empty result", 0usize),
        ("native: 4 KiB result", 4 << 10),
        ("native: 64 KiB result", 64 << 10),
    ] {
        let args = json!({ "bytes": bytes });
        for _ in 0..WARMUP {
            if host.call_tool("native", "fill", args.clone()).is_err() {
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
        }
        let mut samples = Vec::with_capacity(CALLS);
        for _ in 0..CALLS {
            let started = Instant::now();
            let outcome = host.call_tool("native", "fill", args.clone());
            let elapsed = started.elapsed().as_secs_f64() * 1e6;
            match outcome {
                Ok(output) if output.output.len() == bytes => samples.push(elapsed),
                _ => {
                    eprintln!("ipc_cost: native call did not return {bytes} bytes");
                    let _ = std::fs::remove_dir_all(&root);
                    return;
                }
            }
        }
        Row {
            label: label.to_string(),
            samples,
        }
        .report();
    }
    host.unload_all();
    let _ = std::fs::remove_dir_all(&root);
}

/// The native plugin sits beside this example's own executable in the cargo
/// output directory. `CARGO_BIN_EXE_*` is only set for integration tests, so a
/// measurement example has to find it the way a person would.
fn native_plugin_path() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let profile = current.parent()?.parent()?;
    let name = if cfg!(windows) {
        "forge-plugin-native.exe"
    } else {
        "forge-plugin-native"
    };
    let candidate = profile.join(name);
    candidate.is_file().then_some(candidate)
}
