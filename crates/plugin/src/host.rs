//! The host: discovery, lifecycle and the four method families.
//!
//! `Host::start` never spawns anything. Loading is a separate, explicit act,
//! because discovering what is installed must not run code: a listing of a
//! directory is not consent to execute what is in it. That is the same rule
//! `hearth-skill` follows for a `SKILL.md`, and it matters more here, where the
//! artifact is a program.
//!
//! The host holds one `Connection` per loaded plugin behind a `Mutex`, so a
//! call from a second thread waits rather than interleaving bytes on the same
//! pipe. Interleaving would be a protocol violation the plugin could not
//! detect: two requests, two replies, and no way to tell which reply answers
//! which call once the ids are read out of order.
//!
//! There is no health-check thread. A dead plugin is discovered by the next
//! call that tries to use it — `round_trip` checks `try_wait` before writing and
//! treats a broken pipe as `child_exited`. A watcher would have to own the
//! child's exit status and race the call path for it, which is more machinery
//! than the information is worth: the caller is about to find out anyway.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use hearth_mcp::{Logger, NoopLogger};

use crate::connection::{Connection, Stopped};
use crate::error::{Error, Kind, Result};
use crate::manifest::{self, Manifest};
use crate::protocol::{
    self, Capabilities, Handshake, PromptSpec, ToolOutput, ToolSpec, DEFAULT_MAX_BYTES,
    DEFAULT_TIMEOUT_MS, PROTOCOL_VERSION,
};

/// What a loaded plugin turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub manifest: Manifest,
    pub handshake: Handshake,
}

/// The host over one or more plugin roots.
///
/// More than one root exists because a plugin reaches the machine two ways: the
/// operator puts a directory under `<dataDir>/plugins`, or `hearth market install`
/// unpacks a bundle under `<dataDir>/market/bundles`. Both are plugins; a host
/// that only read the first would make the market's own promise — "install plugin
/// bundles from an index" — end at a directory nothing loads.
pub struct Host {
    /// Search order. The first root is the one `start` was given.
    roots: Vec<PathBuf>,
    timeout: Duration,
    max_bytes: usize,
    /// The protocol version this host announces, which is the host's own unless
    /// a caller sets another one to test a plugin against a future host.
    protocol_version: u32,
    next_id: AtomicU64,
    loaded: Mutex<BTreeMap<String, Arc<Mutex<Connection>>>>,
    handshakes: Mutex<BTreeMap<String, Handshake>>,
    logger: Arc<dyn Logger>,
}

impl Host {
    /// Point the host at a directory of plugin directories. Spawns nothing.
    pub fn start(root: impl Into<PathBuf>) -> Host {
        Host {
            roots: vec![root.into()],
            timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
            max_bytes: DEFAULT_MAX_BYTES,
            protocol_version: PROTOCOL_VERSION,
            next_id: AtomicU64::new(0),
            loaded: Mutex::new(BTreeMap::new()),
            handshakes: Mutex::new(BTreeMap::new()),
            logger: Arc::new(NoopLogger),
        }
    }

    /// The primary root: the one `start` was given, and the one a caller prints
    /// when it has one line to say where plugins live.
    pub fn root(&self) -> &Path {
        &self.roots[0]
    }

    /// Every root, in search order.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Search another directory of plugin directories as well. Spawns nothing.
    ///
    /// A name offered by two roots is refused by [`Host::discover`] rather than
    /// resolved by order: the two directories can hold different code, and
    /// running the one the operator did not mean is worse than not running.
    pub fn add_root(&mut self, root: impl Into<PathBuf>) {
        let root = root.into();
        if !self.roots.contains(&root) {
            self.roots.push(root);
        }
    }

