//! Tool wiring shared by `forge chat` and `forge web`: the built-ins plus the
//! three bridges that reach outside this process (MCP servers, installed
//! skills, subagent delegation).
//!
//! This lives in the CLI crate, not in `forge-tool`, because it is the only
//! place that knows all three of "what the operator configured", "which
//! process owns the MCP children" and "who is allowed to approve". The Go build
//! had the same split (`cmd/forge/mcp.go`, `tools.go`) for the same reason: a
//! library that dialled would have to own the process lifecycle, the restart
//! policy and the shutdown of every child it spawned.
//!
//! Ordering matters and is deliberate:
//!
//! 1. built-ins first, so the frozen seven names exist even when nothing else
//!    is configured;
//! 2. every MCP tool becomes its own `mcp__<server>__<tool>` entry with an
//!    `ask` rule ahead of the sandbox defaults — a remote process is not the
//!    sandbox, and the operator should see the call before it leaves;
//! 3. one `skill` tool for the whole installed set, registered only when at
//!    least one skill was found (an always-present tool that can only say
//!    "there are no skills" costs tokens on every request);
//! 4. the `subagent` tool last, because it has to be built from the registry
//!    it is about to join.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use forge_config::Config;
use forge_llm::{Provider, ToolSpec};
use forge_mcp::{CallResult, Client, Content, Server as McpServer, ToolDef, Transport};
use forge_skill::{scan, Set as SkillSet, Skill};
use forge_tool::{
    decode_args, effective_limits, merge, resolve, truncate, Call, Executor, ExecutorOptions,
    Limits, Meta, Policy, Registry, Result as ToolResult, Rules, Tool, ACTION_ASK, CODE_IO_ERROR,
    CODE_NOT_FOUND, CODE_OUTSIDE_WORKSPACE, CODE_TIMEOUT, ERR_BAD_ARGS,
};

use serde::Deserialize;
use serde_json::{json, Value};

/// Prefix of every bridged MCP tool name.
pub const MCP_NAME_PREFIX: &str = "mcp__";

/// The MCP call succeeded at the protocol level but the server said the tool
/// failed. The model should read the text and decide.
pub const CODE_MCP_TOOL_ERROR: &str = "mcp_tool_error";
/// The server is gone for the rest of the session (sticky in `forge-mcp`).
pub const CODE_MCP_SERVER_GONE: &str = "mcp_server_gone";
/// A transport or protocol failure that is the remote's fault.
pub const CODE_MCP_CALL_FAILED: &str = "mcp_call_failed";

/// The one tool that exposes every installed skill.
pub const SKILL_TOOL_NAME: &str = "skill";
/// How many skill names the single tool description lists. The description is
/// sent on every request, so an operator with 300 skills must not pay for all
/// of them.
pub const MAX_SKILLS_IN_DESCRIPTION: usize = 24;

const EMPTY_OBJECT_SCHEMA: &str = r#"{"type":"object","properties":{}}"#;

const SKILL_TOOL_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "name": {"type": "string", "description": "Name of the skill to load."},
    "file": {"type": "string", "description": "Optional path of an auxiliary file inside that skill's directory (for example \"reference.md\"). Omit to get the skill body."}
  },
  "required": ["name"],
  "additionalProperties": false
}"#;

/// A line the caller should print on stderr (a skipped server, a skill
/// warning). Collected rather than printed so this module stays testable
/// without capturing a process's stderr.
pub type Notes = Vec<String>;

/// Everything needed to build the tool set for one command.
pub struct BuildOptions<'a> {
    pub cfg: &'a Config,
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub data_dir: PathBuf,
    pub workspace: String,
    pub sandbox: String,
    /// `None` means no human is available; every `ask` is refused by the
    /// executor, which is the intended unattended behaviour.
    pub approver: Option<Arc<dyn forge_tool::Approver>>,
    pub limits: Limits,
    pub system: String,
    pub parent_session: String,
}

/// The result of wiring: a registry the loop reads specs from, the policy that
/// gates it, and the MCP clients that must be closed on exit.
pub struct WiredTools {
    pub registry: Arc<Registry>,
    pub policy: Arc<dyn Policy>,
    pub notes: Notes,
    approver: Option<Arc<dyn forge_tool::Approver>>,
    limits: Limits,
    workspace: String,
    clients: Vec<Arc<Mutex<Box<dyn Client>>>>,
}

impl WiredTools {
    /// The executor the loop runs tools through: the registry this wiring built,
    /// the policy that includes every MCP `ask` rule, and the approver the caller
    /// supplied (a human, or nobody).
    pub fn executor(&self) -> Executor {
        Executor::new(
            Arc::clone(&self.registry),
            ExecutorOptions {
                policy: Some(Arc::clone(&self.policy)),
                approver: self.approver.clone(),
                limits: self.limits,
                workspace: self.workspace.clone(),
            },
        )
    }

