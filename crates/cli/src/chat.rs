//! `forge chat` — run one agent turn and stream the answer to stdout.
//!
//! Layer L3. Ported from `forge-v2/cmd/forge/chat.go`. The event log is the only
//! truth: every fact is appended before it is reported, so a restart never loses
//! what happened. This module owns argument parsing, session recovery and the
//! approval conversation; the agent crate owns the turn/step event order.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use forge_agent::{Agent, EventLog, Options as AgentOptions, ToolResult as AgentToolResult};
use forge_config::Config;
use forge_core::{Error, ErrorKind, Event, EventType, Result};
use forge_llm::{Client as LlmClient, Options as LlmOptions, Provider, Usage};
use forge_sandbox::{mode_from_str, Mode as SandboxMode, Spec as SandboxSpec};
use forge_session::{Projection, Store};
use forge_tool::builtin;
use forge_tool::executor::{Executor, ExecutorOptions};
use forge_tool::policy::default_rules;
use forge_tool::registry::Registry;
use forge_tool::truncate::DefaultLimits;
use forge_tool::{ApprovalRequest, Approver, Call as ToolCall, Policy};
use serde_json::{json, Value};

use crate::reorder_flags;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_INTERRUPTED: i32 = 130;

const CHAT_USAGE: &str = "usage: forge chat [flags] [prompt]\n\n\
Run one agent turn and stream the answer to stdout. With no prompt argument the prompt is read from stdin until EOF, so `echo hi | forge chat` works.\n\n\
  --model <name>     model to use (otherwise config.json's \"model\")\n\
  --session <id>     session id to use (generated if absent)\n\
  --resume <id>      resume an existing session, adopting its open turn\n\
  --fork <id>        fork a session at its last completed turn\n\
  --system <text>    override the system prompt\n\
  --approve <mode>   never, always or prompt (default never)\n\
  --no-tools         run with no tools\n\
  --sandbox <mode>   read-only, workspace-write or full-access\n\
  --json             emit one JSON report instead of the answer\n\
  --no-compact       do not compact the log at 200 events\n\n\
exit: 0 turn completed, 1 error, 2 usage error, 130 cancelled\n";

const CHAT_STDIN_HINT: &str =
    "reading prompt from stdin; end with Ctrl+Z then Enter (Windows) or Ctrl+D\n";
const NO_APPROVER_HINT: &str = "forge chat: a tool call was denied because --approve=never: rerun with --approve=always to allow every call, or --approve=prompt to answer each one (the turn itself succeeded)\n";

const CHAT_VALUE_FLAGS: &[&str] = &[
    "model", "session", "resume", "fork", "system", "approve", "sandbox",
];

/// Whether bare `forge` arguments are a prompt (contract §2.5).
pub fn is_prompt_invocation(args: &[String]) -> bool {
    let Some(first) = args.first() else {
        return false;
    };
    if args.len() > 1 {
        return true;
    }
    if first.starts_with('-') || first.chars().any(|c| c.is_whitespace()) {
        return true;
    }
    !is_ascii_word(first)
}

fn is_ascii_word(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii() && c >= '!' && !c.is_whitespace())
}

#[derive(Debug, Default)]
struct ChatOptions {
    model: String,
    session: String,
    resume: String,
    fork: String,
    system: String,
    approve: String,
    sandbox: String,
    no_tools: bool,
    json: bool,
    no_compact: bool,
    positionals: Vec<String>,
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = write!(stderr, "{message}");
    let _ = write!(stderr, "{CHAT_USAGE}");
    EXIT_USAGE
}