    /// Where the child's stderr goes. Default is a logger that drops it; the
    /// CLI passes one that writes to its own log.
    pub fn with_logger(mut self, logger: Arc<dyn Logger>) -> Host {
        self.logger = logger;
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The deadline for one call. A plugin that needs longer than this to do
    /// its work is a plugin that should return a handle and work in the
    /// background; the host will not hold a turn open for it.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// The cap on one reply, in bytes, applied to plugins loaded afterwards.
    pub fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes.max(1);
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    /// The version announced in `initialize`. Defaults to [`PROTOCOL_VERSION`];
    /// a caller may lower it to drive a plugin that predates a bump, which is
    /// the only honest way to find out whether the plugin really checks.
    pub fn set_protocol_version(&mut self, version: u32) {
        self.protocol_version = version;
    }

    /// Every plugin directory under the root, whether or not it parses.
    ///
    /// A directory whose `plugin.json` is broken is returned in `broken` rather
    /// than dropped: an installed plugin that stopped parsing is something the
    /// operator has to see, and hiding it would make `hearth plugin list` a
    /// report that only mentions the healthy.
    pub fn discover(&self) -> (Vec<Manifest>, Vec<(PathBuf, Error)>) {
        let mut found: Vec<Manifest> = Vec::new();
        let mut broken = Vec::new();
        for root in &self.roots {
            let entries = match std::fs::read_dir(root) {
                Ok(entries) => entries,
                // A machine without this plugin directory is the normal case,
                // not a failure: it means nobody installed anything here. With
                // several roots it is also the common case — most machines have
                // hand-installed plugins or market ones, not both.
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                match manifest::read(&path) {
                    Ok(manifest) => {
                        if let Some(position) = found.iter().position(|m| m.name == manifest.name) {
                            // Two directories claim one name. Withdraw the first
                            // copy as well: leaving it in `found` would let
                            // `manifest()` return it and silently run the first
                            // root's code, which is exactly the coin flip this
                            // refusal exists to prevent. Neither copy is safely
                            // loadable until the operator removes one.
                            let first = found.remove(position);
                            broken.push((
                                path,
                                Error::new(
                                    Kind::BadManifest,
                                    format!(
                                        "plugin {:?} is installed twice: {} and {}; remove one \
                                         (`hearth market remove {}` for the market copy, or delete \
                                         the plugin directory for the other)",
                                        manifest.name,
                                        first.directory.display(),
                                        manifest.directory.display(),
                                        manifest.name
                                    ),
                                ),
                            ));
                            continue;
                        }
                        found.push(manifest);
                    }
                    Err(error) => broken.push((path, error)),
                }
            }
        }
        found.sort_by(|a, b| a.name.cmp(&b.name));
        broken.sort_by(|a, b| a.0.cmp(&b.0));
        (found, broken)
    }

    fn manifest(&self, name: &str) -> Result<Manifest> {
        let (found, broken) = self.discover();
        if let Some(manifest) = found.into_iter().find(|m| m.name == name) {
            return Ok(manifest);
        }
        // A broken directory is more informative than "unknown": the operator
        // installed exactly this plugin and it is the manifest that is wrong, so
        // the refusal they see should be the manifest's own. Matched by
        // directory name because a manifest too broken to parse has no name to
        // match on.
        if let Some((_, error)) = broken.iter().find(|(path, _)| {
            path.file_name()
                .is_some_and(|directory| directory.to_string_lossy() == name)
        }) {
            return Err(error.clone());
        }
        let searched = self
            .roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(Error::new(
            Kind::UnknownPlugin,
            format!("no plugin named {name:?} under {searched}"),
        ))
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Spawn the plugin, run `initialize`, and remember the handshake.
    ///
    /// Re-loading a plugin that is already running returns the handshake it
    /// gave; re-loading one whose child died replaces it, because a plugin that
    /// crashed is a plugin the caller asked to start again.
    pub fn load(&self, name: &str) -> Result<Loaded> {
        if let Some(connection) = self.lookup(name) {
            let alive = {
                let mut guard = connection.lock().map_err(|_| {
                    Error::new(
                        Kind::Io,
                        format!("{name}: the connection lock was poisoned"),
                    )
                })?;
                !guard.has_exited()
            };
            if alive {
                if let Some(handshake) = self.handshake(name) {
                    return Ok(Loaded {
                        manifest: self.manifest(name)?,
                        handshake,
                    });
                }
            }
            let _ = self.unload(name);
        }

        let manifest = self.manifest(name)?;
        let mut connection =
            Connection::spawn_with_limit(&manifest, Arc::clone(&self.logger), self.max_bytes)?;

        let id = self.next_id();
        let params = json!({
            "protocolVersion": self.protocol_version,
            "client": { "name": "hearth", "version": env!("CARGO_PKG_VERSION") },
        });
        let reply = connection.call(id, "initialize", Some(params), self.timeout);
        let result = match reply {
            Ok(result) => result,
            Err(error) => {
                // A child that failed to introduce itself is not left running:
                // it is either broken or the wrong program, and both are better
                // diagnosed by its stderr than by a retry.
                let error = annex_stderr(error, &connection);
                connection.kill("initialize failed");
                return Err(error);
            }
        };
        let mut handshake: Handshake = match serde_json::from_value(result) {
            Ok(handshake) => handshake,
            Err(error) => {
                connection.kill("initialize answered an unknown shape");
                return Err(Error::new(
                    Kind::BadResponse,
                    format!("{name}: initialize answered a shape this host does not know: {error}"),
                ));
            }
        };
        if handshake.protocol_version != self.protocol_version {
            connection.kill("protocol version mismatch");
            return Err(Error::new(
                Kind::ProtocolError,
                format!(
                    "{name}: speaks protocol version {}, this host speaks {}",
                    handshake.protocol_version, self.protocol_version
                ),
            ));
        }
        if handshake.name.is_empty() {
            handshake.name = manifest.name.clone();
        }
        if handshake.version.is_empty() {
            handshake.version = manifest.version.clone();
        }
        let _ = connection.notify("initialized", None);
        let entry = Arc::new(Mutex::new(connection));
        if let Ok(mut loaded) = self.loaded.lock() {
            loaded.insert(name.to_string(), entry);
        }
        if let Ok(mut map) = self.handshakes.lock() {
            map.insert(name.to_string(), handshake.clone());
        }
        Ok(Loaded {
            manifest,
            handshake,
        })
    }

    /// Stop the plugin, wait for it, and report how it went.
    ///
    /// `Ok(None)` means it was not loaded — not an error, because a caller that
    /// unloads twice (a `Drop`, then an explicit shutdown) is behaving
    /// reasonably.
    pub fn unload(&self, name: &str) -> Result<Option<Stopped>> {
        let connection = self.loaded.lock().ok().and_then(|mut map| map.remove(name));
        let Some(connection) = connection else {
            return Ok(None);
        };
        let mut guard = connection.lock().map_err(|_| {
            Error::new(
                Kind::Io,
                format!("{name}: the connection lock was poisoned"),
            )
        })?;
        let stopped = guard.close();
        drop(guard);
        if let Ok(mut map) = self.handshakes.lock() {
            map.remove(name);
        }
        Ok(Some(stopped))
    }

    /// Stop every loaded plugin.
    pub fn unload_all(&self) {
        let names = self.loaded_names();
        for name in names {
            let _ = self.unload(&name);
        }
    }

    pub fn loaded_names(&self) -> Vec<String> {
        self.loaded
            .lock()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn is_loaded(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    pub fn handshake(&self, name: &str) -> Option<Handshake> {
        self.handshakes
            .lock()
            .ok()
            .and_then(|map| map.get(name).cloned())
    }

    pub fn capabilities(&self, name: &str) -> Option<Capabilities> {
        self.handshake(name).map(|handshake| handshake.capabilities)
    }

    fn lookup(&self, name: &str) -> Option<Arc<Mutex<Connection>>> {
        self.loaded
            .lock()
            .ok()
            .and_then(|map| map.get(name).cloned())
    }

    fn connection(&self, name: &str) -> Result<Arc<Mutex<Connection>>> {
        self.lookup(name).ok_or_else(|| {
            Error::new(
                Kind::NotInitialized,
                format!("{name} is not loaded; call load({name:?}) before using it"),
            )
        })
    }

    /// Fail with `not_initialized` when `name` was never loaded.
    ///
    /// The capability check alone would answer "no tools" for a plugin that is
    /// not loaded, and "it has no tools" is a different fact from "you have not
    /// started it" — one is an answer, the other is a missing step.
    fn require_loaded(&self, name: &str) -> Result<Capabilities> {
        let capabilities = self.capabilities(name).ok_or_else(|| {
            Error::new(
                Kind::NotInitialized,
                format!("{name} is not loaded; call load({name:?}) before using it"),
            )
        })?;
        self.connection(name)?;
        Ok(capabilities)
    }

    /// One round trip, returning the reply's `result`.
    fn call_method(&self, name: &str, method: &str, params: Option<Value>) -> Result<Value> {
        let connection = self.connection(name)?;
        let mut guard = connection.lock().map_err(|_| {
            Error::new(
                Kind::Io,
                format!("{name}: the connection lock was poisoned"),
            )
        })?;
        let id = self.next_id();
        match guard.call(id, method, params, self.timeout) {
            Ok(result) => Ok(result),
            Err(error) => Err(annex_stderr(error, &guard)),
        }
    }

    /// What tools the plugin advertises, with names left unqualified; the
    /// caller namespaces them by plugin so a plugin cannot shadow a builtin.
    pub fn tools(&self, name: &str) -> Result<Vec<ToolSpec>> {
        if !self.require_loaded(name)?.tools {
            // A plugin that did not claim tools is not asked: the claim is what
            // makes "it has no tools" and "it refused to answer" distinguishable
            // without a round trip that may not be implemented.
            return Ok(Vec::new());
        }
        let value = self.call_method(name, "tools/list", None)?;
        let list = value
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::new(
                    Kind::BadResponse,
                    format!("{name}: tools/list answered without a tools array"),
                )
            })?;
        let mut tools = Vec::new();
        for entry in list {
            let tool: ToolSpec = serde_json::from_value(entry.clone()).map_err(|error| {
                Error::new(
                    Kind::BadResponse,
                    format!("{name}: a tool in tools/list is malformed: {error}"),
                )
            })?;
            if tool.name.trim().is_empty() {
                return Err(Error::new(
                    Kind::BadResponse,
                    format!("{name}: tools/list contains a tool with no name"),
                ));
            }
            tools.push(tool);
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tools)
    }

    /// Run one tool.
    ///
    /// A tool that ran and failed comes back `is_error: true` with the plugin's
    /// own message; a plugin that could not run the call at all is a Rust
    /// `Err`. The two are different facts and the model needs to see only the
    /// first one, which is why this returns `ToolOutput`, not `Value`.
    pub fn call_tool(&self, name: &str, tool: &str, arguments: Value) -> Result<ToolOutput> {
        if tool.trim().is_empty() {
            return Err(Error::new(
                Kind::BadResponse,
                format!("{name}: tools/call needs a tool name"),
            ));
        }
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments
        };
        let value = self.call_method(
            name,
            "tools/call",
            Some(json!({ "name": tool, "arguments": arguments })),
        )?;
        let output = value
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                Error::new(
                    Kind::BadResponse,
                    format!("{name}: tools/call answered without an output string"),
                )
            })?;
        let is_error = value
            .get("isError")
            .or_else(|| value.get("is_error"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(ToolOutput { output, is_error })
    }

    pub fn prompts(&self, name: &str) -> Result<Vec<PromptSpec>> {
        if !self.require_loaded(name)?.prompts {
            return Ok(Vec::new());
        }
        let value = self.call_method(name, "prompts/list", None)?;
        let list = value
            .get("prompts")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::new(
                    Kind::BadResponse,
                    format!("{name}: prompts/list answered without a prompts array"),
                )
            })?;
        let mut prompts = Vec::new();
        for entry in list {
            let prompt: PromptSpec = serde_json::from_value(entry.clone()).map_err(|error| {
                Error::new(
                    Kind::BadResponse,
                    format!("{name}: a prompt in prompts/list is malformed: {error}"),
                )
            })?;
            if prompt.name.trim().is_empty() {
                return Err(Error::new(
                    Kind::BadResponse,
                    format!("{name}: prompts/list contains a prompt with no name"),
                ));
            }
            prompts.push(prompt);
        }
        prompts.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(prompts)
    }

    pub fn prompt_text(&self, name: &str, prompt: &str) -> Result<String> {
        let value = self.call_method(name, "prompts/get", Some(json!({ "name": prompt })))?;
        value
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                Error::new(
                    Kind::BadResponse,
                    format!("{name}: prompts/get answered without a text string"),
                )
            })
    }

    /// Run a hook, if the plugin claims that phase.
    ///
    /// A plugin that does not claim the phase is not called at all: calling it
    /// and ignoring an empty answer would make a plugin's own capability list
    /// meaningless, and a plugin that claims nothing should cost nothing.
    pub fn run_hook(&self, name: &str, phase: &str, payload: Value) -> Result<Value> {
        if !protocol::is_hook_phase(phase) {
            return Err(Error::new(
                Kind::ProtocolError,
                format!(
                    "{phase:?} is not a hook phase; the phases are {}",
                    protocol::HOOK_PHASES.join(", ")
                ),
            ));
        }
        if !payload.is_object() {
            return Err(Error::new(
                Kind::BadResponse,
                format!(
                    "{name}: the payload handed to {phase} must be an object, got {}",
                    type_name(&payload)
                ),
            ));
        }
        let capabilities = self.require_loaded(name)?;
        if !capabilities.runs_hook(phase) {
            return Ok(payload);
        }
        let value = self.call_method(
            name,
            "hooks/run",
            Some(json!({ "phase": phase, "payload": payload })),
        )?;
        let returned = value.get("payload").ok_or_else(|| {
            Error::new(
                Kind::BadResponse,
                format!("{name}: hooks/run answered without a payload"),
            )
        })?;
        // The payload must stay an object: a hook that returned a string would
        // otherwise replace a turn's context with a string.
        if !returned.is_object() {
            return Err(Error::new(
                Kind::BadResponse,
                format!(
                    "{name}: hooks/run answered a payload that is not an object, got {}",
                    type_name(returned)
                ),
            ));
        }
        Ok(returned.clone())
    }

    /// Every tool from every loaded plugin, in load-name order.
    pub fn all_tools(&self) -> Vec<(String, ToolSpec)> {
        let mut all = Vec::new();
        for name in self.loaded_names() {
            match self.tools(&name) {
                Ok(tools) => {
                    for tool in tools {
                        all.push((name.clone(), tool));
                    }
                }
                Err(error) => {
                    // A broken plugin must not hide the healthy ones; the caller
                    // can list the failures separately and report them.
                    self.logger.warn(&format!("plugin {name}: {}", error));
                }
            }
        }
        all
    }

    /// The runtime command a manifest implies, for the CLI's dry-run output.
    pub fn command_line(&self, name: &str) -> Result<Vec<String>> {
        self.manifest(name)?.command_line()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.unload_all();
    }
}

