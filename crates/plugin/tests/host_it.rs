//! The host, tested against real child processes.
//!
//! These tests spawn Node and Python. A machine without them skips those cases
//! with a printed reason rather than passing silently: the host must be testable
//! on a bare machine (it is a Rust crate, not a Node wrapper), but CI without
//! Node must not look green by omission — a skip is visible in the test output,
//! and the tests that need no runtime still run everywhere.

use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_plugin::{Host, Kind, Stopped};
use serde_json::json;

/// The directory the reference plugin lives in, at build time.
fn reference_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference-node")
}

/// A plugin root in the temp directory, removed when the test ends.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Scratch {
        let dir = std::env::temp_dir()
            .join("forge-plugin-it")
            .join(format!("{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the plugin root");
        Scratch { dir }
    }

    fn host(&self) -> Host {
        Host::start(&self.dir)
    }

    /// Install a plugin directory with the given manifest and entry files.
    fn install(&self, directory: &str, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
        let plugin = self.dir.join(directory);
        std::fs::create_dir_all(&plugin).expect("create the plugin directory");
        std::fs::write(plugin.join("plugin.json"), manifest).expect("write the manifest");
        for (name, body) in files {
            std::fs::write(plugin.join(name), body).expect("write a plugin file");
        }
        plugin
    }

    /// Copy the reference plugin in under the directory its manifest names.
    fn install_reference(&self) -> PathBuf {
        let destination = self.dir.join("reference");
        copy_tree(&reference_root(), &destination);
        destination
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the fixture directory");
    for entry in std::fs::read_dir(from).expect("read the fixture directory") {
        let entry = entry.expect("fixture entry");
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy a fixture file");
        }
    }
}

fn runtime_available(command: &str) -> bool {
    std::process::Command::new(command)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn node_available() -> bool {
    runtime_available("node")
}

fn python_available() -> bool {
    runtime_available("python")
}

/// Whether this Node can execute `.ts` without a flag or a build.
///
/// Type stripping arrived unflagged in Node 23.6. Below that the entry would
/// need `--experimental-strip-types`, which the host does not pass, so the case
/// is skipped rather than asserted — the version is a property of the machine.
fn node_strips_types() -> bool {
    let output = match std::process::Command::new("node").arg("--version").output() {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let major: u32 = text
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|part| part.parse().ok())
        .unwrap_or(0);
    major >= 23
}

/// Announce why a case did not run. `eprintln!` so `--nocapture` shows it and so
/// a CI log keeps the reason next to the test name.
fn skip(reason: &str) {
    eprintln!("SKIPPED: {reason}");
}

const REFERENCE_MANIFEST: &str = include_str!("reference-node/plugin.json");

// ---------------------------------------------------------------------------
// The reference plugin: the whole host surface, end to end.
// ---------------------------------------------------------------------------

#[test]
fn the_reference_plugin_initializes_and_lists_four_tools_with_their_schemas() {
    if !node_available() {
        skip("node is not on PATH, so the reference plugin cannot run");
        return;
    }
    let scratch = Scratch::new("list");
    scratch.install_reference();
    let host = scratch.host();

    let loaded = host.load("reference").expect("initialize");
    assert_eq!(loaded.handshake.name, "reference");
    assert_eq!(loaded.handshake.version, "1.0.0");
    assert_eq!(loaded.handshake.protocol_version, 1);
    assert!(loaded.handshake.capabilities.tools);
    assert!(loaded.handshake.capabilities.prompts);
    assert!(loaded.handshake.capabilities.runs_hook("before_turn"));
    // A phase it did not claim is not available: the capability list decides.
    assert!(!loaded.handshake.capabilities.runs_hook("after_tool"));

    let tools = host.tools("reference").expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["add", "big", "echo", "fail"]);

    // The schema comes through as written, not re-invented: a host that rewrote
    // a schema would describe a different tool than the one it calls.
    let echo = tools.iter().find(|tool| tool.name == "echo").expect("echo");
    assert_eq!(echo.description, "Return the text it was given, unchanged.");
    assert_eq!(echo.schema["required"], json!(["text"]));
    assert_eq!(echo.schema["properties"]["text"]["type"], json!("string"));

    let add = tools.iter().find(|tool| tool.name == "add").expect("add");
    assert_eq!(add.schema["required"], json!(["a", "b"]));

    let prompts = host.prompts("reference").expect("prompts/list");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "greeting");
    let text = host
        .prompt_text("reference", "greeting")
        .expect("prompts/get");
    assert!(text.contains("reference plugin"), "{text}");

    let stopped = host
        .unload("reference")
        .expect("unload")
        .expect("it was loaded");
    assert_eq!(stopped, assert_clean(&stopped));
    assert!(host.loaded_names().is_empty());
}

