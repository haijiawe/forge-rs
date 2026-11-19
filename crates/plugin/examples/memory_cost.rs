//! What a plugin process costs in RAM, which is a different question from latency.
//!
//! `ipc_cost` answers "how long does a call take" and the answer is small enough
//! to ignore next to a model round trip. That number says nothing about memory,
//! and memory is the property this whole rewrite was sold on: `forge web` idles at
//! a few MB where the Electron original held several GB. An interpreter per
//! plugin could quietly spend that advantage, so it has to be measured rather
//! than assumed in either direction.
//!
//! Run it with:
//!
//! ```text
//! cargo build --release -p forge-plugin --bins --offline
//! cargo run --release -p forge-plugin --example memory_cost --offline -- 40
//! ```
//!
//! The argument is how many seconds to hold the children open (default 30). While
//! it holds, sample the working set of this process and its children — one method
//! for every runtime, so the comparison is fair:
//!
//! ```powershell
//! $p = <pid printed below>
//! Get-CimInstance Win32_Process -Filter "ParentProcessId=$p" |
//!   Select-Object Name, ProcessId, WorkingSetSize
//! (Get-Process -Id $p).WorkingSet64
//! ```
//!
//! On Linux the same rows come from `ps -o rss= --ppid <pid>`.
//!
//! Node also reports its own numbers through a `mem` tool, which cross-checks the
//! OS view and splits the total into heap and external. Python and the native
//! binary have no equivalent one-liner, so the OS row is the shared measurement
//! and the self-report is only a check on it.
//!
//! Nothing here is a gate: it needs `node`, `python` and a built
//! `forge-plugin-native`, it skips whatever is missing rather than pretending,
//! and it neither writes a baseline nor fails a build.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use forge_plugin::Host;
use serde_json::{json, Value};

/// Long enough to sample by hand from another shell, short enough that a forgotten
/// run does not sit on the machine.
const DEFAULT_HOLD_SECS: u64 = 30;

/// A Node plugin that can describe its own memory. `process.memoryUsage()` is the
/// only self-report any of the three runtimes offers in one call.
const NODE_ENTRY: &str = r#"
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
        protocolVersion: 1, name: 'mem-node', version: '0.0.0',
        capabilities: { tools: true, prompts: false, hooks: [] },
      }});
    case 'tools/list':
      return send({ jsonrpc: '2.0', id, result: { tools: [
        { name: 'mem', description: 'reports this process memory usage',
          schema: { type: 'object', properties: {} } },
      ]}});
    case 'tools/call':
      return send({ jsonrpc: '2.0', id, result: {
        output: JSON.stringify(process.memoryUsage()), isError: false,
      }});
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

/// A Python plugin that does nothing but stay alive. Its memory is the interpreter's
/// floor, which is exactly the number being asked for.
const PYTHON_ENTRY: &str = r#"
import json
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    rid = req.get("id")
    method = req.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": 1, "name": "mem-python", "version": "0.0.0",
            "capabilities": {"tools": True, "prompts": False, "hooks": []}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": "hello", "description": "answers with a fixed string",
             "schema": {"type": "object", "properties": {}}}]}})
    elif method == "tools/call":
        send({"jsonrpc": "2.0", "id": rid,
              "result": {"output": "hello from python", "isError": False}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": rid, "result": None})
        break
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid,
              "error": {"code": -32601, "message": "no method"}})
"#;

fn temp_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("forge-memory-cost-{}-{nanos}", std::process::id()))
}

fn write_plugin(root: &Path, name: &str, runtime: &str, entry: &str, body: &str) -> bool {
    let directory = root.join(name);
    if std::fs::create_dir_all(&directory).is_err() {
        return false;
    }
    let manifest = json!({
        "name": name,
        "version": "0.0.0",
        "description": "a plugin held open so its memory can be sampled",
        "runtime": runtime,
        "entry": entry,
    });
    std::fs::write(directory.join("plugin.json"), manifest.to_string()).is_ok()
        && std::fs::write(directory.join(entry), body).is_ok()
}