    /// Names in registry order, for the `tools=N` diagnostic line.
    pub fn names(&self) -> Vec<String> {
        self.registry.names()
    }

    /// Close every MCP client. Idempotent; children are killed here, so it must
    /// run before the process exits even on the error paths.
    pub fn close(&mut self) {
        for client in &self.clients {
            let mut guard = match client.lock() {
                Ok(guard) => guard,
                // A poisoned lock means a tool call panicked while holding the
                // connection; closing through the poison is still correct.
                Err(poisoned) => poisoned.into_inner(),
            };
            let _ = guard.close();
        }
        self.clients.clear();
    }
}

impl Drop for WiredTools {
    fn drop(&mut self) {
        self.close();
    }
}

// ---------------------------------------------------------------------------
// MCP naming
// ---------------------------------------------------------------------------

/// Escapes one name segment so the result contains only `[a-z0-9_]`.
///
/// `_` becomes `__` and `.` becomes `_dot_`; every other byte outside
/// `[a-z0-9]` becomes `_u<2 hex>_`, uppercase letters included — lower-casing
/// `Read` would collide with a real `read`. The result is pure ASCII and
/// deterministic, so the same server yields the same tool name on every run,
/// which is what lets a policy rule name it.
pub fn escape_mcp_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 8);
    for byte in name.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            b'_' => out.push_str("__"),
            b'.' => out.push_str("_dot_"),
            // Escaped per byte, so a multi-byte rune becomes several escapes
            // rather than one lossy character.
            other => out.push_str(&format!("_u{other:02x}_")),
        }
    }
    out
}

/// `mcp__<server>__<tool>`, the frozen bridge name.
///
/// `MCP_NAME_PREFIX` already carries its trailing double underscore, so the
/// separator here is exactly one more `__` between the two escaped segments.
pub fn mcp_tool_name(server: &str, remote: &str) -> String {
    format!(
        "{MCP_NAME_PREFIX}{}__{}",
        escape_mcp_name(server),
        escape_mcp_name(remote)
    )
}

/// One remote tool behind the `Tool` interface.
struct McpTool {
    /// Shared with every other tool of the same server: a `Client` is `&mut`
    /// for every call, so the connection is serialised. Two MCP servers stay
    /// independent because they have two handles.
    client: Arc<Mutex<Box<dyn Client>>>,
    server: String,
    /// The name the server knows.
    remote: String,
    /// `mcp__<server>__<tool>`.
    name: String,
    /// Precomputed: the loop asks for the spec on every request, and a remote
    /// description must not be re-fetched per request.
    spec: ToolSpec,
    limits: Limits,
}

impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call(&self, call: Call) -> ToolResult {
        // Verbatim, not decoded and re-encoded: the arguments belong to the
        // remote tool's schema, and a round trip would drop fields this build
        // does not know about.
        let args = if call.args.is_null() {
            json!({})
        } else {
            call.args.clone()
        };

        let mut guard = match self.client.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let outcome = guard.call(&self.remote, args);

        let mut meta = Meta::new();
        meta.insert("server".into(), json!(self.server));
        meta.insert("tool".into(), json!(self.remote));

        match outcome {
            Ok(result) => {
                meta.insert("blocks".into(), json!(result.content.len()));
                let (output, cut) = truncate(&self.render(&result), effective_limits(self.limits));
                let mut out = ToolResult::ok(output);
                out.meta = meta;
                out.truncated = cut;
                if result.is_error {
                    // The server said the call failed. Pass that through but
                    // keep its text: it is usually the only explanation.
                    out.is_error = true;
                    out.code = CODE_MCP_TOOL_ERROR.to_string();
                }
                out
            }
            Err(err) => {
                let code = self.failure_code(&err);
                let mut out = ToolResult::error(code, self.failure_text(code, &err));
                out.meta = meta;
                out
            }
        }
    }
}

impl McpTool {
    fn render(&self, result: &CallResult) -> String {
        if result.content.is_empty() {
            return format!("{}: the MCP server returned no content.", self.name);
        }
        let mut out = String::new();
        for (index, block) in result.content.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&render_block(block));
        }
        out
    }

    fn failure_code(&self, err: &forge_mcp::Error) -> &'static str {
        match err {
            // Sticky by design: the same client will never answer again, so a
            // retry is a guaranteed lost turn.
            forge_mcp::Error::ServerGone(_) => CODE_MCP_SERVER_GONE,
            forge_mcp::Error::Timeout(_) => CODE_TIMEOUT,
            _ => CODE_MCP_CALL_FAILED,
        }
    }

    fn failure_text(&self, code: &str, err: &forge_mcp::Error) -> String {
        match code {
            CODE_MCP_SERVER_GONE => format!(
                "{}: MCP server {:?} is gone ({err}). Do not retry this tool: that server will not answer again in this session. Continue without its tools, or tell the user the server needs to be restarted.",
                self.name, self.server
            ),
            CODE_TIMEOUT => format!(
                "{}: MCP server {:?} did not answer in time: {err}",
                self.name, self.server
            ),
            _ => format!(
                "{}: MCP server {:?} call failed: {err}",
                self.name, self.server
            ),
        }
    }
}