/// Assert a plugin stopped on its own and successfully, and hand it back for the
/// fields a caller might still want.
fn assert_clean(stopped: &Stopped) -> Stopped {
    assert!(
        stopped.exited,
        "the child had to be killed: {}",
        stopped.detail
    );
    assert!(
        stopped.clean,
        "the child exited uncleanly: {}",
        stopped.detail
    );
    assert_eq!(stopped.code, Some(0), "{}", stopped.detail);
    stopped.clone()
}

#[test]
fn a_tool_call_returns_the_exact_bytes_the_plugin_wrote() {
    if !node_available() {
        skip("node is not on PATH, so the reference plugin cannot run");
        return;
    }
    let scratch = Scratch::new("call");
    scratch.install_reference();
    let host = scratch.host();
    host.load("reference").expect("initialize");

    let echo = host
        .call_tool("reference", "echo", json!({ "text": "  spaced  " }))
        .expect("echo");
    // Verbatim, including the spaces and the case: a host that trimmed or
    // normalised is a host that changes what the model sees.
    assert_eq!(echo.output, "  spaced  ");
    assert!(!echo.is_error);

    let unicode = host
        .call_tool("reference", "echo", json!({ "text": "插件\nline two\t" }))
        .expect("echo non-ASCII");
    assert_eq!(unicode.output, "插件\nline two\t");

    let add = host
        .call_tool("reference", "add", json!({ "a": 2, "b": 40 }))
        .expect("add");
    assert_eq!(add.output, "42");
    assert!(!add.is_error);
    // The number came back as text, which is what the protocol says a tool
    // returns: one output string, because that is what the model reads.
    assert_eq!(add.output.parse::<i64>().expect("a number"), 42);

    let failed = host
        .call_tool("reference", "fail", json!({}))
        .expect("a failing tool is a result, not a Rust error");
    assert!(failed.is_error);
    assert_eq!(
        failed.output,
        "the fail tool always fails: this message comes from the plugin"
    );

    let big = host
        .call_tool("reference", "big", json!({}))
        .expect("1 MiB arrives whole");
    assert_eq!(big.output.len(), 1 << 20, "the body was truncated");
    let unit = "0123456789abcdef";
    assert_eq!(big.output.len(), unit.len() * (1 << 20) / unit.len());
    assert!(
        big.output.chars().all(|ch| unit.contains(ch)),
        "the body does not consist of the unit the plugin wrote"
    );
    assert!(
        big.output.starts_with(unit.repeat(4).as_str()),
        "the body did not start where the plugin started"
    );
    assert!(
        big.output.ends_with(unit.repeat(4).as_str()),
        "the body did not end where the plugin ended"
    );

    host.unload("reference").expect("unload");
}