/// The native binary sits beside this example in the same profile directory. An
/// example cannot use `CARGO_BIN_EXE_*`, which is only set for integration tests.
fn native_binary() -> Option<PathBuf> {
    let here = std::env::current_exe().ok()?;
    let profile = here.parent()?.parent()?;
    let name = if cfg!(windows) {
        "forge-plugin-native.exe"
    } else {
        "forge-plugin-native"
    };
    let candidate = profile.join(name);
    candidate.is_file().then_some(candidate)
}

fn interpreter_works(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn main() {
    let hold = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_HOLD_SECS);
    // How many Node plugins to hold at once. The marginal cost of the second one
    // is what decides whether several plugins should share one interpreter: if it
    // is another full floor, multiplexing saves that floor per plugin, and if it
    // is small there is nothing to save.
    let node_count = std::env::args()
        .nth(2)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 8);

    let root = temp_root();
    if std::fs::create_dir_all(&root).is_err() {
        eprintln!("memory_cost: cannot create {}", root.display());
        std::process::exit(1);
    }

    let mut wanted: Vec<String> = Vec::new();
    if interpreter_works("node") {
        for index in 1..=node_count {
            let name = if index == 1 {
                "mem-node".to_string()
            } else {
                format!("mem-node-{index}")
            };
            if write_plugin(&root, &name, "node", "index.js", NODE_ENTRY) {
                wanted.push(name);
            }
        }
    } else {
        println!("node: not on PATH, skipped");
    }
    if interpreter_works("python") {
        if write_plugin(&root, "mem-python", "python", "main.py", PYTHON_ENTRY) {
            wanted.push("mem-python".to_string());
        }
    } else {
        println!("python: not on PATH, skipped");
    }
    match native_binary() {
        Some(binary) => {
            let directory = root.join("mem-native");
            let copied = directory.join("native");
            let ok = std::fs::create_dir_all(&directory).is_ok()
                && std::fs::copy(&binary, &copied).is_ok()
                && std::fs::write(
                    directory.join("plugin.json"),
                    json!({
                        "name": "mem-native",
                        "version": "0.0.0",
                        "description": "a native plugin held open for sampling",
                        "runtime": "exec",
                        "entry": "native",
                    })
                    .to_string(),
                )
                .is_ok();
            if ok {
                wanted.push("mem-native".to_string());
            }
        }
        None => println!(
            "native: build it first with `cargo build --release -p forge-plugin --bins`, skipped"
        ),
    }

    if wanted.is_empty() {
        eprintln!("memory_cost: nothing to measure");
        let _ = std::fs::remove_dir_all(&root);
        std::process::exit(1);
    }

    let host = Host::start(&root);
    let mut loaded: Vec<String> = Vec::new();
    for name in &wanted {
        match host.load(name.as_str()) {
            Ok(_) => loaded.push(name.clone()),
            Err(err) => println!(
                "{name}: could not start ({}): {}",
                err.code(),
                err.message()
            ),
        }
    }

    println!();
    println!("host pid   {}", std::process::id());
    println!("plugins    {}", loaded.join(", "));
    println!("root       {}", root.display());
    println!();

    // The self-report is a cross-check on the OS row, not a replacement for it:
    // rss here is Node's own accounting, which does not include everything the
    // kernel charges to the process.
    if loaded.iter().any(|name| name == "mem-node") {
        match host.call_tool("mem-node", "mem", json!({})) {
            Ok(output) => match serde_json::from_str::<Value>(&output.output) {
                Ok(usage) => {
                    let mb = |key: &str| -> f64 {
                        usage.get(key).and_then(Value::as_f64).unwrap_or(0.0) / (1024.0 * 1024.0)
                    };
                    println!(
                        "node self-report   rss {:.1} MB   heapTotal {:.1} MB   heapUsed {:.1} MB   external {:.1} MB",
                        mb("rss"),
                        mb("heapTotal"),
                        mb("heapUsed"),
                        mb("external"),
                    );
                }
                Err(err) => println!("node self-report   unparseable: {err}"),
            },
            Err(err) => println!("node self-report   call failed: {}", err.message()),
        }
    }

    println!();
    println!(
        "holding for {hold}s — sample the working set of pid {} and its children now",
        std::process::id()
    );
    std::thread::sleep(Duration::from_secs(hold));

    host.unload_all();
    let _ = std::fs::remove_dir_all(&root);
    println!("done");
}