/// Text blocks pass through; binary blocks are summarised by type, size and
/// MIME type and their base64 payload is dropped — a base64 image in the
/// context window is thousands of tokens the model cannot decode.
fn render_block(block: &Content) -> String {
    match block.type_.as_str() {
        "text" | "" => block.text.clone().unwrap_or_default(),
        "image" => format!("[{}]", summarise_binary("image", block)),
        other => {
            // Unknown block types keep their text if the server sent any (the
            // spec allows extra fields), otherwise they are summarised.
            match &block.text {
                Some(text) if !text.is_empty() => text.clone(),
                _ => format!("[{}]", summarise_binary(other, block)),
            }
        }
    }
}

fn summarise_binary(kind: &str, block: &Content) -> String {
    let mime = match &block.mime_type {
        Some(mime) if !mime.is_empty() => mime.as_str(),
        _ => "unknown type",
    };
    // `data` is base64, so 3/4 of its length is the decoded size: close enough
    // for a summary, and never an excuse to decode the payload.
    let decoded = block
        .data
        .as_ref()
        .map(|data| data.len() / 4 * 3)
        .unwrap_or(0);
    format!("{kind} {mime}, about {decoded} bytes; payload not shown")
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SkillToolArgs {
    #[serde(default)]
    name: String,
    #[serde(default)]
    file: String,
}

/// The single tool for a scanned skill set.
struct SkillTool {
    by_name: BTreeMap<String, Skill>,
    /// Sorted, so the description and every "available skills" list is stable.
    names: Vec<String>,
    description: String,
    limits: Limits,
}

impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: SKILL_TOOL_NAME.to_string(),
            description: self.description.clone(),
            schema: serde_json::from_str(SKILL_TOOL_SCHEMA)
                .expect("the skill schema is a constant"),
        }
    }

    fn call(&self, call: Call) -> ToolResult {
        let args: SkillToolArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(fail) => return fail,
        };
        let name = args.name.trim();
        if name.is_empty() {
            return ToolResult::error(
                ERR_BAD_ARGS,
                format!(
                    "{SKILL_TOOL_NAME}: \"name\" is required; available skills: {}",
                    self.available()
                ),
            );
        }
        let Some(skill) = self.by_name.get(name) else {
            // not_found rather than bad_args: the name was well formed, the
            // skill simply is not installed, and the list is the recovery hint.
            return ToolResult::error(
                CODE_NOT_FOUND,
                format!(
                    "{SKILL_TOOL_NAME}: no skill named {name:?}; available skills: {}",
                    self.available()
                ),
            );
        };

        let want = args.file.trim();
        if !want.is_empty() {
            return self.read_file(skill, want);
        }

        // Loaded at call time, so wiring 50 skills reads no files and an edited
        // SKILL.md is picked up by the next call instead of after a restart.
        let loaded = match skill.load() {
            Ok(loaded) => loaded,
            Err(err) => {
                return ToolResult::error(
                    CODE_IO_ERROR,
                    format!("{SKILL_TOOL_NAME}: load skill {name:?}: {err}"),
                )
            }
        };
        let mut out = loaded.body.trim().to_string();
        if out.is_empty() && loaded.files.is_empty() {
            out = format!("Skill {name:?} has an empty body.");
        }
        if !loaded.files.is_empty() {
            out = append_file_list(out, &loaded.files);
        }
        let (text, cut) = truncate(&out, effective_limits(self.limits));
        let mut result = ToolResult::ok(text);
        result.truncated = cut;
        result.meta.insert("skill".into(), json!(name));
        result
            .meta
            .insert("files".into(), json!(loaded.files.len()));
        result
    }
}

impl SkillTool {
    fn available(&self) -> String {
        if self.names.is_empty() {
            return "none".to_string();
        }
        self.names.join(", ")
    }