#[test]
fn a_hook_may_modify_the_payload_and_a_phase_it_does_not_claim_is_untouched() {
    if !node_available() {
        skip("node is not on PATH, so the reference plugin cannot run");
        return;
    }
    let scratch = Scratch::new("hooks");
    scratch.install_reference();
    let host = scratch.host();
    host.load("reference").expect("initialize");

    let payload = json!({ "text": "hello" });
    let modified = host
        .run_hook("reference", "before_turn", payload.clone())
        .expect("before_turn");
    // The plugin's change came back: the reference plugin appends a newline, so
    // the result is exactly that and nothing else was invented.
    assert_eq!(modified, json!({ "text": "hello\n" }));

    // A phase the plugin does not claim is returned untouched **without a round
    // trip**: asking would make its capability list a suggestion.
    let untouched = host
        .run_hook("reference", "after_tool", payload.clone())
        .expect("after_tool is not claimed");
    assert_eq!(untouched, payload);

    host.unload("reference").expect("unload");
}

// ---------------------------------------------------------------------------
// Failure modes, each with the name the operator acts on.
// ---------------------------------------------------------------------------

#[test]
fn a_call_past_the_deadline_is_killed_and_the_next_call_says_the_child_is_gone() {
    if !node_available() {
        skip("node is not on PATH, so no plugin can hang");
        return;
    }
    let scratch = Scratch::new("timeout");
    let slow = include_str!("reference-node/slow.js");
    scratch.install(
        "slow",
        r#"{"name":"slow","version":"1.0.0","description":"never answers a tool call","runtime":"node","entry":"slow.js"}"#,
        &[("slow.js", slow)],
    );

    let mut host = scratch.host();
    host.set_timeout(Duration::from_millis(200));
    // It introduces itself fine; only tool calls hang. A plugin that hung during
    // `initialize` would be a different test (`load` refusing), and both are
    // covered by the same code path in `Connection::round_trip`.
    host.load("slow").expect("the slow plugin initializes");

    let started = std::time::Instant::now();
    let error = host.tools("slow").expect_err("no reply within 200 ms");
    assert_eq!(error.kind(), Kind::Timeout, "{}", error.message());
    assert_eq!(error.code(), "timeout");
    assert!(error.message().contains("200 ms"), "{}", error.message());
    assert!(error.message().contains("killed"), "{}", error.message());
    // Killed, not merely abandoned: the deadline is honoured, not overshot by
    // waiting for a child that will never answer.
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the timeout took {:?}, so the child was waited for instead of killed",
        started.elapsed()
    );

    // The child is gone, and the next call says so immediately rather than
    // hanging for another deadline.
    let again = host.tools("slow").expect_err("the child is dead");
    assert_eq!(again.kind(), Kind::ChildExited, "{}", again.message());
    assert_eq!(again.code(), "child_exited");

    // Unloading a plugin whose child died is not an error; it just reports that
    // it had to kill something already dead.
    let stopped = host
        .unload("slow")
        .expect("unload after a death")
        .expect("was loaded");
    assert!(
        !stopped.clean,
        "a killed child must not be reported as clean"
    );
}