/// Append the child's own diagnosis to a failure.
///
/// A plugin's own diagnosis is almost always better than the host's, and the
/// host is the only party that captured it. Without this line the operator sees
/// "child_exited" and has to guess.
///
/// Why it is not just the last line: Node ends every crash report with a bare
/// `Node.js v22.23.4`, so reporting the last line reports the version and
/// throws away the `Error:` above it. The first CI run on a runner whose Node
/// was older than the development machine's produced exactly that useless
/// message. The chosen line is the last one that looks like a diagnosis; when
/// there is none, the last non-empty line still beats nothing.
fn annex_stderr(error: Error, connection: &Connection) -> Error {
    if matches!(
        error.kind(),
        Kind::ChildExited | Kind::Timeout | Kind::ProtocolError
    ) {
        let tail = connection.stderr_tail(12);
        let lines: Vec<&str> = tail
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !is_version_footer(line))
            .collect();
        let chosen = lines
            .iter()
            .rev()
            .find(|line| line.starts_with("Error") || line.contains("Error: "))
            .or_else(|| lines.last());
        if let Some(line) = chosen {
            return Error::new(
                error.kind(),
                format!("{} (stderr: {line})", error.message()),
            );
        }
    }
    error
}

/// `Node.js v22.23.4` — Node's crash footer, never the cause.
fn is_version_footer(line: &str) -> bool {
    match line.strip_prefix("Node.js v") {
        Some(rest) => rest.starts_with(|ch: char| ch.is_ascii_digit()),
        None => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("hearth-plugin-host-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write_plugin(root: &Path, name: &str, json_body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("plugin.json"), json_body).expect("write manifest");
        std::fs::write(dir.join("index.js"), b"// body").expect("write entry");
    }

    #[test]
    fn starting_a_host_spawns_nothing_and_a_missing_root_is_empty() {
        let root = std::env::temp_dir().join("hearth-plugin-nonexistent-root-xyz");
        let _ = std::fs::remove_dir_all(&root);
        let host = Host::start(&root);
        let (found, broken) = host.discover();
        assert!(found.is_empty());
        assert!(broken.is_empty());
        assert!(host.loaded_names().is_empty());
        // The defaults the document states.
        assert_eq!(host.timeout(), Duration::from_millis(DEFAULT_TIMEOUT_MS));
        assert_eq!(host.max_bytes(), DEFAULT_MAX_BYTES);
        assert_eq!(host.protocol_version(), PROTOCOL_VERSION);
    }

    #[test]
    fn discovery_separates_the_healthy_from_the_broken() {
        let root = scratch("discovery");
        write_plugin(
            &root,
            "good",
            r#"{"name":"good","version":"1.0.0","description":"fine","runtime":"node","entry":"index.js"}"#,
        );
        write_plugin(
            &root,
            "bad",
            r#"{"name":"bad","version":"1.0.0","description":"fine","runtime":"deno","entry":"index.ts"}"#,
        );
        let host = Host::start(&root);
        let (found, broken) = host.discover();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "good");
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].1.kind(), Kind::UnsupportedRuntime);
        // Nothing was spawned: discovery is a listing, not consent to execute.
        assert!(host.loaded_names().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loading_an_unknown_plugin_names_the_root_it_searched() {
        let root = scratch("unknown");
        let host = Host::start(&root);
        let error = host.load("nope").expect_err("unknown");
        assert_eq!(error.kind(), Kind::UnknownPlugin);
        assert!(
            error.message().contains(&root.display().to_string()),
            "{}",
            error.message()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loading_a_directory_whose_manifest_is_broken_reports_that_not_unknown() {
        let root = scratch("broken-load");
        write_plugin(
            &root,
            "broken",
            r#"{"name":"broken","version":"1.0.0","description":"fine","runtime":"deno","entry":"index.ts"}"#,
        );
        let host = Host::start(&root);
        let error = host.load("broken").expect_err("bad runtime");
        assert_eq!(error.kind(), Kind::UnsupportedRuntime);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn using_a_plugin_that_was_never_loaded_says_so() {
        let root = scratch("not-loaded");
        let host = Host::start(&root);
        let error = host.tools("ghost").expect_err("not loaded");
        assert_eq!(error.kind(), Kind::NotInitialized);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unloading_something_that_is_not_loaded_is_not_an_error() {
        let root = scratch("unload-missing");
        let host = Host::start(&root);
        assert!(host.unload("ghost").expect("unload").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hook_phase_outside_the_closed_set_is_refused() {
        let root = scratch("hook-phase");
        write_plugin(
            &root,
            "example",
            r#"{"name":"example","version":"1.0.0","description":"fine","runtime":"node","entry":"index.js"}"#,
        );
        let host = Host::start(&root);
        // The phase check runs before the "is it loaded" check, because a wrong
        // phase is a caller bug that no load could fix.
        let error = host
            .run_hook("example", "during_breakfast", json!({}))
            .expect_err("bad phase");
        assert_eq!(error.kind(), Kind::ProtocolError);
        assert!(
            error.message().contains("before_turn"),
            "{}",
            error.message()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hook_payload_must_be_an_object() {
        let root = scratch("hook-payload");
        let host = Host::start(&root);
        let error = host
            .run_hook("example", "before_turn", json!("a string"))
            .expect_err("bad payload");
        assert_eq!(error.kind(), Kind::BadResponse);
        assert!(error.message().contains("object"), "{}", error.message());
        let _ = std::fs::remove_dir_all(&root);
    }
}