    /// Reads one auxiliary file from inside the skill directory.
    ///
    /// The containment check is a security boundary, not a convenience: a skill
    /// is third-party content, and a prompt-injected model must not be able to
    /// turn `{"name":"x","file":"../../config.json"}` into a read of the user's
    /// configuration. `resolve` cleans the path and rechecks it after resolving
    /// symlinks, so both `..` and a symlink out of the directory are refused.
    fn read_file(&self, skill: &Skill, want: &str) -> ToolResult {
        if skill.dir.trim().is_empty() {
            return ToolResult::error(
                CODE_NOT_FOUND,
                format!(
                    "{SKILL_TOOL_NAME}: skill {:?} has no directory on disk",
                    skill.name
                ),
            );
        }
        let path = match resolve(&skill.dir, want) {
            Ok(path) => path,
            Err(err) => {
                if forge_tool::is_outside(&err) {
                    return ToolResult::error(
                        CODE_OUTSIDE_WORKSPACE,
                        format!(
                            "{SKILL_TOOL_NAME}: {want:?} is outside the skill directory of {:?}; only files inside it can be read",
                            skill.name
                        ),
                    );
                }
                return ToolResult::error(
                    CODE_NOT_FOUND,
                    format!("{SKILL_TOOL_NAME}: {}", describe(&err)),
                );
            }
        };
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) => {
                return ToolResult::error(
                    CODE_NOT_FOUND,
                    format!("{SKILL_TOOL_NAME}: {want:?}: {err}"),
                )
            }
        };
        if meta.is_dir() {
            return ToolResult::error(
                CODE_IO_ERROR,
                format!(
                    "{SKILL_TOOL_NAME}: {want:?} is a directory in skill {:?}, not a file; its files are: {}",
                    skill.name,
                    if skill.files.is_empty() {
                        "none".to_string()
                    } else {
                        skill.files.join(", ")
                    }
                ),
            );
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                return ToolResult::error(
                    CODE_IO_ERROR,
                    format!("{SKILL_TOOL_NAME}: read {want:?}: {err}"),
                )
            }
        };
        let text = String::from_utf8_lossy(&bytes).to_string();
        let (output, cut) = truncate(&text, effective_limits(self.limits));
        let mut result = ToolResult::ok(output);
        result.truncated = cut;
        result.meta.insert("skill".into(), json!(skill.name));
        result.meta.insert("file".into(), json!(want));
        result.meta.insert("bytes".into(), json!(bytes.len()));
        result
    }
}

fn append_file_list(body: String, files: &[String]) -> String {
    let mut out = body;
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("Files in this skill's directory: ");
    out.push_str(&files.join(", "));
    out
}

fn describe(err: &forge_tool::PathError) -> String {
    format!("{err}")
}

fn skill_description(names: &[String], by_name: &BTreeMap<String, Skill>) -> String {
    let mut out = String::from(
        "Load an installed skill: the full instructions for a task, kept in a skill directory. Call with {\"name\": ...}; pass \"file\" to read one auxiliary file from that skill's directory.",
    );
    if names.is_empty() {
        out.push_str(" No skills are installed in this session.");
        return out;
    }
    out.push_str(" Available skills:");
    for (index, name) in names.iter().enumerate() {
        if index == MAX_SKILLS_IN_DESCRIPTION {
            // Sorted order makes the truncation stable across runs.
            out.push_str(&format!(
                " …and {} more (call with a name to use one).",
                names.len() - index
            ));
            break;
        }
        let description = by_name
            .get(name)
            .map(|skill| skill.description.trim().to_string())
            .unwrap_or_default();
        if description.is_empty() {
            out.push_str(&format!(" {name}."));
        } else {
            out.push_str(&format!(" {name} ({description})."));
        }
    }
    out
}

fn skill_tool(set: SkillSet, limits: Limits) -> SkillTool {
    let mut by_name = BTreeMap::new();
    let mut names = Vec::new();
    for skill in set.skills {
        if skill.name.trim().is_empty() {
            continue;
        }
        // `scan` already resolves duplicates (first root wins); this only stops
        // a hand-built set from silently shadowing one.
        if by_name.contains_key(&skill.name) {
            continue;
        }
        names.push(skill.name.clone());
        by_name.insert(skill.name.clone(), skill);
    }
    names.sort();
    let description = skill_description(&names, &by_name);
    SkillTool {
        by_name,
        names,
        description,
        limits,
    }
}

// ---------------------------------------------------------------------------
// Loggers
// ---------------------------------------------------------------------------

/// A logger that collects diagnostics for the caller to print. Both
/// `forge-mcp` and `forge-subagent` define their own narrow `Logger` trait on
/// purpose (neither may depend on a CLI type), so this implements both.
#[derive(Clone, Default)]
pub struct NoteLogger {
    notes: Arc<Mutex<Vec<String>>>,
}

impl NoteLogger {
    pub fn take(&self) -> Notes {
        match self.notes.lock() {
            Ok(mut notes) => std::mem::take(&mut *notes),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }

    fn push(&self, level: &str, message: &str) {
        if let Ok(mut notes) = self.notes.lock() {
            notes.push(format!("{level}: {message}"));
        }
    }
}

impl forge_mcp::Logger for NoteLogger {
    fn debug(&self, message: &str) {
        // Debug output is protocol chatter; it is only interesting when
        // something is wrong, and then the warning says so.
        let _ = message;
    }