#[test]
fn three_broken_manifests_produce_three_different_codes() {
    let scratch = Scratch::new("manifests");
    scratch.install(
        "unknown-runtime",
        r#"{"name":"unknown-runtime","version":"1.0.0","description":"d","runtime":"deno","entry":"index.ts"}"#,
        &[("index.ts", "// body")],
    );
    scratch.install(
        "bad-name",
        r#"{"name":"Bad Name","version":"1.0.0","description":"d","runtime":"node","entry":"index.js"}"#,
        &[("index.js", "// body")],
    );
    scratch.install(
        "no-entry",
        r#"{"name":"no-entry","version":"1.0.0","description":"d","runtime":"node"}"#,
        &[("index.js", "// body")],
    );

    let host = scratch.host();
    let (found, broken) = host.discover();
    // The no-entry manifest is legible and therefore discovered; it fails when a
    // start is attempted, because there is nothing to run. The other two cannot
    // even be discovered, which is why `discover` has a `broken` list at all.
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "no-entry");
    assert_eq!(broken.len(), 2, "{broken:?}");

    // Three refusals, three codes, because each sends a different person to a
    // different place: the author to their identity, the author to their
    // "runtime" line, the operator to "this plugin names nothing to run".
    let mut codes: Vec<&'static str> = broken.iter().map(|(_, error)| error.code()).collect();
    codes.sort_unstable();
    assert_eq!(codes, vec!["bad_manifest", "unsupported_runtime"]);

    // Loading each by name reaches the same refusal, not "unknown plugin": the
    // operator installed exactly this directory.
    let refused = [
        host.load("unknown-runtime").expect_err("deno"),
        host.load("bad-name").expect_err("bad name"),
        host.load("no-entry").expect_err("no entry"),
    ];
    assert_eq!(
        refused[0].code(),
        "unsupported_runtime",
        "{}",
        refused[0].message()
    );
    assert_eq!(
        refused[1].code(),
        "bad_manifest",
        "{}",
        refused[1].message()
    );
    assert_eq!(
        refused[2].code(),
        "spawn_failed",
        "{}",
        refused[2].message()
    );
    // Three refusals, three codes — not one generic "bad plugin". Two of them are
    // also already in `broken`; a no-entry plugin is legible, so it reaches
    // `spawn_failed` only when a start is attempted.
    let mut distinct: Vec<&str> = refused.iter().map(|error| error.code()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "{distinct:?}");
    // And nothing was spawned by any of that.
    assert!(host.loaded_names().is_empty());

    // The refusal for the entry names the field, not serde's voice.
    assert!(
        refused[2].message().contains("entry"),
        "{}",
        refused[2].message()
    );
    assert!(
        refused[0].message().contains("node, python, exec"),
        "{}",
        refused[0].message()
    );
}

#[test]
fn a_reply_with_a_different_id_is_a_protocol_error() {
    if !node_available() {
        skip("node is not on PATH, so no plugin can answer the wrong id");
        return;
    }
    let scratch = Scratch::new("bad-id");
    let liar = r#"
// Answers every request with the id plus one. If the host accepted "close
// enough", this plugin would hand every call some other call's result.
const readline = require('node:readline');
readline.createInterface({ input: process.stdin }).on('line', (line) => {
  if (line.trim() === '') return;
  const request = JSON.parse(line);
  const reply = (result) =>
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id + 1, result }) + '\n');
  if (request.method === 'initialize') {
    reply({ protocolVersion: 1, name: 'liar', version: '1.0.0', capabilities: { tools: true } });
    return;
  }
  reply({ tools: [] });
});
"#;
    scratch.install(
        "liar",
        r#"{"name":"liar","version":"1.0.0","description":"answers the wrong id","runtime":"node","entry":"liar.js"}"#,
        &[("liar.js", liar)],
    );

    let host = scratch.host();
    // `initialize` is answered with id + 1, so loading itself must fail: there
    // is no point loading a plugin whose first answer was already untrustworthy.
    let error = host.load("liar").expect_err("the id does not match");
    assert_eq!(error.kind(), Kind::ProtocolError, "{}", error.message());
    assert_eq!(error.code(), "protocol_error");
    assert!(error.message().contains("id"), "{}", error.message());
    // And it was not left running.
    assert!(host.loaded_names().is_empty());
}