fn parse_chat_flags(
    args: &[String],
    stderr: &mut dyn Write,
) -> std::result::Result<ChatOptions, i32> {
    let reordered = reorder_flags(args, CHAT_VALUE_FLAGS);
    let mut opts = ChatOptions::default();
    let mut i = 0;
    let mut end_flags = false;
    while i < reordered.len() {
        let arg = &reordered[i];
        if end_flags {
            opts.positionals.push(arg.clone());
            i += 1;
            continue;
        }
        if arg == "--" {
            end_flags = true;
            i += 1;
            continue;
        }
        if arg == "-h" || arg == "--help" {
            let _ = write!(stderr, "{CHAT_USAGE}");
            return Err(EXIT_OK);
        }
        if !arg.starts_with('-') || arg == "-" {
            opts.positionals.push(arg.clone());
            i += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n.trim_start_matches('-'), Some(v.to_string())),
            None => (arg.trim_start_matches('-'), None),
        };
        let value_flag = CHAT_VALUE_FLAGS.contains(&name);
        let value = if let Some(v) = inline {
            Some(v)
        } else if value_flag && i + 1 < reordered.len() {
            i += 1;
            Some(reordered[i].clone())
        } else {
            None
        };
        match name {
            "model" | "session" | "resume" | "fork" | "system" | "approve" | "sandbox" => {
                let val = value.ok_or_else(|| {
                    usage_error(
                        stderr,
                        &format!("forge chat: flag needs an argument: -{name}\n"),
                    )
                })?;
                match name {
                    "model" => opts.model = val,
                    "session" => opts.session = val,
                    "resume" => opts.resume = val,
                    "fork" => opts.fork = val,
                    "system" => opts.system = val,
                    "approve" => opts.approve = val,
                    "sandbox" => opts.sandbox = val,
                    _ => unreachable!(),
                }
            }
            "no-tools" => opts.no_tools = true,
            "json" => opts.json = true,
            "no-compact" => opts.no_compact = true,
            _ => {
                return Err(usage_error(
                    stderr,
                    &format!("forge chat: unknown flag -{name}\n"),
                ));
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn valid_session_id(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn new_session_id() -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    // Local time is not available in core; format UTC like Go's local stamp.
    let stamp = format_epoch_stamp(secs);
    let mut bytes = [0u8; 3];
    getrandom::fill(&mut bytes).map_err(|e| Error::new(ErrorKind::Io, format!("random: {e}")))?;
    Ok(format!(
        "{stamp}-{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2]
    ))
}

fn format_epoch_stamp(secs: i64) -> String {
    // Civil-from-days conversion (UTC), enough for a sortable session id.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

fn resolve_model(flag: &str, cfg: &Config) -> std::result::Result<String, i32> {
    let model = if flag.is_empty() {
        cfg.model.trim()
    } else {
        flag.trim()
    };
    if model.is_empty() {
        Err(runtime_error_msg(
            "no model configured: pass --model <name> or set \"model\" in config.json",
        ))
    } else {
        Ok(model.to_string())
    }
}

fn runtime_error_msg(message: &str) -> i32 {
    eprintln!("forge chat: {message}");
    EXIT_FAILURE
}

fn load_config() -> std::result::Result<Config, i32> {
    let path =
        forge_config::default_path().map_err(|e| runtime_error_msg(&format!("config: {e}")))?;
    let cfg = Config::load(&path).map_err(|e| runtime_error_msg(&format!("config: {e}")))?;
    if let Err(err) = cfg.validate() {
        if !err.message().contains("mcp") {
            return Err(runtime_error_msg(&format!("config: {err}")));
        }
        eprintln!("forge: {err} (continuing without those servers)");
    }
    Ok(cfg)
}

fn resolve_sandbox(flag: &str, cfg: &Config) -> std::result::Result<SandboxMode, i32> {
    let raw = if flag.is_empty() {
        cfg.sandbox.as_str()
    } else {
        flag
    };
    mode_from_str(raw).ok_or_else(|| {
        usage_error_msg(&format!(
            "invalid --sandbox {raw:?}: use read-only, workspace-write or full-access"
        ))
    })
}

fn usage_error_msg(message: &str) -> i32 {
    eprintln!("forge chat: {message}");
    let _ = std::io::stderr().write_all(CHAT_USAGE.as_bytes());
    EXIT_USAGE
}

fn workspace_dir() -> std::result::Result<PathBuf, i32> {
    std::env::current_dir().map_err(|e| runtime_error_msg(&format!("workspace: {e}")))
}

fn approval_mode(raw: &str) -> std::result::Result<ApproveMode, i32> {
    match raw {
        "" | "never" => Ok(ApproveMode::Never),
        "always" => Ok(ApproveMode::Always),
        "prompt" => Ok(ApproveMode::Prompt),
        other => Err(usage_error_msg(&format!(
            "invalid --approve {other:?}: use never, always or prompt"
        ))),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ApproveMode {
    Never,
    Always,
    Prompt,
}

/// Run `forge chat`. Kept as a thin wrapper so the whole surface is testable.
pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    match run_inner(args, stdout, stderr) {
        Ok(code) => code,
        Err(code) => code,
    }
}

fn run_inner(
    args: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> std::result::Result<i32, i32> {
    let opts = parse_chat_flags(args, stderr)?;

    if !opts.resume.is_empty() && !opts.fork.is_empty() {
        return Err(runtime_error_str(stderr, "--resume and --fork contradict each other: resume continues a session, fork copies it into a new one"));
    }
    if !opts.resume.is_empty() && !opts.session.is_empty() {
        return Err(runtime_error_str(
            stderr,
            &format!(
                "--resume {} already names the session: drop --session {}",
                opts.resume, opts.session
            ),
        ));
    }
    if !opts.resume.is_empty() && !valid_session_id(&opts.resume) {
        return Err(runtime_error_str(stderr, &format!("invalid --resume {:?}: use 1-128 letters, digits, '.', '_' or '-', starting with a letter or digit", opts.resume)));
    }
    if !opts.fork.is_empty() && !valid_session_id(&opts.fork) {
        return Err(runtime_error_str(stderr, &format!("invalid --fork {:?}: use 1-128 letters, digits, '.', '_' or '-', starting with a letter or digit", opts.fork)));
    }
    let approve = approval_mode(&opts.approve)?;
    if approve == ApproveMode::Prompt && !std::io::stdin().is_terminal() {
        return Err(runtime_error_str(stderr, "--approve=prompt needs a terminal on stdin: use --approve=always to allow every tool call, or run forge chat in a terminal"));
    }

    let cfg = load_config()?;
    let prompt = chat_prompt(&opts, stderr)?;
    if prompt.is_empty() {
        return Err(usage_error_str(
            stderr,
            "empty prompt: pass it as an argument or pipe it on stdin",
        ));
    }
    let model = resolve_model(&opts.model, &cfg)?;
    let session_id = if !opts.resume.is_empty() {
        opts.resume.clone()
    } else if !opts.session.is_empty() {
        if !valid_session_id(&opts.session) {
            return Err(runtime_error_str(stderr, &format!("invalid --session {:?}: use 1-128 letters, digits, '.', '_' or '-', starting with a letter or digit", opts.session)));
        }
        opts.session.clone()
    } else {
        new_session_id().map_err(|e| runtime_error_str(stderr, &format!("{e}")))?
    };
    let sandbox = resolve_sandbox(&opts.sandbox, &cfg)?;
    let workspace = workspace_dir()?;
    let workspace_str = workspace.to_string_lossy().to_string();

    execute_turn(
        &opts,
        &cfg,
        &prompt,
        &model,
        &session_id,
        sandbox,
        &workspace_str,
        approve,
        stdout,
        stderr,
    )
}

fn runtime_error_str(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "forge chat: {message}");
    EXIT_FAILURE
}

fn usage_error_str(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "forge chat: {message}");
    let _ = stderr.write_all(CHAT_USAGE.as_bytes());
    EXIT_USAGE
}

fn chat_prompt(opts: &ChatOptions, stderr: &mut dyn Write) -> std::result::Result<String, i32> {
    if !opts.positionals.is_empty() {
        return Ok(opts.positionals.join(" "));
    }
    if !std::io::stdin().is_terminal() {
        let _ = stderr.write_all(CHAT_STDIN_HINT.as_bytes());
    }
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| runtime_error_str(stderr, &format!("read stdin: {e}")))?;
    while buf.ends_with('\n') || buf.ends_with('\r') {
        buf.pop();
    }
    Ok(buf)
}

// ---- approval recording ----------------------------------------------------
//
// The Rust agent writes the turn/step/tool events but no approval events, and
// the tool executor writes no events at all. This adapter supplies the missing
// half of the consent conversation: approval/request is durable before the
// question is shown, and approval/decision is written after the answer (or
// after the answer failed), mirroring Go's `recordingApprover`.

/// An approver that writes the approval conversation around an inner approver.
pub(crate) struct RecordingApprover {
    log: Arc<dyn EventLog>,
    session: String,
    inner: Arc<dyn Approver>,
    name: String,
    /// When true a successful answer is recorded by somebody else (the HTTP
    /// handler in `forge web`), so only the failure path is written here.
    inner_records_decision: bool,
}

impl RecordingApprover {
    pub(crate) fn new(
        log: Arc<dyn EventLog>,
        session: String,
        inner: Arc<dyn Approver>,
        name: impl Into<String>,
        inner_records_decision: bool,
    ) -> Self {
        Self {
            log,
            session,
            inner,
            name: name.into(),
            inner_records_decision,
        }
    }
}

/// `forge_core::new_id` is 16 random bytes (32 hex); the contract asks for a
/// fresh opaque id with exactly that shape.
fn approval_id() -> std::result::Result<String, String> {
    forge_core::new_id().map_err(|e| e.message().to_string())
}

/// Classify a failed ask. The three values are different audit facts and must
/// never collapse to the approver's name.
fn decided_by_for_error(err: &str) -> &'static str {
    let lower = err.to_ascii_lowercase();
    if lower.contains("cancel") {
        "cancelled"
    } else if lower.contains("timeout")
        || lower.contains("deadline")
        || lower.contains("no decision in")
    {
        "timeout"
    } else {
        "error"
    }
}

fn step_event(session: &str, turn: u32, step: u32, typ: EventType, payload: Value) -> Event {
    let mut event = Event::new(session, typ, Some(payload));
    event.turn = turn;
    event.step = step;
    event
}

/// Locate the tool/call event that belongs to a call so the approval events
/// carry the same turn and step. Falls back to (0, 0) if the log is unreadable.
fn call_turn_step(log: &dyn EventLog, call_id: &str) -> (u32, u32) {
    let mut found = (0, 0);
    if let Ok(events) = log.read_all() {
        for event in &events {
            if event.typ == EventType::TOOL_CALL.as_str() {
                if let Some(payload) = &event.payload {
                    if payload.get("callId").and_then(Value::as_str) == Some(call_id) {
                        found = (event.turn, event.step);
                    }
                }
            }
        }
    }
    found
}

impl Approver for RecordingApprover {
    fn approve(&self, req: ApprovalRequest) -> std::result::Result<bool, String> {
        let id = approval_id()?;
        let (turn, step) = call_turn_step(self.log.as_ref(), &req.call.id);
        let args = if req.call.args.is_null() {
            json!({})
        } else {
            req.call.args.clone()
        };
        let request = step_event(
            &self.session,
            turn,
            step,
            EventType::APPROVAL_REQUEST,
            json!({
                "approvalId": id,
                "callId": req.call.id,
                "tool": req.call.name,
                "args": args,
                "reason": req.reason,
            }),
        );
        self.log
            .append(&request)
            .map_err(|e| e.message().to_string())?;

        let answer = self.inner.approve(req.clone());
        // A self-recording inner approver writes its own decision on a real
        // answer; only its failure path needs the wrapper's audit line.
        if self.inner_records_decision && answer.is_ok() {
            return answer;
        }

        let (decision, decided_by, note) = match &answer {
            Ok(yes) => (
                if *yes { "allow" } else { "deny" },
                self.name.as_str(),
                None,
            ),
            Err(err) => (
                "deny",
                decided_by_for_error(err),
                Some(format!("no answer: {err}")),
            ),
        };
        let mut payload = json!({
            "approvalId": id,
            "callId": req.call.id,
            "decision": decision,
            "decidedBy": decided_by,
        });
        if let Some(note) = note {
            payload["note"] = json!(note);
        }
        let decision_event = step_event(
            &self.session,
            turn,
            step,
            EventType::APPROVAL_DECISION,
            payload,
        );
        match self.log.append(&decision_event) {
            Ok(_) => answer,
            Err(_) if answer.is_err() => answer,
            Err(e) => Err(e.message().to_string()),
        }
    }

    fn records_own_decision(&self) -> bool {
        true
    }
}

/// Unattended consent: answer yes without a human. Used by --approve=always.
pub(crate) struct AlwaysApprover;

impl Approver for AlwaysApprover {
    fn approve(&self, _request: ApprovalRequest) -> std::result::Result<bool, String> {
        Ok(true)
    }
}

/// Ask on stderr and read one line from stdin. Used by --approve=prompt.
struct PromptApprover;

impl Approver for PromptApprover {
    fn approve(&self, request: ApprovalRequest) -> std::result::Result<bool, String> {
        let mut stderr = std::io::stderr().lock();
        let _ = write!(
            stderr,
            "forge chat: approve tool={} in {}? {} [y/N] ",
            request.call.name, request.call.workspace, request.reason
        );
        let _ = stderr.flush();
        let mut line = String::new();
        let n = std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("cancelled: read stdin: {e}"))?;
        if n == 0 {
            return Err("cancelled: stdin closed".to_string());
        }
        let answer = line.trim();
        let yes = answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes");
        let _ = writeln!(
            stderr,
            "forge chat: approval: {}",
            if yes { "allow" } else { "deny" }
        );
        Ok(yes)
    }
}

/// Adapter from the frozen tool executor to the agent's ToolRunner boundary.
pub(crate) struct ToolRunnerAdapter {
    pub(crate) executor: Executor,
}

impl forge_agent::ToolRunner for ToolRunnerAdapter {
    fn specs(&self) -> Vec<forge_llm::ToolSpec> {
        self.executor.specs()
    }

    fn run(&self, call: forge_llm::ToolCall) -> Result<AgentToolResult> {
        let call = ToolCall::from_tool_call(&call, self.executor.workspace());
        let result = self.executor.run(call);
        Ok(AgentToolResult {
            output: result.output,
            is_error: result.is_error,
            code: result.code,
            truncated: result.truncated,
            meta: if result.meta.is_empty() {
                None
            } else {
                Some(Value::Object(result.meta))
            },
        })
    }
}

struct DeltaState {
    buffer: Vec<u8>,
    wrote: bool,
    last_step: u32,
}

fn agent_usage(log: &Store, turn: u32) -> Usage {
    let mut usage = Usage::default();
    if let Ok(events) = log.read_all() {
        for event in &events {
            if event.turn != turn || event.typ != EventType::ASSISTANT_MESSAGE.as_str() {
                continue;
            }
            if let Some(payload) = &event.payload {
                if let Some(value) = payload.get("usage") {
                    if let Ok(part) = serde_json::from_value::<Usage>(value.clone()) {
                        usage.add(part);
                    }
                }
            }
        }
    }
    usage
}

#[derive(serde::Serialize)]
struct ChatApproval {
    #[serde(rename = "callId")]
    call_id: String,
    tool: String,
    decision: String,
}

fn turn_approvals(log: &Store, turn: u32) -> Vec<ChatApproval> {
    let mut by_approval: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    if let Ok(events) = log.read_all() {
        for event in &events {
            if event.turn != turn || event.payload.is_none() {
                continue;
            }
            let payload = event.payload.as_ref().unwrap();
            let id = payload
                .get("approvalId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            match event.typ.as_str() {
                "approval/request" => {
                    let call_id = payload
                        .get("callId")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let tool = payload
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    by_approval.insert(id, (call_id.to_string(), tool.to_string()));
                }
                "approval/decision" => {
                    let (fallback_call, tool) = by_approval.get(&id).cloned().unwrap_or_default();
                    let call_id = payload
                        .get("callId")
                        .and_then(Value::as_str)
                        .unwrap_or(&fallback_call);
                    let decision = payload
                        .get("decision")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    out.push(ChatApproval {
                        call_id: call_id.to_string(),
                        tool,
                        decision: decision.to_string(),
                    });
                }
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.call_id.cmp(&b.call_id).then(a.tool.cmp(&b.tool)));
    out
}

fn turn_ask_denied(log: &Store, turn: u32) -> bool {
    let mut found = false;
    if let Ok(events) = log.read_all() {
        for event in &events {
            if found || event.turn != turn || event.typ != EventType::TOOL_RESULT.as_str() {
                continue;
            }
            if let Some(payload) = &event.payload {
                if payload.get("ok").and_then(Value::as_bool) == Some(false)
                    && payload.get("code").and_then(Value::as_str)
                        == Some(forge_tool::executor::CODE_NO_APPROVER)
                {
                    found = true;
                }
            }
        }
    }
    found
}

/// Execute one agent turn over the real provider and log, then report it.
/// Ten parameters mirror the ten independent inputs of the Go `runChat` tail;
/// grouping them in a struct would hide that this is the single turn boundary.
#[allow(clippy::too_many_arguments)]
fn execute_turn(
    opts: &ChatOptions,
    cfg: &Config,
    prompt: &str,
    model: &str,
    session_id: &str,
    sandbox: SandboxMode,
    workspace: &str,
    approve: ApproveMode,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> std::result::Result<i32, i32> {
    let client = LlmClient::new(LlmOptions::default())
        .map_err(|e| runtime_error_str(stderr, &format!("no LLM provider: {}", e.message())))?;
    let provider: Arc<dyn Provider> = Arc::new(client);

    let mut spec = SandboxSpec::new(sandbox);
    spec.workspace = workspace.to_string();
    // Hold the enforcer for the turn: its Drop closes the sandbox hook.
    let _enforcer = forge_sandbox::new(spec, None)
        .map_err(|e| runtime_error_str(stderr, &format!("sandbox: {}", e.message())))?;

    let data_dir = PathBuf::from(cfg.data_dir.trim());
    let log_dir = if data_dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        data_dir
    }
    .join("sessions");
    std::fs::create_dir_all(&log_dir)
        .map_err(|e| runtime_error_str(stderr, &format!("create sessions dir: {e}")))?;
    let log_path = log_dir.join(format!("{session_id}.jsonl"));

    if !opts.fork.is_empty() {
        let parent = log_dir.join(format!("{}.jsonl", opts.fork));
        if !parent.exists() {
            return Err(runtime_error_str(
                stderr,
                &format!("no such session {:?}: there is nothing to fork", opts.fork),
            ));
        }
        let fork_opts = forge_session::fork::ForkOptions {
            parent_path: parent,
            target_path: log_path.clone(),
            target_id: session_id.to_string(),
            at: 0,
        };
        match forge_session::fork::fork(fork_opts) {
            Ok(result) => {
                let _ = writeln!(
                    stderr,
                    "forge chat: forked {} at seq {} into session {} ({} events)",
                    opts.fork, result.forked_at_seq, session_id, result.events
                );
            }
            Err(e) => {
                return Err(runtime_error_str(stderr, e.message()));
            }
        }
    }

    if !opts.resume.is_empty() && !log_path.exists() {
        return Err(runtime_error_str(
            stderr,
            &format!(
                "no such session {:?}: {} does not exist",
                opts.resume,
                log_path.display()
            ),
        ));
    }

    let store = Arc::new(
        Store::open(&log_path, forge_session::Options::default())
            .map_err(|e| runtime_error_str(stderr, &format!("open session: {}", e.message())))?,
    );

    if !opts.resume.is_empty() {
        let events = store
            .read_all()
            .map_err(|e| runtime_error_str(stderr, &format!("read session: {}", e.message())))?;
        let snapshot = Projection::project(&events)
            .map_err(|e| runtime_error_str(stderr, &format!("read session: {}", e.message())))?;
        if let Some(pending) = snapshot.pending_approvals.first() {
            return Err(runtime_error_str(
                stderr,
                &format!(
                    "session {:?} is waiting for approval {:?}: answer it in `forge web`, or continue from a copy with `forge chat --fork {}`",
                    session_id, pending.id, session_id
                ),
            ));
        }
        if let Some(last) = snapshot.turns.last() {
            if last.end_seq == 0 {
                let event = step_event(
                    session_id,
                    last.number,
                    0,
                    EventType::TURN_END,
                    json!({
                        "reason": forge_agent::TURN_CANCELLED,
                        "steps": last.steps,
                        "cancelledBy": "cli:recovery",
                    }),
                );
                store.append(&event).map_err(|e| {
                    runtime_error_str(stderr, &format!("close open turn: {}", e.message()))
                })?;
                let _ = writeln!(
                    stderr,
                    "forge chat: recovered session {}: closed turn {}, which a dead process left open",
                    session_id, last.number
                );
            }
        }
    }

    let before_seq = store.last_seq();

    let registry = Registry::new(builtin::all(DefaultLimits));
    let rules = default_rules(sandbox.as_str());
    let policy: Arc<dyn Policy> = Arc::new(rules);
    let store_log: Arc<dyn EventLog> = store.clone();
    let approver: Option<Arc<dyn Approver>> = match approve {
        ApproveMode::Never => None,
        ApproveMode::Always => Some(Arc::new(RecordingApprover::new(
            Arc::clone(&store_log),
            session_id.to_string(),
            Arc::new(AlwaysApprover),
            "flag:--approve=always",
            false,
        ))),
        ApproveMode::Prompt => Some(Arc::new(RecordingApprover::new(
            Arc::clone(&store_log),
            session_id.to_string(),
            Arc::new(PromptApprover),
            "user",
            false,
        ))),
    };
    let executor = Executor::new(
        Arc::new(registry),
        ExecutorOptions {
            policy: Some(policy),
            approver,
            workspace: workspace.to_string(),
            ..ExecutorOptions::default()
        },
    );
    let tool_names = executor
        .specs()
        .iter()
        .map(|s| s.name.clone())
        .collect::<Vec<_>>();
    let tools: Option<Arc<dyn forge_agent::ToolRunner>> = if opts.no_tools {
        None
    } else {
        Some(Arc::new(ToolRunnerAdapter { executor }))
    };

    let _ = writeln!(
        stderr,
        "forge chat: model={model} sandbox={} tools={} approve={}",
        sandbox.as_str(),
        tool_names.len(),
        match approve {
            ApproveMode::Never => "never",
            ApproveMode::Always => "always",
            ApproveMode::Prompt => "prompt",
        }
    );
    let _ = writeln!(stderr, "forge chat: session={session_id}");
    let _ = writeln!(stderr, "forge chat: log={}", log_path.display());

    let delta = Arc::new(Mutex::new(DeltaState {
        buffer: Vec::new(),
        wrote: false,
        last_step: 0,
    }));
    let on_delta: forge_agent::DeltaHook = {
        let delta = Arc::clone(&delta);
        Arc::new(move |_turn, step, text| {
            let mut state = delta.lock().unwrap_or_else(|e| e.into_inner());
            if state.wrote && step != state.last_step {
                state.buffer.push(b'\n');
            }
            state.last_step = step;
            if !text.is_empty() {
                state.wrote = true;
            }
            state.buffer.extend_from_slice(text.as_bytes());
        })
    };

    let log: Arc<dyn EventLog> = store.clone();
    let mut agent_opts = AgentOptions::new(
        provider,
        log,
        model.to_string(),
        session_id.to_string(),
        workspace.to_string(),
    );
    agent_opts.system = opts.system.clone();
    agent_opts.tools = tools;
    if !opts.json {
        agent_opts.on_delta = Some(on_delta);
    }
    let agent = Agent::new(agent_opts)
        .map_err(|e| runtime_error_str(stderr, &format!("agent: {}", e.message())))?;
    let (result, run_err) = agent.run(prompt);

    let usage = agent_usage(&store, result.turn);
    let approvals = turn_approvals(&store, result.turn);
    let appended = store.last_seq().saturating_sub(before_seq);
    let cancelled = run_err
        .as_ref()
        .map(forge_agent::is_cancelled)
        .unwrap_or(false)
        || result.reason == forge_agent::TURN_CANCELLED
        || result.reason == forge_agent::STOP_CANCELLED;

    if opts.json {
        let report = json!({
            "turn": result.turn,
            "reason": result.reason,
            "steps": result.steps,
            "text": result.text,
            "usage": {
                "inputTokens": usage.input_tokens,
                "outputTokens": usage.output_tokens,
                "cacheReadTokens": usage.cache_read_tokens,
                "cacheWriteTokens": usage.cache_write_tokens,
            },
            "events": appended,
            "tools": tool_names,
            "approvals": approvals,
        });
        let _ = writeln!(
            stdout,
            "{}",
            serde_json::to_string(&report).unwrap_or_default()
        );
    } else {
        let state = delta.lock().unwrap_or_else(|e| e.into_inner());
        let _ = stdout.write_all(&state.buffer);
        drop(state);
        if !result.text.is_empty() && !result.text.ends_with('\n') {
            let _ = stdout.write_all(b"\n");
        }
    }

    let _ = writeln!(
        stderr,
        "forge chat: turn={} steps={} reason={} usage in={} out={} total={}",
        result.turn,
        result.steps,
        result.reason,
        usage.input_tokens,
        usage.output_tokens,
        usage.input_tokens + usage.output_tokens
    );
    if approve == ApproveMode::Never && turn_ask_denied(&store, result.turn) {
        let _ = stderr.write_all(NO_APPROVER_HINT.as_bytes());
    }

    if cancelled {
        let _ = stderr.write_all(b"\n");
        return Ok(EXIT_INTERRUPTED);
    }
    if let Some(err) = run_err {
        let _ = writeln!(stderr, "forge chat: {}", err.message());
        return Ok(EXIT_FAILURE);
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store() -> (Arc<Store>, PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "forge-chat-test-{nanos}-{}.jsonl",
            std::process::id()
        ));
        let store = Arc::new(Store::open(&path, forge_session::Options::default()).unwrap());
        (store, path)
    }

    /// An approver that always fails with a specific message — no real provider.
    struct FailingApprover(String);
    impl Approver for FailingApprover {
        fn approve(&self, _req: ApprovalRequest) -> std::result::Result<bool, String> {
            Err(self.0.clone())
        }
    }

    /// A cancelled/timed-out/error ask is an audit fact with its own decidedBy;
    /// it must never reuse the approver's name. This pins lead decision m00378.
    #[test]
    fn cancelled_ask_records_decided_by_cancelled() {
        let (store, _path) = temp_store();
        let log: Arc<dyn EventLog> = store.clone();
        let approver = RecordingApprover::new(
            Arc::clone(&log),
            "s-test".to_string(),
            Arc::new(FailingApprover(
                "cancelled: operator stopped the ask".to_string(),
            )),
            "web",
            false,
        );
        let request = ApprovalRequest {
            call: ToolCall::new("call-1", "fs_read", json!({}), "D:/workspace"),
            reason: "need to read".to_string(),
        };
        // The wrapper records the audit lines and propagates the inner error.
        let answer = approver.approve(request);
        assert_eq!(
            answer,
            Err("cancelled: operator stopped the ask".to_string())
        );

        let events = store.read_all().unwrap();
        let decisions: Vec<_> = events
            .iter()
            .filter(|e| e.typ == EventType::APPROVAL_DECISION.as_str())
            .collect();
        assert_eq!(decisions.len(), 1, "exactly one decision line");
        let payload = decisions[0].payload.as_ref().unwrap();
        assert_eq!(
            payload.get("decidedBy").and_then(|v| v.as_str()),
            Some("cancelled")
        );
        assert_eq!(
            payload.get("decision").and_then(|v| v.as_str()),
            Some("deny")
        );
        assert_eq!(
            payload.get("callId").and_then(|v| v.as_str()),
            Some("call-1")
        );
        let requests: Vec<_> = events
            .iter()
            .filter(|e| e.typ == EventType::APPROVAL_REQUEST.as_str())
            .collect();
        assert_eq!(requests.len(), 1, "request is durable before the ask");
        assert_eq!(
            requests[0]
                .payload
                .as_ref()
                .unwrap()
                .get("approvalId")
                .and_then(|v| v.as_str()),
            payload.get("approvalId").and_then(|v| v.as_str()),
            "request and decision share one id"
        );
    }

    #[test]
    fn timeout_and_generic_errors_get_distinct_decided_by() {
        assert_eq!(
            decided_by_for_error("timeout: no decision in 600s"),
            "timeout"
        );
        assert_eq!(decided_by_for_error("context deadline exceeded"), "timeout");
        assert_eq!(decided_by_for_error("error: broken pipe"), "error");
    }

    #[test]
    fn bare_prompt_detection() {
        // Contract §2.5: only a single bare ascii word is a mistyped command.
        assert!(!is_prompt_invocation(&["frobnicate".into()]));
        assert!(!is_prompt_invocation(&["summarize".into()]));
        // Anything with whitespace, non-ascii, or a flag is a prompt.
        assert!(is_prompt_invocation(&["explain this code".into()]));
        assert!(is_prompt_invocation(&["你好".into()]));
        assert!(is_prompt_invocation(&["--model".into(), "x".into()]));
        assert!(!is_prompt_invocation(&[]));
    }
}