    fn warn(&self, message: &str) {
        self.push("mcp", message);
    }
}

impl forge_subagent::Logger for NoteLogger {
    fn info(&self, message: &str) {
        self.push("subagent", message);
    }

    fn warn(&self, message: &str) {
        self.push("subagent", message);
    }

    fn error(&self, message: &str) {
        self.push("subagent", message);
    }
}

/// Wires the frozen `Executor` into the agent's `ToolRunner` boundary.
struct ExecutorRunner {
    executor: Executor,
}

impl forge_agent::ToolRunner for ExecutorRunner {
    fn specs(&self) -> Vec<ToolSpec> {
        self.executor.specs()
    }

    fn run(&self, call: forge_llm::ToolCall) -> forge_core::Result<forge_agent::ToolResult> {
        let input = Call::from_tool_call(&call, self.executor.workspace());
        let out = self.executor.run(input);
        Ok(forge_agent::ToolResult {
            output: out.output,
            is_error: out.is_error,
            code: out.code,
            truncated: out.truncated,
            meta: if out.meta.is_empty() {
                None
            } else {
                Some(Value::Object(out.meta))
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

/// Builds the full tool set for one command.
pub fn build(options: BuildOptions<'_>) -> WiredTools {
    let mut notes: Notes = Vec::new();
    let logger = NoteLogger::default();

    // --- Platform tools ----------------------------------------------------
    // The network client is built once per command, not per turn: TLS roots are
    // expensive to assemble, and a bad CA file must be reported once rather than
    // surfacing as a failing `web_fetch` in the middle of a conversation.
    let mut tool_options = forge_tool::Options::new(options.limits);
    tool_options.data_dir = options.data_dir.clone();
    match forge_net::Client::new(forge_net::ClientOptions {
        ca_file: options.cfg.ca_file.as_deref().map(PathBuf::from),
        ..forge_net::ClientOptions::default()
    }) {
        Ok(client) => {
            let client = Arc::new(client);
            tool_options.net = Some(Arc::clone(&client));
            let endpoint = options.cfg.search.endpoint.trim();
            if !endpoint.is_empty() {
                tool_options.search = Some(forge_tool::SearchConfig {
                    endpoint: endpoint.to_string(),
                    key: options.cfg.search.key.clone(),
                });
            }
        }
        Err(err) => {
            // No client means no `web_fetch` and no `web_search`. Say so once:
            // an absent tool is discoverable, a tool that always fails is not.
            notes.push(format!("web tools disabled: {err}"));
        }
    }

    let registry = Arc::new(Registry::new(forge_tool::all(&tool_options)));
    let mut clients: Vec<Arc<Mutex<Box<dyn Client>>>> = Vec::new();
    let mut ask_rules: Vec<forge_tool::Rule> = Vec::new();

    // --- MCP ---------------------------------------------------------------
    let mut contributed: BTreeSet<String> = BTreeSet::new();
    for server in options.cfg.usable_mcp_servers() {
        let mut spec = McpServer {
            name: server.name.clone(),
            transport: transport_of(server.effective_transport()),
            command: server.command.clone(),
            env: server.env.clone(),
            url: server.url.clone(),
            headers: server
                .headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect(),
            timeout_ms: if server.timeout_ms > 0 {
                server.timeout_ms as u64
            } else {
                0
            },
        };
        if spec.name.trim().is_empty() {
            spec.name = server.name.clone();
        }

        let mut client = match forge_mcp::dial(&spec, Arc::new(logger.clone())) {
            Ok(client) => client,
            Err(err) => {
                // One bad server must not cost the operator the other servers
                // or the turn: say which one and continue.
                notes.push(format!("mcp server skipped: {:?}: {err}", server.name,));
                continue;
            }
        };
        if let Err(err) = client.initialize() {
            notes.push(format!(
                "mcp server skipped: {:?}: initialize: {err}",
                server.name
            ));
            let _ = client.close();
            continue;
        }
        let defs = match client.tools() {
            Ok(defs) => defs,
            Err(err) => {
                notes.push(format!(
                    "mcp server skipped: {:?}: list tools: {err}",
                    server.name
                ));
                let _ = client.close();
                continue;
            }
        };
        let handle: Arc<Mutex<Box<dyn Client>>> = Arc::new(Mutex::new(client));
        clients.push(Arc::clone(&handle));
        for def in defs {
            let remote = def.name.trim().to_string();
            if remote.is_empty() {
                // A nameless remote tool cannot be called or named.
                continue;
            }
            let name = mcp_tool_name(&server.name, &remote);
            if !contributed.insert(name.clone()) {
                // A server listing the same tool twice would give the registry
                // two entries with one name, and registration panics on that.
                continue;
            }
            let tool = McpTool {
                client: Arc::clone(&handle),
                server: server.name.clone(),
                remote: remote.clone(),
                spec: mcp_spec(&name, &server.name, &remote, &def),
                name: name.clone(),
                limits: options.limits,
            };
            registry.register_arc(Arc::new(tool));
            // Every remote tool asks, whatever the sandbox mode says: the call
            // leaves this process and the operator should see it first. The
            // rule is prepended by `merge`, so it beats the mode defaults.
            ask_rules.push(forge_tool::Rule::new(name, "", ACTION_ASK));
        }
    }

    // --- Skills ------------------------------------------------------------
    let roots: Vec<PathBuf> = options
        .cfg
        .skill_roots()
        .iter()
        .map(|root| PathBuf::from(root.as_str().trim()))
        .filter(|root| !root.as_os_str().is_empty())
        .collect();
    if !roots.is_empty() {
        match scan(&roots) {
            Ok(set) => {
                for warning in &set.warnings {
                    notes.push(format!("skill: {warning}"));
                }
                for skipped in &set.skipped {
                    notes.push(format!("skill skipped: {skipped}"));
                }
                if !set.skills.is_empty() {
                    registry.register(skill_tool(set, options.limits));
                }
            }
            Err(err) => notes.push(format!("skill: scan: {}", err.message())),
        }
    }

    // --- Policy ------------------------------------------------------------
    let base = forge_tool::default_rules(&options.sandbox);
    let extra = if ask_rules.is_empty() {
        None
    } else {
        Some(Rules::from_rules(ask_rules))
    };
    let policy: Arc<dyn Policy> = Arc::new(merge(Some(&base), extra.as_ref()));

    // --- Subagent ----------------------------------------------------------
    // Only when the operator asked for it: `maxDepth` defaults to zero, and a
    // tool nobody configured is a tool that spends a turn discovering that.
    if options.cfg.subagent.max_depth > 0 {
        let parent_runner: Arc<dyn forge_agent::ToolRunner> = Arc::new(ExecutorRunner {
            executor: Executor::new(
                Arc::clone(&registry),
                ExecutorOptions {
                    policy: Some(Arc::clone(&policy)),
                    approver: None,
                    limits: options.limits,
                    workspace: options.workspace.clone(),
                },
            ),
        });
        let mut sub = forge_subagent::Options::new(
            Arc::clone(&options.provider),
            options.data_dir.clone(),
            options.workspace.clone(),
            options.model.clone(),
        );
        sub.system = options.system.clone();
        sub.tools = Some(Arc::clone(&parent_runner));
        sub.max_depth = options.cfg.subagent.max_depth as usize;
        sub.parent_session_id = options.parent_session.clone();
        sub.logger = Arc::new(logger.clone());
        sub.policy = Some(Arc::clone(&policy));
        sub.limits = options.limits;
        match forge_subagent::Runner::new(sub) {
            Ok(runner) => {
                let runner = Arc::new(runner);
                registry.register_arc(runner.tool());
            }
            Err(err) => notes.push(format!("subagent tool skipped: {}", err.message())),
        }
    }

    // Diagnostics the bridges collected (a child process that complained, a
    // skill that was skipped) are the operator's business.
    notes.extend(logger.take());

    WiredTools {
        registry,
        policy,
        notes,
        approver: options.approver,
        limits: options.limits,
        workspace: options.workspace,
        clients,
    }
}

/// Maps the config's transport spelling onto the client's enum. Anything that
/// is not `http` is stdio, which is the config default and the only other
/// transport the contract defines.
fn transport_of(value: &str) -> Transport {
    if value.trim().eq_ignore_ascii_case("http") {
        Transport::Http
    } else {
        Transport::Stdio
    }
}

fn mcp_spec(name: &str, server: &str, remote: &str, def: &ToolDef) -> ToolSpec {
    let description = def.description.trim();
    let text = if description.is_empty() {
        format!("Remote tool {remote:?} on the MCP server {server:?} (the server supplied no description).")
    } else {
        // Provenance matters: the model should know this capability lives
        // outside the harness, because a failure here is the remote's fault.
        format!("{description}\n\n(Remote tool {remote:?} on MCP server {server:?}. It runs outside forge and asks for approval every time.)")
    };
    let schema = if def.input_schema.is_object() {
        def.input_schema.clone()
    } else {
        serde_json::from_str(EMPTY_OBJECT_SCHEMA).expect("the empty schema is a constant")
    };
    ToolSpec {
        name: name.to_string(),
        description: text,
        schema,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_mcp::Error as McpError;
    use forge_tool::DefaultLimits;

    /// A client stand-in that records the calls it received.
    struct FakeClient {
        tools: Vec<ToolDef>,
        answer: std::result::Result<CallResult, String>,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        closed: Arc<Mutex<bool>>,
    }

    impl Client for FakeClient {
        fn initialize(&mut self) -> std::result::Result<forge_mcp::ServerInfo, McpError> {
            Ok(forge_mcp::ServerInfo {
                name: "fake".into(),
                version: "1".into(),
            })
        }

        fn tools(&mut self) -> std::result::Result<Vec<ToolDef>, McpError> {
            Ok(self.tools.clone())
        }

        fn call(&mut self, name: &str, args: Value) -> std::result::Result<CallResult, McpError> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((name.to_string(), args.clone()));
            match &self.answer {
                Ok(result) => Ok(result.clone()),
                Err(text) if text == "gone" => Err(McpError::gone("child exited")),
                Err(text) if text == "timeout" => Err(McpError::timeout("no answer")),
                Err(text) => Err(McpError::protocol(text.clone())),
            }
        }

        fn close(&mut self) -> std::result::Result<(), McpError> {
            *self.closed.lock().expect("closed lock") = true;
            Ok(())
        }

        fn clone_handle(&self) -> Box<dyn Client> {
            Box::new(FakeClient {
                tools: self.tools.clone(),
                answer: self.answer.clone(),
                calls: Arc::clone(&self.calls),
                closed: Arc::clone(&self.closed),
            })
        }
    }

    /// What a fake MCP tool remembers: one `(tool, args)` pair per call.
    type RecordedCalls = Arc<Mutex<Vec<(String, Value)>>>;

    fn tool_over(answer: std::result::Result<CallResult, String>) -> (McpTool, RecordedCalls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let client = FakeClient {
            tools: Vec::new(),
            answer,
            calls: Arc::clone(&calls),
            closed: Arc::new(Mutex::new(false)),
        };
        let name = mcp_tool_name("files", "read.file");
        let def = ToolDef {
            name: "read.file".into(),
            description: "read a file".into(),
            input_schema: json!({"type":"object"}),
        };
        let tool = McpTool {
            client: Arc::new(Mutex::new(Box::new(client) as Box<dyn Client>)),
            server: "files".into(),
            remote: "read.file".into(),
            spec: mcp_spec(&name, "files", "read.file", &def),
            name,
            limits: DefaultLimits,
        };
        (tool, calls)
    }

    #[test]
    fn escapes_every_segment_into_a_policy_safe_name() {
        assert_eq!(escape_mcp_name("files"), "files");
        assert_eq!(escape_mcp_name("read_file"), "read__file");
        assert_eq!(escape_mcp_name("read.file"), "read_dot_file");
        // Uppercase is escaped, not lower-cased: `Read` must not collide with
        // a real `read`.
        assert_eq!(escape_mcp_name("Read"), "_u52_ead");
        // `mcp__` + escape(server) + `__` + escape(remote): the separator is the
        // doubled underscore, so the prefix itself is `mcp__`.
        assert_eq!(
            mcp_tool_name("my.server", "do_thing"),
            "mcp__my_dot_server__do__thing"
        );
    }

    #[test]
    fn renders_text_and_summarises_binary_blocks() {
        let text = Content {
            type_: "text".into(),
            text: Some("hello".into()),
            ..Content::default()
        };
        let image = Content {
            type_: "image".into(),
            data: Some("A".repeat(400)),
            mime_type: Some("image/png".into()),
            ..Content::default()
        };
        let unknown = Content {
            type_: "audio".into(),
            text: None,
            ..Content::default()
        };
        assert_eq!(render_block(&text), "hello");
        assert_eq!(
            render_block(&image),
            "[image image/png, about 300 bytes; payload not shown]"
        );
        assert_eq!(
            render_block(&unknown),
            "[audio unknown type, about 0 bytes; payload not shown]"
        );
    }

    #[test]
    fn passes_arguments_verbatim_and_maps_failure_codes() {
        let (tool, calls) = tool_over(Ok(CallResult::default()));
        let out = tool.call(Call::new(
            "c1",
            &tool.name,
            json!({"path":"a","x":[1,2]}),
            ".",
        ));
        // No content from the server is said out loud, not silently empty.
        assert!(out.output.contains("returned no content"), "{}", out.output);
        assert_eq!(
            calls.lock().expect("calls").as_slice(),
            &[("read.file".to_string(), json!({"path":"a","x":[1,2]}))]
        );

        let (gone, _) = tool_over(Err("gone".into()));
        let out = gone.call(Call::new("c1", &gone.name, json!({}), "."));
        assert_eq!(out.code, CODE_MCP_SERVER_GONE);
        assert!(out.is_error);
        // The advice is part of the output: the model, not the operator,
        // decides whether to retry.
        assert!(out.output.contains("Do not retry this tool"));

        let (slow, _) = tool_over(Err("timeout".into()));
        let out = slow.call(Call::new("c1", &slow.name, json!({}), "."));
        assert_eq!(out.code, CODE_TIMEOUT);

        let (broken, _) = tool_over(Err("bad frame".into()));
        let out = broken.call(Call::new("c1", &broken.name, json!({}), "."));
        assert_eq!(out.code, CODE_MCP_CALL_FAILED);
    }

    #[test]
    fn a_server_side_tool_error_keeps_its_text() {
        let result = CallResult {
            content: vec![Content {
                type_: "text".into(),
                text: Some("path does not exist".into()),
                ..Content::default()
            }],
            is_error: true,
        };
        let (tool, _) = tool_over(Ok(result));
        let out = tool.call(Call::new("c1", &tool.name, json!({}), "."));
        assert!(out.is_error);
        assert_eq!(out.code, CODE_MCP_TOOL_ERROR);
        assert!(out.output.contains("path does not exist"));
        assert_eq!(out.meta.get("blocks").and_then(Value::as_u64), Some(1));
    }

    #[test]
    fn null_arguments_become_an_object() {
        let (tool, calls) = tool_over(Ok(CallResult {
            content: vec![Content {
                type_: "text".into(),
                text: Some("ok".into()),
                ..Content::default()
            }],
            is_error: false,
        }));
        tool.call(Call::new("c1", &tool.name, Value::Null, "."));
        assert_eq!(
            calls.lock().expect("calls")[0].1,
            json!({}),
            "an MCP call with no arguments still needs an object"
        );
    }

    fn skill(name: &str, description: &str, dir: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            dir: dir.to_string(),
            path: format!("{dir}/SKILL.md"),
            body: String::new(),
            files: Vec::new(),
        }
    }

    #[test]
    fn skill_description_lists_names_and_truncates_stably() {
        let set = SkillSet {
            skills: vec![skill("beta", "second", ""), skill("alpha", "", "")],
            skipped: Vec::new(),
            warnings: Vec::new(),
        };
        let tool = skill_tool(set, DefaultLimits);
        assert_eq!(tool.names, vec!["alpha", "beta"]);
        assert!(tool.description.contains(" alpha."));
        assert!(tool.description.contains(" beta (second)."));

        let many: Vec<Skill> = (0..30)
            .map(|index| skill(&format!("s{index:02}"), "", ""))
            .collect();
        let tool = skill_tool(
            SkillSet {
                skills: many,
                skipped: Vec::new(),
                warnings: Vec::new(),
            },
            DefaultLimits,
        );
        assert!(
            tool.description.contains("…and 6 more"),
            "{}",
            tool.description
        );
        assert!(!tool.description.contains("s24"));
    }

    #[test]
    fn missing_skill_name_is_bad_args_and_unknown_name_is_not_found() {
        let tool = skill_tool(
            SkillSet {
                skills: vec![skill("alpha", "", "")],
                skipped: Vec::new(),
                warnings: Vec::new(),
            },
            DefaultLimits,
        );
        let out = tool.call(Call::new("c1", SKILL_TOOL_NAME, json!({}), "."));
        assert_eq!(out.code, ERR_BAD_ARGS);
        assert!(out.output.contains("alpha"));

        let out = tool.call(Call::new(
            "c1",
            SKILL_TOOL_NAME,
            json!({"name":"nope"}),
            ".",
        ));
        assert_eq!(out.code, CODE_NOT_FOUND);
        assert!(out.output.contains("alpha"));
    }

    #[test]
    fn skill_file_read_refuses_to_escape_the_skill_directory() {
        let root = std::env::temp_dir().join(format!("forge-skill-{}", std::process::id()));
        let dir = root.join("alpha");
        std::fs::create_dir_all(&dir).expect("create skill dir");
        std::fs::write(dir.join("SKILL.md"), "---\nname: alpha\n---\nbody\n").expect("write");
        std::fs::write(root.join("secret.txt"), "outside").expect("write");

        let tool = skill_tool(
            SkillSet {
                skills: vec![skill("alpha", "", dir.to_str().expect("utf-8 path"))],
                skipped: Vec::new(),
                warnings: Vec::new(),
            },
            DefaultLimits,
        );
        let out = tool.call(Call::new(
            "c1",
            SKILL_TOOL_NAME,
            json!({"name":"alpha","file":"../secret.txt"}),
            ".",
        ));
        assert_eq!(out.code, CODE_OUTSIDE_WORKSPACE, "{}", out.output);

        let out = tool.call(Call::new(
            "c1",
            SKILL_TOOL_NAME,
            json!({"name":"alpha","file":"SKILL.md"}),
            ".",
        ));
        assert!(out.is_ok(), "{}", out.output);
        assert!(out.output.contains("alpha"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