#[test]
fn a_plugin_that_exits_on_its_own_is_noticed_by_the_next_call_not_a_watcher() {
    if !node_available() {
        skip("node is not on PATH, so no plugin can exit on its own");
        return;
    }
    let scratch = Scratch::new("dies");
    // Introduces itself, then exits with status 3 without being asked. There is
    // no health thread to notice: the next call finds `try_wait` reporting an
    // exit, which is why a plugin nobody calls costs nothing to watch.
    let dying = r#"
const readline = require('node:readline');
readline.createInterface({ input: process.stdin }).on('line', (line) => {
  if (line.trim() === '') return;
  const request = JSON.parse(line);
  process.stdout.write(JSON.stringify({
    jsonrpc: '2.0', id: request.id,
    result: { protocolVersion: 1, name: 'dying', version: '1.0.0', capabilities: { tools: true } },
  }) + '\n');
  // Give the host a moment to read the handshake before leaving.
  setTimeout(() => process.exit(3), 20);
});
"#;
    scratch.install(
        "dying",
        r#"{"name":"dying","version":"1.0.0","description":"exits after the handshake","runtime":"node","entry":"dying.js"}"#,
        &[("dying.js", dying)],
    );

    let host = scratch.host();
    host.load("dying")
        .expect("the handshake arrives before it leaves");
    std::thread::sleep(Duration::from_millis(150));
    let error = host.tools("dying").expect_err("the child is gone");
    assert_eq!(error.kind(), Kind::ChildExited, "{}", error.message());
    // The exit status is in the message: "it died" without a code leaves the
    // operator to guess whether the plugin crashed or was killed.
    assert!(error.message().contains('3'), "{}", error.message());
    // And the host is still usable: it reports the death, it does not propagate it.
    assert!(host.loaded_names().contains(&"dying".to_string()));
    host.unload("dying")
        .expect("unloading a dead plugin is not an error");
}

#[test]
fn a_malformed_reply_is_bad_response_and_quotes_two_hundred_bytes_at_most() {
    if !node_available() {
        skip("node is not on PATH, so no plugin can write a malformed line");
        return;
    }
    let scratch = Scratch::new("bad-response");
    let noisy = r#"
// Answers initialize, then writes a line that is not JSON-RPC at all — the shape
// of a stray `console.log` that reached stdout. The host must call it
// `bad_response` and quote enough of it to recognise, not echo all of it.
const readline = require('node:readline');
const filler = 'y'.repeat(400);
readline.createInterface({ input: process.stdin }).on('line', (line) => {
  if (line.trim() === '') return;
  const request = JSON.parse(line);
  if (request.method === 'initialize') {
    process.stdout.write(JSON.stringify({
      jsonrpc: '2.0', id: request.id,
      result: { protocolVersion: 1, name: 'noisy', version: '1.0.0', capabilities: { tools: true } },
    }) + '\n');
    return;
  }
  process.stdout.write('not json at all: ' + filler + '\n');
});
"#;
    scratch.install(
        "noisy",
        r#"{"name":"noisy","version":"1.0.0","description":"writes a malformed reply","runtime":"node","entry":"noisy.js"}"#,
        &[("noisy.js", noisy)],
    );

    let host = scratch.host();
    host.load("noisy")
        .expect("initialize is answered correctly");
    let error = host.tools("noisy").expect_err("the reply is not JSON-RPC");
    assert_eq!(error.kind(), Kind::BadResponse, "{}", error.message());
    assert_eq!(error.code(), "bad_response");
    // The malformed line is quoted, and the quote is capped: a 5 MB garbage line
    // must not become a 5 MB error message in somebody's log.
    assert!(
        error.message().contains("not json at all"),
        "{}",
        error.message()
    );
    assert!(error.message().contains("not JSON"), "{}", error.message());
    let quote = error.message().split("it began ").nth(1).unwrap_or("");
    assert!(quote.starts_with('"'), "{quote}");
    // 200 bytes of payload plus escaping (`y` does not escape, so the count is
    // exact) and the surrounding quotes.
    assert!(
        quote.len() < 260,
        "the quoted head was {} bytes: {quote}",
        quote.len()
    );
    assert!(quote.contains(&"y".repeat(150)), "{quote}");
}

#[test]
fn a_reply_over_the_byte_cap_is_refused_without_buffering_it() {
    if !node_available() {
        skip("node is not on PATH, so no plugin can overflow the cap");
        return;
    }
    let scratch = Scratch::new("too-large");
    let huge = r#"
// Answers tools/list with a five-megabyte line: more than the 1 MiB cap the
// test sets, and more than the host is willing to hold for one plugin.
const readline = require('node:readline');
const body = 'x'.repeat(5 << 20);
readline.createInterface({ input: process.stdin }).on('line', (line) => {
  if (line.trim() === '') return;
  const request = JSON.parse(line);
  if (request.method === 'initialize') {
    process.stdout.write(JSON.stringify({
      jsonrpc: '2.0', id: request.id,
      result: { protocolVersion: 1, name: 'huge', version: '1.0.0', capabilities: { tools: true } },
    }) + '\n');
    return;
  }
  process.stdout.write(JSON.stringify({
    jsonrpc: '2.0', id: request.id, result: { tools: [{ name: body, description: '', schema: {} }] },
  }) + '\n');
});
"#;
    scratch.install(
        "huge",
        r#"{"name":"huge","version":"1.0.0","description":"answers 5 MiB","runtime":"node","entry":"huge.js"}"#,
        &[("huge.js", huge)],
    );

    let mut host = scratch.host();
    host.set_max_bytes(1 << 20);
    assert_eq!(host.max_bytes(), 1 << 20);
    host.load("huge").expect("initialize fits under the cap");
    let error = host.tools("huge").expect_err("5 MiB over a 1 MiB cap");
    assert_eq!(error.kind(), Kind::TooLarge, "{}", error.message());
    assert_eq!(error.code(), "too_large");
    // The message quotes the cap, because "too large" without the number leaves
    // the author guessing at which of their plugins' limits they hit.
    assert!(
        error.message().contains("1048576-byte cap"),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("dropped, not buffered"),
        "{}",
        error.message()
    );
    // The host stayed alive and the child stayed usable: the over-long line was
    // drained, not half-read, so the next call is answered normally.
    let stopped = host.unload("huge").expect("unload");
    assert!(stopped.is_some());
}

// ---------------------------------------------------------------------------
// A second runtime, with no host change: the language claim in one test.
// ---------------------------------------------------------------------------

#[test]
fn a_python_plugin_joins_the_protocol_with_ten_lines() {
    if !python_available() {
        skip("python is not on PATH, so the second-runtime case cannot run");
        return;
    }
    let scratch = Scratch::new("python");
    let smoke = include_str!("reference-node/smoke.py");
    scratch.install(
        "smoke",
        r#"{"name":"smoke","version":"1.0.0","description":"a second runtime, ten lines","runtime":"python","entry":"smoke.py"}"#,
        &[("smoke.py", smoke)],
    );

    let host = scratch.host();
    let loaded = host.load("smoke").expect("a Python plugin initializes");
    assert_eq!(loaded.handshake.name, "smoke");
    assert_eq!(loaded.handshake.version, "1.0.0");

    let tools = host.tools("smoke").expect("tools/list");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "hello");
    let output = host
        .call_tool("smoke", "hello", json!({}))
        .expect("tools/call");
    assert_eq!(output.output, "hello from python");
    assert!(!output.is_error);

    let stopped = host.unload("smoke").expect("unload").expect("was loaded");
    assert_clean(&stopped);
}

// ---------------------------------------------------------------------------
// Lifecycle details the brief calls out.
// ---------------------------------------------------------------------------

#[test]
fn starting_a_host_spawns_nothing_and_unloading_waits_for_the_child() {
    let scratch = Scratch::new("lifecycle");
    // Even the reference plugin is not spawned by `start` or `discover`; the
    // assertion is `loaded_names`, which is empty until `load` is called.
    let host = scratch.host();
    let (found, broken) = host.discover();
    assert!(found.is_empty());
    assert!(broken.is_empty());
    assert!(host.loaded_names().is_empty());

    if !node_available() {
        skip("node is not on PATH, so the load/unload half cannot run");
        return;
    }
    scratch.install_reference();
    host.load("reference").expect("initialize");
    assert_eq!(host.loaded_names(), vec!["reference".to_string()]);

    let stopped = host
        .unload("reference")
        .expect("unload")
        .expect("was loaded");
    assert_clean(&stopped);
    assert!(host.loaded_names().is_empty());

    // A second unload is a no-op, not an error: a caller that unloads in `Drop`
    // and again explicitly is behaving reasonably.
    assert!(host.unload("reference").expect("second unload").is_none());

    // Loading again after an unload starts a fresh child, not a dead handle.
    host.load("reference").expect("reload");
    assert_eq!(host.loaded_names(), vec!["reference".to_string()]);
}

#[test]
fn the_manifest_the_reference_plugin_ships_parses_as_written() {
    // A cheap test that does not need Node: the fixture manifest is part of the
    // contract `PROTOCOL.md` documents, so it is checked on every machine.
    let manifest: serde_json::Value =
        serde_json::from_str(REFERENCE_MANIFEST).expect("plugin.json is valid JSON");
    assert_eq!(manifest["name"], json!("reference"));
    assert_eq!(manifest["runtime"], json!("node"));
    assert_eq!(manifest["entry"], json!("index.js"));
    assert!(forge_plugin::is_valid_name(
        manifest["name"].as_str().expect("a name")
    ));
}

// ---------------------------------------------------------------------------
// The exec runtime: a native binary, no interpreter in front.
// ---------------------------------------------------------------------------

/// The native plugin this package builds, at its cargo output path.
const NATIVE_EXE: &str = env!("CARGO_BIN_EXE_forge-plugin-native");

/// Install the native binary as a plugin. The entry must live inside the plugin
/// directory (`Manifest::command_line` joins it and refuses absolute paths), so
/// the executable is copied in — which is also what an installed native bundle
/// looks like on disk.
fn install_native(scratch: &Scratch) -> PathBuf {
    let plugin = scratch.dir.join("native");
    std::fs::create_dir_all(&plugin).expect("create the plugin directory");
    let entry = if cfg!(windows) {
        "native.exe"
    } else {
        "native"
    };
    std::fs::copy(NATIVE_EXE, plugin.join(entry)).expect("copy the native plugin");
    let manifest = json!({
        "name": "native",
        "version": "0.1.0",
        "description": "a native plugin, for the exec runtime",
        "runtime": "exec",
        "entry": entry,
    });
    std::fs::write(
        plugin.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).expect("manifest json"),
    )
    .expect("write the manifest");
    plugin
}

#[test]
fn a_native_binary_answers_the_whole_handshake_and_calls_a_tool() {
    // The gap this closes: `exec` had only ever been tested as a string that
    // parses. Every other case in this file spawns an interpreter, so a bug in
    // the no-interpreter argv would have passed the suite.
    let scratch = Scratch::new("native-call");
    install_native(&scratch);
    let host = scratch.host();
    let loaded = host.load("native").expect("load the native plugin");
    assert_eq!(loaded.handshake.protocol_version, 1);
    assert_eq!(loaded.handshake.name, "native");
    assert!(loaded.handshake.capabilities.tools);
    assert!(!loaded.handshake.capabilities.prompts);

    let tools = host.tools("native").expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["echo", "fail", "fill"]);

    let out = host
        .call_tool("native", "echo", json!({ "text": "hello from rust" }))
        .expect("call echo");
    assert_eq!(out.output, "hello from rust");
    assert!(!out.is_error);
}

#[test]
fn a_native_plugin_keeps_the_two_failure_shapes_apart() {
    let scratch = Scratch::new("native-failures");
    install_native(&scratch);
    let host = scratch.host();
    host.load("native").expect("load");

    // A tool that ran and refused: a result the model must see.
    let refused = host
        .call_tool("native", "fail", json!({}))
        .expect("call fail");
    assert!(refused.is_error);
    assert_eq!(refused.output, "this tool always refuses");

    // A call that was never attempted: an error, not a result.
    let err = host
        .call_tool("native", "nosuchtool", json!({}))
        .expect_err("an unknown tool is not a result");
    assert_eq!(err.code(), "bad_response");
    assert!(
        err.message().contains("no tool named"),
        "the plugin's own words should survive: {}",
        err.message()
    );
}

#[test]
fn a_native_plugin_is_discovered_without_being_started_and_stops_cleanly() {
    let scratch = Scratch::new("native-lifecycle");
    install_native(&scratch);
    let host = scratch.host();
    let (found, broken) = host.discover();
    assert!(broken.is_empty(), "{broken:?}");
    assert_eq!(found.len(), 1);
    assert!(host.loaded_names().is_empty(), "discovery must not spawn");

    host.load("native").expect("load");
    assert_eq!(host.loaded_names(), vec!["native".to_string()]);
    let stopped = host.unload("native").expect("unload").expect("was loaded");
    assert!(
        stopped.clean,
        "the plugin should answer shutdown: {stopped:?}"
    );
    assert!(host.loaded_names().is_empty());
}

// ---------------------------------------------------------------------------
// A TypeScript entry, with no build step.
// ---------------------------------------------------------------------------

const TS_MANIFEST: &str = r#"{
  "name": "typed",
  "version": "0.1.0",
  "description": "a TypeScript entry, executed as written",
  "runtime": "node",
  "entry": "index.ts"
}"#;

/// Types are erased by Node, so the entry is the source file itself.
///
/// This is not a new runtime and not a host feature: `Runtime::Node` already runs
/// `node <entry>`, and the entry being `.ts` is not a special case. The case
/// exists because "TypeScript works with no build step" is a claim about somebody
/// else's binary, and a claim like that rots the moment the machine changes
/// unless a test re-checks it.
const TS_ENTRY: &str = r#"
import * as readline from "node:readline";

interface Params { name?: string; arguments?: Record<string, unknown> }
interface Frame { jsonrpc: string; id?: unknown; method?: string; params?: Params }

function greet(args: Record<string, unknown>): string {
  const name: string = typeof args.name === "string" ? args.name : "";
  if (!name) throw new Error("greet needs arguments.name");
  return `typed hello, ${name}`;
}

function reply(id: unknown, result: unknown): void {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line: string) => {
  if (!line.trim()) return;
  const frame: Frame = JSON.parse(line);
  if (frame.id === undefined) return;
  if (frame.method === "initialize") {
    reply(frame.id, { protocolVersion: 1, name: "typed", version: "0.1.0",
      capabilities: { tools: true, prompts: false, hooks: [] } });
  } else if (frame.method === "tools/list") {
    reply(frame.id, { tools: [{ name: "greet", description: "typed greeting",
      schema: { type: "object", properties: { name: { type: "string" } }, required: ["name"] } }] });
  } else if (frame.method === "tools/call") {
    try {
      reply(frame.id, { output: greet(frame.params?.arguments ?? {}), isError: false });
    } catch (err) {
      reply(frame.id, { output: (err as Error).message, isError: true });
    }
  } else if (frame.method === "shutdown") {
    reply(frame.id, {});
    process.stdout.write("", () => process.exit(0));
  }
});
"#;

#[test]
fn a_typescript_entry_runs_with_no_build_step() {
    if !node_available() {
        skip("node is not on PATH");
        return;
    }
    if !node_strips_types() {
        skip("node is older than 23.6, so .ts needs --experimental-strip-types");
        return;
    }
    let scratch = Scratch::new("ts-entry");
    scratch.install("typed", TS_MANIFEST, &[("index.ts", TS_ENTRY)]);
    let host = scratch.host();

    let loaded = host.load("typed").expect("a .ts entry must initialize");
    assert_eq!(loaded.handshake.name, "typed");
    assert_eq!(loaded.handshake.protocol_version, 1);
    let tools = host.tools("typed").expect("tools/list");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "greet");

    let out = host
        .call_tool("typed", "greet", json!({"name": "no build step"}))
        .expect("tools/call");
    assert!(!out.is_error);
    assert_eq!(out.output, "typed hello, no build step");

    // The refusal path still works through the erased types.
    let refused = host
        .call_tool("typed", "greet", json!({}))
        .expect("a refusal is a result");
    assert!(refused.is_error);
    assert!(refused.output.contains("arguments.name"));
    host.unload_all();
}
