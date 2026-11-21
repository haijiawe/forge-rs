//! `hearth web` — serve the browser UI on loopback.
//!
//! Layer L3. Ported from `forge-go/cmd/forge/web.go`. The server owns every log
//! append; this runner owns only the model loop and answering approvals by
//! polling the log through a DecisionWatcher. Prompt events are written by the
//! HTTP layer, so the agent adopts the open turn (`prompt_recorded = true`).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hearth_agent::{Agent, EventLog, Options as AgentOptions, ProviderSummarizer};
use hearth_config::Config;
use hearth_core::{event::parse_log, Event, EventType, Result};
use hearth_llm::{Client as LlmClient, Options as LlmOptions, Provider};
use hearth_protocol::decision;
use hearth_protocol::{CreateSessionRequest, CreateSessionResponse};
use hearth_sandbox::{mode_from_str, Mode as SandboxMode, Spec as SandboxSpec};
use hearth_server::{CancelToken, Options as ServerOptions, TurnContext, TurnRunner};
use hearth_session::DecisionWatcher;

use hearth_tool::truncate::DefaultLimits;
use hearth_tool::{ApprovalRequest, Approver};
use serde_json::json;

use crate::chat::{AlwaysApprover, RecordingApprover, ToolRunnerAdapter};
use crate::reorder_flags;
use crate::VERSION;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

const WEB_USAGE: &str = "usage: hearth web [flags]\n\n\
Serve the browser UI on loopback (default 127.0.0.1:8420).\n\n\
  --addr <host:port>  listen address (loopback only)\n\
  --model <name>      model to use (otherwise config.json's \"model\")\n\
  --workspace <dir>   workspace root (default current directory)\n\
  --no-browser        do not open the browser\n\
  --sandbox <mode>    read-only, workspace-write or full-access\n\
  --approve <mode>    web, never or always (default web)\n\n\
exit: 0 clean shutdown, 1 error, 2 usage error, 130 interrupted\n";

const WEB_VALUE_FLAGS: &[&str] = &["addr", "model", "workspace", "sandbox", "approve"];
const DEFAULT_ADDR: &str = "127.0.0.1:8420";
/// Below this many log records the web surface does not attempt compaction: a
/// short session would lose a turn or two while paying for a provider call and
/// appending a record that can never be removed.
const WEB_COMPACT_MESSAGE_EVENTS: u64 = 200;
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
const APPROVAL_POLL: Duration = Duration::from_millis(150);

#[derive(Debug, Default)]
struct WebOptions {
    addr: String,
    model: String,
    workspace: String,
    no_browser: bool,
    sandbox: String,
    approve: String,
    positionals: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebApproveMode {
    Web,
    Never,
    Always,
}

impl WebApproveMode {
    fn as_str(self) -> &'static str {
        match self {
            WebApproveMode::Web => "web",
            WebApproveMode::Never => "never",
            WebApproveMode::Always => "always",
        }
    }
}

fn parse_web_flags(
    args: &[String],
    stderr: &mut dyn Write,
) -> std::result::Result<WebOptions, i32> {
    let reordered = reorder_flags(args, WEB_VALUE_FLAGS);
    let mut opts = WebOptions {
        addr: DEFAULT_ADDR.to_string(),
        ..WebOptions::default()
    };
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
            let _ = write!(stderr, "{WEB_USAGE}");
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
        let value = if let Some(v) = inline {
            Some(v)
        } else if WEB_VALUE_FLAGS.contains(&name) && i + 1 < reordered.len() {
            i += 1;
            Some(reordered[i].clone())
        } else {
            None
        };
        match name {
            "addr" | "model" | "workspace" | "sandbox" | "approve" => {
                let val = value.ok_or_else(|| {
                    let _ = writeln!(stderr, "hearth web: flag needs an argument: -{name}");
                    EXIT_USAGE
                })?;
                match name {
                    "addr" => opts.addr = val,
                    "model" => opts.model = val,
                    "workspace" => opts.workspace = val,
                    "sandbox" => opts.sandbox = val,
                    "approve" => opts.approve = val,
                    _ => unreachable!(),
                }
            }
            "no-browser" => opts.no_browser = true,
            _ => {
                let _ = writeln!(stderr, "hearth web: unknown flag -{name}");
                let _ = write!(stderr, "{WEB_USAGE}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "hearth web: {message}");
    let _ = write!(stderr, "{WEB_USAGE}");
    EXIT_USAGE
}

fn runtime_error(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "hearth web: {message}");
    EXIT_FAILURE
}

fn web_approve_mode(raw: &str, stderr: &mut dyn Write) -> std::result::Result<WebApproveMode, i32> {
    match raw {
        "" | "web" => Ok(WebApproveMode::Web),
        "never" => Ok(WebApproveMode::Never),
        "always" => Ok(WebApproveMode::Always),
        "prompt" => Err(usage_error(
            stderr,
            "--approve=prompt reads stdin, which `hearth web` does not ask on: use --approve=web to answer in the browser",
        )),
        other => Err(usage_error(
            stderr,
            &format!("unknown --approve value {other:?}: use web, never or always"),
        )),
    }
}

fn load_config(stderr: &mut dyn Write) -> std::result::Result<Config, i32> {
    let path = hearth_config::default_path()
        .map_err(|e| runtime_error(stderr, &format!("config: {e}")))?;
    let cfg = Config::load(&path).map_err(|e| runtime_error(stderr, &format!("config: {e}")))?;
    if let Err(err) = cfg.validate() {
        if !err.message().contains("mcp") {
            return Err(runtime_error(stderr, &format!("config: {err}")));
        }
        let _ = writeln!(
            stderr,
            "hearth web: {err} (continuing without those servers)"
        );
    }
    Ok(cfg)
}

/// An `EventLog` that routes every append through the server, so the HTTP and
/// WebSocket layers see the same lines the agent writes. `read_all` is a plain
/// file read: the log is append-only and the server is the only writer.
struct ServerLog {
    server: Arc<hearth_server::Server>,
    session: String,
}

impl EventLog for ServerLog {
    fn append(&self, event: &Event) -> Result<u64> {
        self.server.append_event(&self.session, event)
    }

    fn last_seq(&self) -> u64 {
        self.server.last_seq(&self.session)
    }

    fn read_all(&self) -> Result<Vec<Event>> {
        let path = self.server.path(&self.session);
        match std::fs::read_to_string(path) {
            Ok(text) => parse_log(&text),
            // A brand-new session has no log yet; that is an empty projection.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(hearth_core::Error::new(
                hearth_core::ErrorKind::Io,
                format!("web: read log: {e}"),
            )),
        }
    }
}

/// Answers one approval by polling the session log for the decision the HTTP
/// handler appended for a browser click (`decidedBy: user`).
struct WebApprover {
    path: PathBuf,
    cancel: hearth_agent::Cancel,
}

impl Approver for WebApprover {
    fn approve(&self, req: ApprovalRequest) -> std::result::Result<bool, String> {
        let mut watcher = DecisionWatcher::new(&self.path);
        let deadline = Instant::now() + APPROVAL_TIMEOUT;
        loop {
            if self.cancel.is_cancelled() {
                return Err("cancelled: web approval interrupted".to_string());
            }
            match watcher.poll(&req.call.id) {
                Ok(Some(decision)) => return Ok(decision == decision::ALLOW),
                Ok(None) => {}
                Err(e) => return Err(format!("error: {e}")),
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timeout: no decision in {}s",
                    APPROVAL_TIMEOUT.as_secs()
                ));
            }
            thread::sleep(APPROVAL_POLL);
        }
    }

    fn records_own_decision(&self) -> bool {
        // The HTTP handler appends the decision for a browser click. The CLI must
        // not write a second one on a real answer; only the failure path is ours.
        true
    }
}

/// What every turn needs in order to rebuild its tool set: the config the
/// external bridges read, the data directory a subagent session must land in,
/// and the system prompt. Bundled so `WebRunner::new` stays readable and so the
/// four values a turn shares cannot drift apart.
struct TurnEnv {
    cfg: Arc<Config>,
    data_dir: PathBuf,
    system: String,
}

/// One turn runner per session, supplied to the server by the factory closure.
struct WebRunner {
    server: Arc<hearth_server::Server>,
    provider: Arc<dyn Provider>,
    /// The host the provider points at, for `request/context`. Empty when the
    /// base URL did not name one, and then the event omits the field.
    provider_name: String,
    model: String,
    workspace: String,
    sandbox: SandboxMode,
    /// Installed on every child process this runner's tool sets start. `None` only
    /// in tests: a runner without an enforcer would start commands unrestricted
    /// while `--sandbox` claimed otherwise.
    exec_sandbox: Option<Arc<dyn hearth_exec::Sandbox>>,
    approve: WebApproveMode,
    env: TurnEnv,
    /// The agent's cancellation flag; `cancel()` flips it without appending.
    agent_cancel: Arc<Mutex<hearth_agent::Cancel>>,
    /// Bridges the server's per-turn token to the agent's flag.
    turn_cancel: Arc<Mutex<Option<CancelToken>>>,
    /// One resident agent per session, built on the first turn and reused after
    /// that. See [`ResidentHost`] for why this is an object and not a loop.
    hosts: Mutex<HashMap<String, Arc<ResidentHost>>>,
    /// How many times a resident host was built, not reused.
    ///
    /// The map above cannot answer that question: it is keyed by session, so a
    /// runner that rebuilt the host on every turn would still hold one entry.
    /// This counter is the only honest witness that the tool registry, the MCP
    /// servers and the plugins were dialled once per session rather than once
    /// per message.
    host_builds: std::sync::atomic::AtomicUsize,
}

impl WebRunner {
    // Eight arguments, each a distinct collaborator; a builder would hide the
    // one ordering that matters (the server must exist before a runner can).
    #[allow(clippy::too_many_arguments)]
    fn new(
        server: Arc<hearth_server::Server>,
        provider: Arc<dyn Provider>,
        provider_name: String,
        model: String,
        workspace: String,
        sandbox: SandboxMode,
        approve: WebApproveMode,
        env: TurnEnv,
    ) -> Self {
        Self {
            server,
            provider,
            provider_name,
            model,
            workspace,
            sandbox,
            exec_sandbox: None,
            approve,
            env,
            agent_cancel: Arc::new(Mutex::new(hearth_agent::Cancel::default())),
            turn_cancel: Arc::new(Mutex::new(None)),
            hosts: Mutex::new(HashMap::new()),
            host_builds: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Hand the enforcer to every tool set this runner builds.
    ///
    /// Separate from `new` because only `hearth web` has one to give: the unit
    /// tests build runners that never start a child process.
    fn with_exec_sandbox(mut self, sandbox: Option<Arc<dyn hearth_exec::Sandbox>>) -> Self {
        self.exec_sandbox = sandbox;
        self
    }
}

impl WebRunner {
    /// Fold the oldest complete history into one summary record, when there is
    /// something worth folding. Returns whether a record landed.
    ///
    /// The read goes through a short-lived [`hearth_session::Store`] because the file
    /// is the interface, and the write goes through `Server::append_event` because
    /// the server owns the seq counter: a second writer holding its own counter
    /// would mint duplicate seqs.
    ///
    /// Compaction is best effort. A failure is reported to the caller, which ignores
    /// it: the log is correct whether or not this ran, and the turn should not fail
    /// because a summary could not be produced.
    fn compact(&self, session_id: &str) -> Result<bool> {
        let path = self.server.path(session_id);
        let store = hearth_session::Store::open(&path, hearth_session::Options::default())?;
        let mut events: Vec<Event> = Vec::new();
        store.read_from(1, |event| {
            events.push(event.clone());
            Ok(())
        })?;
        // Release the file handle before asking the server to append: two handles on
        // one log are two seq counters, which is the bug this split exists to avoid.
        drop(store);

        if (events.len() as u64) < WEB_COMPACT_MESSAGE_EVENTS {
            return Ok(false);
        }
        let plan = hearth_session::plan_compaction(&events, 0)?;
        if plan.is_empty() {
            return Ok(false);
        }
        let covered: Vec<Event> = events
            .iter()
            .filter(|event| event.seq >= plan.from && event.seq <= plan.to)
            .cloned()
            .collect();
        let summarizer =
            ProviderSummarizer::new(Some(Arc::clone(&self.provider)), self.model.clone());
        // No fallback text: a summary that is not the model's words would become the
        // permanent record of what those events said.
        let summary = summarizer.summarize(&covered)?;
        // An empty summary would replace hundreds of records with nothing: the
        // session would lose its history and gain no account of it. The CLI path
        // gets this guard from `compact_log`; the web path writes the event
        // itself, so it has to check here.
        if summary.trim().is_empty() {
            return Ok(false);
        }
        let payload = json!({
            "summary": summary,
            "replaces": {"from": plan.from, "to": plan.to},
        });
        let event = Event::new(
            session_id.to_string(),
            EventType::COMPACTION_SUMMARY,
            Some(payload),
        );
        self.server.append_event(session_id, &event)?;
        Ok(true)
    }
}

/// One agent, many turns.
///
/// The loop is a pure function of the log, so a fresh `Agent` per turn is
/// correct — but it is not *resident*: every turn rebuilt the tool registry,
/// re-dialled MCP servers, reloaded plugins, and threw away the object the phase
/// machine lives on. This holds one agent per session for as long as the process
/// serves that session, which is what "resident" has to mean if it is to mean
/// anything.
///
/// Two pieces of the loop are fixed when it is built but differ per turn — the
/// delta sink and the steering source, both of which belong to the HTTP request
/// being answered. They read `slot` instead of capturing a context, so one agent
/// can serve many requests without losing the one that is live.
struct ResidentHost {
    agent: Agent,
    cancel: hearth_agent::Cancel,
    slot: Arc<Mutex<Option<TurnContext>>>,
    /// Turns are serialised: two runs at once would interleave two turns in one
    /// log, and the second would read a history the first is still writing.
    lock: Mutex<()>,
    /// Dropped with the host. `WiredTools` owns the plugin host and the MCP
    /// children; dropping it unloads the plugins whose tools this agent exposes.
    _wired: crate::wiring::WiredTools,
}

impl ResidentHost {
    fn turn(&self, ctx: &TurnContext, data_dir: &Path, text: &str) {
        let _guard = self.lock.lock().expect("web: host lock poisoned");
        *self.slot.lock().expect("web: turn slot poisoned") = Some(ctx.clone());
        // The cancellation flag outlives the turn it stopped. Without this the
        // second turn of a session would begin already cancelled, which is the
        // kind of bug that only shows up after an operator presses stop once.
        self.cancel.reset();
        let _ = self.agent.run(text);
        // A session can accumulate work while this turn ran: a background
        // subagent's answer is a `next-turn` message, another process's steer is
        // a `next-step` one, and a second POST is accepted while the first turn
        // is still going. Nothing consumes those until something asks.
        let drained = crate::resident::drain(&self.agent);
        if let Some(err) = &drained.error {
            note_diagnostics(
                data_dir,
                &[format!(
                    "drained turn stopped: {} ({} turn(s) ran)",
                    err.message(),
                    drained.turns
                )],
            );
        } else if drained.capped {
            note_diagnostics(
                data_dir,
                &[format!(
                    "drain stopped at {} turns with work still queued; the next turn will pick it up",
                    crate::resident::MAX_DRAINED_TURNS
                )],
            );
        }
        *self.slot.lock().expect("web: turn slot poisoned") = None;
    }
}

impl WebRunner {
    /// The resident agent for `session_id`, built on first use.
    ///
    /// Everything in the construction is a property of the session — the log, the
    /// approver bound to it, the workspace, the model — so it is safe to keep. The
    /// per-turn parts live in `slot` and in `agent_cancel`.
    fn host_for(&self, session_id: &str) -> Result<Arc<ResidentHost>> {
        let mut hosts = self.hosts.lock().expect("web: hosts lock poisoned");
        if let Some(host) = hosts.get(session_id) {
            *self
                .agent_cancel
                .lock()
                .expect("web: agent_cancel poisoned") = host.cancel.clone();
            return Ok(Arc::clone(host));
        }

        self.host_builds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let cancel = hearth_agent::Cancel::default();
        *self
            .agent_cancel
            .lock()
            .expect("web: agent_cancel poisoned") = cancel.clone();

        let log: Arc<dyn EventLog> = Arc::new(ServerLog {
            server: Arc::clone(&self.server),
            session: session_id.to_string(),
        });
        let log_path = self.server.path(session_id);

        let approver: Option<Arc<dyn Approver>> = match self.approve {
            WebApproveMode::Never => None,
            WebApproveMode::Always => Some(Arc::new(RecordingApprover::new(
                Arc::clone(&log),
                session_id.to_string(),
                Arc::new(AlwaysApprover),
                "flag:--approve=always",
                false,
            ))),
            WebApproveMode::Web => Some(Arc::new(RecordingApprover::new(
                Arc::clone(&log),
                session_id.to_string(),
                Arc::new(WebApprover {
                    path: log_path.clone(),
                    cancel: cancel.clone(),
                }),
                "web",
                true,
            ))),
        };

        let wired = crate::wiring::build(crate::wiring::BuildOptions {
            cfg: self.env.cfg.as_ref(),
            log: Some(log_path.clone()),
            provider: Arc::clone(&self.provider),
            model: self.model.clone(),
            data_dir: self.env.data_dir.clone(),
            workspace: self.workspace.clone(),
            sandbox: self.sandbox.as_str().to_string(),
            exec_sandbox: self.exec_sandbox.clone(),
            approver,
            limits: DefaultLimits,
            system: self.env.system.clone(),
            parent_session: session_id.to_string(),
            goal: false,
        });
        note_diagnostics(&self.env.data_dir, &wired.notes);
        let executor = wired.executor();
        let tools: Option<Arc<dyn hearth_agent::ToolRunner>> =
            Some(Arc::new(ToolRunnerAdapter { executor }));

        let slot: Arc<Mutex<Option<TurnContext>>> = Arc::new(Mutex::new(None));
        let delta_slot = Arc::clone(&slot);
        let on_delta: hearth_agent::DeltaHook = Arc::new(move |turn, step, text| {
            let live = delta_slot.lock().ok().and_then(|guard| guard.clone());
            if let Some(ctx) = live {
                ctx.delta(turn, step, text);
            }
        });
        let steer_slot = Arc::clone(&slot);
        let steering: hearth_agent::SteeringHook = Arc::new(move || {
            steer_slot
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .map(|ctx| ctx.take_steering())
                .unwrap_or_default()
        });

        let mut opts = AgentOptions::new(
            Arc::clone(&self.provider),
            Arc::clone(&log),
            self.model.clone(),
            session_id.to_string(),
            self.workspace.clone(),
        );
        opts.prompt_recorded = true; // the HTTP layer wrote turn/start + user/message
        opts.tools = tools;
        opts.attachments = Some(Arc::new(crate::attachments::parts_hook(
            &self.env.data_dir,
            Some(PathBuf::from(&self.workspace)),
        )));
        opts.hidden = Some(Arc::new(crate::context_tools::hidden_hook(&log_path)));
        opts.hooks = wired.hooks().clone();
        opts.on_delta = Some(on_delta);
        opts.steering = Some(steering);
        opts.cancel = cancel.clone();
        opts.provider_name = self.provider_name.clone();

        let agent = match Agent::new(opts) {
            Ok(agent) => agent,
            Err(e) => {
                return Err(hearth_core::Error::new(
                    hearth_core::ErrorKind::Invalid,
                    format!("web: agent: {}", e.message()),
                ))
            }
        };
        let host = Arc::new(ResidentHost {
            agent,
            cancel,
            slot,
            lock: Mutex::new(()),
            _wired: wired,
        });
        hosts.insert(session_id.to_string(), Arc::clone(&host));
        Ok(host)
    }
}

impl TurnRunner for WebRunner {
    fn run_turn(&self, ctx: &TurnContext, session_id: &str, text: &str) -> Result<()> {
        // Compaction happens at the top of the turn, before the loop reads the log,
        // which means it can never cut into the prompt the user is waiting on: the
        // HTTP layer already wrote turn/start + user/message, so those records are
        // the open tail the planner keeps. The cost is that this turn still sends
        // the long history once; the next one does not.
        let _ = self.compact(session_id);
        // Bridge the server's cancellation token into the agent's flag. The
        // server calls `cancel()` before flipping ctx.cancel, so this is mostly
        // a belt-and-braces path for in-process callers.
        *self.turn_cancel.lock().expect("web: turn_cancel poisoned") = Some(ctx.cancel.clone());
        let host = self.host_for(session_id)?;
        host.turn(ctx, &self.env.data_dir, text);
        Ok(())
    }

    fn cancel(&self) -> Result<()> {
        if let Ok(guard) = self.agent_cancel.lock() {
            guard.cancel_with(hearth_agent::Cause::User);
        }
        Ok(())
    }
}

/// Appends the tool-wiring diagnostics to `<data_dir>/web.log`.
///
/// A skipped MCP server, a skills root that does not exist, a child process
/// that complained: all of it is the operator's business, and none of it may
/// cost a turn. Writing that to stderr from the turn thread is a trap — stderr
/// is a pipe whenever a harness, a CI job or `2>file` owns it, and a pipe whose
/// reader has stopped reading blocks the writer, which here is the turn the
/// client is waiting on. A file has no such back-pressure.
fn note_diagnostics(data_dir: &Path, notes: &[String]) {
    if notes.is_empty() {
        return;
    }
    let path = data_dir.join("web.log");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        // Nowhere to write is not a reason to fail a turn; the notes are
        // diagnostics, not results.
        return;
    };
    for note in notes {
        let _ = writeln!(file, "hearth web: {note}");
    }
}

fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Run `hearth web`.
pub fn run(args: &[String], _stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    run_inner(args, stderr)
}

fn run_inner(args: &[String], stderr: &mut dyn Write) -> i32 {
    let opts = match parse_web_flags(args, stderr) {
        Ok(opts) => opts,
        Err(code) => return code,
    };
    if !opts.positionals.is_empty() {
        return usage_error(
            stderr,
            &format!(
                "unexpected argument {:?}: the command takes flags only",
                opts.positionals[0]
            ),
        );
    }
    if opts.addr.trim().is_empty() {
        return runtime_error(stderr, "--addr must not be empty");
    }
    let approve = match web_approve_mode(&opts.approve, stderr) {
        Ok(mode) => mode,
        Err(code) => return code,
    };

    let cfg = match load_config(stderr) {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };

    let workspace = if opts.workspace.trim().is_empty() {
        match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => return runtime_error(stderr, &format!("workspace: {e}")),
        }
    } else {
        PathBuf::from(opts.workspace.trim())
    };
    let workspace = match workspace.canonicalize() {
        Ok(dir) if dir.is_dir() => dir,
        Ok(dir) => {
            return runtime_error(
                stderr,
                &format!("workspace {} is not a directory", dir.display()),
            )
        }
        Err(e) => return runtime_error(stderr, &format!("workspace: {e}")),
    };
    let workspace_str = workspace.to_string_lossy().to_string();

    let model = if opts.model.trim().is_empty() {
        cfg.model.trim().to_string()
    } else {
        opts.model.trim().to_string()
    };
    if model.is_empty() {
        return runtime_error(
            stderr,
            "no model configured: pass --model <name> or set \"model\" in config.json",
        );
    }

    // The loop cannot see an endpoint through `Arc<dyn Provider>`, so the host is
    // captured here while the client is still concrete. It is a label for
    // `request/context`, not a routing decision.
    let (provider, provider_name): (Arc<dyn Provider>, String) =
        match LlmClient::new(LlmOptions::default()) {
            Ok(client) => {
                let label = crate::provider_label(client.base_url());
                (Arc::new(client), label)
            }
            Err(e) => return runtime_error(stderr, &format!("no LLM provider: {}", e.message())),
        };

    let sandbox_raw = if opts.sandbox.trim().is_empty() {
        cfg.sandbox.as_str()
    } else {
        opts.sandbox.trim()
    };
    let sandbox = match mode_from_str(sandbox_raw) {
        Some(mode) => mode,
        None => {
            return usage_error(
                stderr,
                &format!(
                "invalid --sandbox {sandbox_raw:?}: use read-only, workspace-write or full-access"
            ),
            )
        }
    };

    let mut enforcer_spec = SandboxSpec::new(sandbox);
    enforcer_spec.workspace = workspace_str.clone();
    // Same grants as `chat`: a browser session's tools are the same tools.
    let grants = crate::grants::effective(&cfg);
    enforcer_spec.extra_read = grants.read;
    enforcer_spec.extra_write = grants.write;
    enforcer_spec.allow_net = grants.allow_net;
    // Held, not dropped: the terminal installs it on every command it runs. It
    // used to be bound to `_enforcer` and never used, which meant `--sandbox`
    // constrained nothing about a child process — only which tools the policy
    // layer would offer. Keeping the value here is what makes the terminal
    // stricter than the `shell` tool rather than merely equivalent.
    let enforcer: Arc<dyn hearth_sandbox::Enforcer> = match hearth_sandbox::new(enforcer_spec, None)
    {
        Ok(enforcer) => Arc::from(enforcer),
        Err(e) => return runtime_error(stderr, &format!("sandbox: {}", e.message())),
    };

    let data_dir = if cfg.data_dir.trim().is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(cfg.data_dir.trim())
    };
    let mut server_options = ServerOptions {
        addr: opts.addr.trim().to_string(),
        data_dir: PathBuf::from(cfg.data_dir.trim()),
        version: VERSION.to_string(),
        // Saving a buffer is allowed exactly when the session's own sandbox is
        // allowed to write. A read-only session whose tools cannot change a file
        // must not offer an editor that can: the two surfaces would then disagree
        // about what the session is, and the quieter one would win.
        workspace_write: sandbox != SandboxMode::ReadOnly,
        // The terminal is offered on the same judgement as saving a file, and
        // for the same reason: a read-only session that cannot write through its
        // tools must not offer a shell that can, or the quieter surface wins.
        new_terminal: if sandbox == SandboxMode::ReadOnly {
            None
        } else {
            let enforcer = Arc::clone(&enforcer);
            Some(Arc::new(move |_session: &str| {
                Arc::new(crate::terminal::Terminal::new(Arc::clone(&enforcer)))
                    as Arc<dyn hearth_server::api::TerminalRunner>
            }))
        },
        // A snapshot, not a query: plugins are loaded once, so what the model
        // can reach does not change while this process runs. Discovering it here
        // keeps `hearth-server` free of the four crates that would be needed.
        capabilities: crate::capabilities::report(
            &cfg,
            &crate::capabilities::Facts {
                mode: sandbox.as_str(),
                workspace: &workspace_str,
                model: &model,
                data_dir: &data_dir,
            },
        ),
        ..Default::default()
    };

    // The runner factory needs the server, which does not exist until after
    // Server::new; share a slot and fill it immediately after construction.
    // The runner factory cannot see `cfg` closures cheaply, and every turn
    // rebuilds the tool set (the approver is bound to one session log), so hand
    // the shared pieces over as owned values.
    // The same enforcer the terminal installs, handed to each session's tool set.
    let exec_sandbox: Option<Arc<dyn hearth_exec::Sandbox>> =
        Some(crate::exec_sandbox::tool_sandbox(Arc::clone(&enforcer)));
    let cfg = Arc::new(cfg);
    let server_slot: Arc<Mutex<Option<Arc<hearth_server::Server>>>> = Arc::new(Mutex::new(None));
    {
        let slot = Arc::clone(&server_slot);
        let provider = Arc::clone(&provider);
        let model = model.clone();
        let workspace_str = workspace_str.clone();
        let cfg = Arc::clone(&cfg);
        let data_dir = data_dir.clone();
        let exec_sandbox = exec_sandbox.clone();
        server_options.new_runner = Some(Arc::new(move |_session_id: &str| {
            let server = slot
                .lock()
                .expect("web: server slot poisoned")
                .as_ref()
                .cloned()
                .expect("web: runner created before server was ready");
            Arc::new(
                WebRunner::new(
                    server,
                    Arc::clone(&provider),
                    provider_name.clone(),
                    model.clone(),
                    workspace_str.clone(),
                    sandbox,
                    approve,
                    TurnEnv {
                        cfg: Arc::clone(&cfg),
                        data_dir: data_dir.clone(),
                        system: String::new(),
                    },
                )
                .with_exec_sandbox(exec_sandbox.clone()),
            ) as Arc<dyn TurnRunner>
        }));
    }

    let server = match hearth_server::Server::new(server_options) {
        Ok(server) => Arc::new(server),
        Err(e) => return runtime_error(stderr, &format!("server: {}", e.message())),
    };
    *server_slot.lock().expect("web: server slot poisoned") = Some(Arc::clone(&server));

    let (serve_tx, serve_rx) = std::sync::mpsc::channel::<Result<()>>();
    {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let result = server.listen_and_serve();
            let _ = serve_tx.send(result);
        });
    }

    // Wait for the listener to bind. A non-loopback address fails fast with the
    // server's exact refusal message; that must reach stderr as a runtime error.
    let deadline = Instant::now() + READY_TIMEOUT;
    let bound_addr = loop {
        if let Ok(result) = serve_rx.try_recv() {
            match result {
                Ok(()) => {
                    return runtime_error(stderr, "server stopped before it was ready");
                }
                Err(e) => return runtime_error(stderr, e.message()),
            }
        }
        if let Some(addr) = server.addr() {
            break addr;
        }
        if Instant::now() >= deadline {
            let _ = server.shutdown();
            return runtime_error(stderr, "server did not start within 5s");
        }
        thread::sleep(Duration::from_millis(25));
    };

    let request = CreateSessionRequest {
        workspace: workspace_str.clone(),
        model: Some(model.clone()),
        profile: None,
    };
    let session: CreateSessionResponse = match server.create_session(&request) {
        Ok(response) => response,
        Err(e) => {
            let _ = server.shutdown();
            return runtime_error(stderr, e.message());
        }
    };
    let session_id = session.session.id;
    let base_url = format!("http://{bound_addr}");
    let open_url = format!("{base_url}/?token={}#/s/{session_id}", server.token());

    let _ = writeln!(stderr, "hearth web: serving {base_url}");
    let _ = writeln!(
        stderr,
        "hearth web: session={session_id} model={model} workspace={workspace_str} sandbox={} approve={}",
        sandbox.as_str(),
        approve.as_str()
    );
    let _ = writeln!(stderr, "hearth web: open {open_url}");

    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
    let signals = Arc::new(AtomicUsize::new(0));
    {
        let signals = Arc::clone(&signals);
        let shutdown_tx = shutdown_tx.clone();
        // A second install (e.g. in tests) is harmless; keep the first handler.
        let _ = ctrlc::set_handler(move || {
            let count = signals.fetch_add(1, Ordering::SeqCst) + 1;
            if count >= 2 {
                std::process::exit(130);
            }
            let _ = shutdown_tx.send(());
        });
    }

    if !opts.no_browser {
        let url = open_url.clone();
        thread::spawn(move || open_browser(&url));
    }

    // Serve until Ctrl+C or the server thread ends. A second Ctrl+C exits 130.
    let code = loop {
        match shutdown_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => {
                let _ = server.shutdown();
                break EXIT_OK;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if serve_rx.try_recv().is_ok() {
                    break EXIT_OK;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break EXIT_OK,
        }
    };
    // Drain the server thread so the listener is fully closed before return.
    let _ = serve_rx.recv_timeout(READY_TIMEOUT);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    /// The regression that cost a whole evening: a turn stalled forever because
    /// the tool-wiring note was written to stderr, which was a pipe nobody was
    /// draining. The note must land in a file, and an empty note list must not
    /// create one.
    #[test]
    fn diagnostics_go_to_a_file_and_not_to_stderr() {
        let dir = std::env::temp_dir().join(format!("hearth-web-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        note_diagnostics(&dir, &[]);
        assert!(
            !dir.join("web.log").exists(),
            "an empty note list must not create a diagnostics file"
        );

        note_diagnostics(&dir, &["skill: root missing".to_string()]);
        note_diagnostics(&dir, &["mcp server skipped: boom".to_string()]);
        let text = std::fs::read_to_string(dir.join("web.log")).expect("diagnostics file");
        assert!(text.contains("hearth web: skill: root missing"), "{text}");
        assert!(
            text.contains("hearth web: mcp server skipped: boom"),
            "{text}"
        );
        assert_eq!(
            text.lines().count(),
            2,
            "each note is one line and appends: {text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_and_positional_rejection() {
        let mut err = Vec::new();
        assert_eq!(
            run(&args(&["extra"]), &mut Vec::new(), &mut err),
            EXIT_USAGE
        );
        assert!(String::from_utf8_lossy(&err).contains("takes flags only"));
    }

    #[test]
    fn approve_prompt_is_rejected_with_web_hint() {
        let mut err = Vec::new();
        let code = run(&args(&["--approve", "prompt"]), &mut Vec::new(), &mut err);
        assert_eq!(code, EXIT_USAGE);
        let text = String::from_utf8_lossy(&err);
        assert!(text.contains("--approve=prompt"), "{text}");
        assert!(text.contains("--approve=web"), "{text}");
    }

    #[test]
    fn approve_unknown_is_rejected() {
        let mut err = Vec::new();
        assert_eq!(
            run(&args(&["--approve=bogus"]), &mut Vec::new(), &mut err),
            EXIT_USAGE
        );
        assert!(String::from_utf8_lossy(&err).contains("use web, never or always"));
    }

    #[test]
    fn approve_modes() {
        let mut err = Vec::new();
        assert_eq!(web_approve_mode("", &mut err), Ok(WebApproveMode::Web));
        assert_eq!(web_approve_mode("web", &mut err), Ok(WebApproveMode::Web));
        assert_eq!(
            web_approve_mode("never", &mut err),
            Ok(WebApproveMode::Never)
        );
        assert_eq!(
            web_approve_mode("always", &mut err),
            Ok(WebApproveMode::Always)
        );
    }

    #[test]
    fn parse_flags_after_value_and_equals() {
        let parsed = parse_web_flags(
            &args(&["--model", "m1", "--approve=never", "--no-browser"]),
            &mut Vec::new(),
        )
        .expect("flags parse");
        assert_eq!(parsed.model, "m1");
        assert_eq!(parsed.approve, "never");
        assert!(parsed.no_browser);
        assert_eq!(parsed.addr, DEFAULT_ADDR);
    }

    // -----------------------------------------------------------------------
    // Compaction on the web surface
    // -----------------------------------------------------------------------

    /// A provider that answers every request with one delta and then closes.
    /// The summarizer never offers tools, so a single text delta is a complete
    /// answer for it.
    struct FixedProvider(String);

    struct FixedStream {
        text: Option<String>,
    }

    impl hearth_llm::Stream for FixedStream {
        fn recv(&mut self) -> Result<Option<hearth_llm::Delta>> {
            Ok(self.text.take().map(|text| hearth_llm::Delta {
                text,
                ..Default::default()
            }))
        }
        fn close(&mut self) {}
    }

    impl Provider for FixedProvider {
        fn stream(&self, _request: &hearth_llm::Request) -> Result<Box<dyn hearth_llm::Stream>> {
            Ok(Box::new(FixedStream {
                text: Some(self.0.clone()),
            }))
        }
    }

    /// The web surface must fold history the same way the CLI does: one summary
    /// record that replaces a run of complete turns, written through the server
    /// because the server owns the seq counter.
    ///
    /// The claim this test pins is the one the README used to get wrong. The
    /// summary lands *in the current turn*, appended after the HTTP layer's
    /// `turn/start` + `user/message`; the plan's `replaces` stops at the last
    /// closed turn, so the open tail is re-applied after the summary by
    /// projection. Nothing is deferred to the next turn.
    #[test]
    fn web_compaction_folds_closed_turns_into_one_summary() {
        let dir = std::env::temp_dir().join(format!("hearth-web-compact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let server = Arc::new(
            hearth_server::Server::new(ServerOptions {
                addr: "127.0.0.1:0".to_string(),
                data_dir: dir.clone(),
                version: "test".to_string(),
                token: "test-token".to_string(),
                ..ServerOptions::default()
            })
            .expect("server"),
        );
        let session = "web_compact_session";
        server
            .append_event(
                session,
                &Event::new(session, EventType::SESSION_START, Some(json!({}))),
            )
            .expect("session/start");

        // 60 complete turns of four events each: comfortably over the 200-record
        // floor and an exact multiple of a turn, so the plan's `to` is the last
        // turn/end and nothing has to be guessed.
        for turn in 1..=60u32 {
            for typ in [
                EventType::TURN_START,
                EventType::USER_MESSAGE,
                EventType::ASSISTANT_MESSAGE,
                EventType::TURN_END,
            ] {
                // The summarizer projects `text`, so a fixture that only carries
                // `turn` projects to zero messages and the summarizer answers an
                // empty string without an error. Seed what the product writes.
                let text = format!("turn {turn} of the fold");
                let payload = match typ {
                    EventType::USER_MESSAGE => json!({ "text": text }),
                    EventType::ASSISTANT_MESSAGE => json!({ "text": text }),
                    _ => json!({ "turn": turn }),
                };
                let mut event = Event::new(session, typ, Some(payload));
                event.turn = turn;
                server.append_event(session, &event).expect("seed event");
            }
        }
        // The HTTP layer writes these two before the runner runs, which is what
        // makes "the summary lands one turn late" false.
        for typ in [EventType::TURN_START, EventType::USER_MESSAGE] {
            let mut event = Event::new(session, typ, Some(json!({ "text": "open" })));
            event.turn = 61;
            server.append_event(session, &event).expect("open tail");
        }

        let runner = WebRunner::new(
            Arc::clone(&server),
            Arc::new(FixedProvider("the fold, in the model's words".to_string())),
            String::new(),
            "fixed-model".to_string(),
            dir.to_string_lossy().to_string(),
            SandboxMode::ReadOnly,
            WebApproveMode::Never,
            TurnEnv {
                cfg: Arc::new(Config::default()),
                data_dir: dir.clone(),
                system: String::new(),
            },
        );
        assert!(
            runner.compact(session).expect("compaction runs"),
            "241 records must produce a summary"
        );

        let log = std::fs::read_to_string(server.path(session)).expect("log");
        let events = parse_log(&log).expect("log parses");
        let summary = events.last().expect("a summary is the last record");
        assert_eq!(
            summary.typ,
            EventType::COMPACTION_SUMMARY.as_str(),
            "tail was {:?}",
            events
                .iter()
                .rev()
                .take(3)
                .map(|event| event.typ.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            summary.payload.as_ref().expect("payload")["summary"],
            json!("the fold, in the model's words")
        );
        // The replacement stops at the last closed turn: turn 61 is still open,
        // so its turn/start and user/message stay outside the folded range.
        let replaces = &summary.payload.as_ref().expect("payload")["replaces"];
        assert_eq!(replaces["from"], json!(1));
        let to = replaces["to"].as_u64().expect("to is a seq");
        let folded = events
            .iter()
            .find(|event| event.seq == to)
            .expect("the folded range ends on a real record");
        assert_eq!(
            folded.typ,
            EventType::TURN_END.as_str(),
            "a plan may only cut at a turn boundary"
        );
        assert!(
            events
                .iter()
                .any(|event| event.seq > to && event.typ == EventType::USER_MESSAGE.as_str()),
            "the open prompt must survive the fold"
        );

        // A second call must not mint a second summary of the same range.
        assert!(
            !runner.compact(session).expect("second compaction runs"),
            "an already-folded range must not be folded twice"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A log can hold hundreds of records that project to no messages at all:
    /// turns that only called tools, or messages written without text. The
    /// summarizer answers those with an empty string and no error, so the fold
    /// has to refuse them itself — otherwise the session loses its history and
    /// gains no account of it.
    #[test]
    fn web_compaction_refuses_an_empty_summary() {
        let dir = std::env::temp_dir().join(format!("hearth-web-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let server = Arc::new(
            hearth_server::Server::new(ServerOptions {
                addr: "127.0.0.1:0".to_string(),
                data_dir: dir.clone(),
                version: "test".to_string(),
                token: "test-token".to_string(),
                ..ServerOptions::default()
            })
            .expect("server"),
        );
        let session = "web_empty_summary";
        server
            .append_event(
                session,
                &Event::new(session, EventType::SESSION_START, Some(json!({}))),
            )
            .expect("session/start");
        for turn in 1..=60u32 {
            for typ in [
                EventType::TURN_START,
                EventType::USER_MESSAGE,
                EventType::ASSISTANT_MESSAGE,
                EventType::TURN_END,
            ] {
                let mut event = Event::new(session, typ, Some(json!({ "turn": turn })));
                event.turn = turn;
                server.append_event(session, &event).expect("seed event");
            }
        }
        let before = std::fs::read(server.path(session)).expect("log before");

        let runner = WebRunner::new(
            Arc::clone(&server),
            Arc::new(FixedProvider(String::new())),
            String::new(),
            "fixed-model".to_string(),
            dir.to_string_lossy().to_string(),
            SandboxMode::ReadOnly,
            WebApproveMode::Never,
            TurnEnv {
                cfg: Arc::new(Config::default()),
                data_dir: dir.clone(),
                system: String::new(),
            },
        );

        assert!(
            !runner.compact(session).expect("compaction answers"),
            "an empty summary is refused rather than appended"
        );
        let after = std::fs::read(server.path(session)).expect("log after");
        assert_eq!(before, after, "a refused fold must not touch the log");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The point of H4: work queued by somebody else is consumed by the turn that
    /// is already running, without the operator sending anything else.
    ///
    /// The queue lives in the log, so "another process" is simulated by appending
    /// the same record that process would write. Nothing about this involves a
    /// timer or a background thread: `run_turn` simply asks after it is done.
    #[test]
    fn a_message_queued_by_another_process_is_drained_after_the_turn() {
        let dir = std::env::temp_dir().join(format!("hearth-web-drain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let server = Arc::new(
            hearth_server::Server::new(ServerOptions {
                addr: "127.0.0.1:0".to_string(),
                data_dir: dir.clone(),
                version: "test".to_string(),
                token: "test-token".to_string(),
                ..ServerOptions::default()
            })
            .expect("server"),
        );
        let session = "web_drain_session";
        server
            .append_event(
                session,
                &Event::new(session, EventType::SESSION_START, Some(json!({}))),
            )
            .expect("session/start");
        // What the HTTP layer writes before it calls the runner.
        for typ in [EventType::TURN_START, EventType::USER_MESSAGE] {
            let mut event = Event::new(session, typ, Some(json!({ "text": "first question" })));
            event.turn = 1;
            server.append_event(session, &event).expect("open turn");
        }
        // Somebody else asks for work to happen after this turn.
        let queued =
            hearth_agent::inbox::Msg::new("work from another process").expect("message id");
        let empty = hearth_agent::inbox::fold(&[]);
        let enqueue = hearth_agent::inbox::enqueue(
            session,
            hearth_agent::inbox::Target::NextTurn,
            &empty,
            &queued,
        );
        server.append_event(session, &enqueue).expect("queued");

        let runner = WebRunner::new(
            Arc::clone(&server),
            Arc::new(FixedProvider("answer".to_string())),
            String::new(),
            "fixed-model".to_string(),
            dir.to_string_lossy().to_string(),
            SandboxMode::ReadOnly,
            WebApproveMode::Never,
            TurnEnv {
                cfg: Arc::new(Config::default()),
                data_dir: dir.clone(),
                system: String::new(),
            },
        );
        runner
            .run_turn(&TurnContext::default(), session, "first question")
            .expect("turn runs");

        let events = parse_log(&std::fs::read_to_string(server.path(session)).expect("log"))
            .expect("log parses");
        let ends = events
            .iter()
            .filter(|event| event.typ == EventType::TURN_END.as_str())
            .count();
        assert_eq!(ends, 2, "the queued message must run as its own turn");
        let asked: Vec<&str> = events
            .iter()
            .filter(|event| event.typ == EventType::USER_MESSAGE.as_str())
            .filter_map(|event| event.payload.as_ref()?.get("text")?.as_str())
            .collect();
        assert!(
            asked.contains(&"work from another process"),
            "the drained prompt must reach the model, saw {asked:?}"
        );
        let left = hearth_agent::inbox::fold(&events);
        assert!(
            left.is_empty(),
            "a drained queue must be empty afterwards, saw {left:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One agent per session, not one per turn.
    ///
    /// The observable part is `hosts`: two turns of the same session must leave
    /// exactly one entry, and both turns must have produced an answer. The point
    /// of caching is not the map — it is that the tool registry, MCP servers and
    /// plugins are dialled once per session instead of once per message.
    #[test]
    fn a_session_keeps_one_agent_across_turns() {
        let dir = std::env::temp_dir().join(format!("hearth-web-resident-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let server = Arc::new(
            hearth_server::Server::new(ServerOptions {
                addr: "127.0.0.1:0".to_string(),
                data_dir: dir.clone(),
                version: "test".to_string(),
                token: "test-token".to_string(),
                ..ServerOptions::default()
            })
            .expect("server"),
        );
        let session = "web_resident_session";
        server
            .append_event(
                session,
                &Event::new(session, EventType::SESSION_START, Some(json!({}))),
            )
            .expect("session/start");

        let runner = WebRunner::new(
            Arc::clone(&server),
            Arc::new(FixedProvider("answer".to_string())),
            String::new(),
            "fixed-model".to_string(),
            dir.to_string_lossy().to_string(),
            SandboxMode::ReadOnly,
            WebApproveMode::Never,
            TurnEnv {
                cfg: Arc::new(Config::default()),
                data_dir: dir.clone(),
                system: String::new(),
            },
        );

        for (index, text) in ["first", "second"].iter().enumerate() {
            let turn = index as u32 + 1;
            for typ in [EventType::TURN_START, EventType::USER_MESSAGE] {
                let mut event = Event::new(session, typ, Some(json!({ "text": text })));
                event.turn = turn;
                server.append_event(session, &event).expect("open turn");
            }
            runner
                .run_turn(&TurnContext::default(), session, text)
                .expect("turn runs");
        }

        assert_eq!(
            runner.host_builds.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "two turns of one session must build one resident agent, not one each"
        );
        let hosts = runner.hosts.lock().expect("hosts lock");
        assert_eq!(hosts.len(), 1, "one session, one entry");
        drop(hosts);

        let events = parse_log(&std::fs::read_to_string(server.path(session)).expect("log"))
            .expect("log parses");
        let ends = events
            .iter()
            .filter(|event| event.typ == EventType::TURN_END.as_str())
            .count();
        assert_eq!(ends, 2, "both turns must close");
        let answers = events
            .iter()
            .filter(|event| event.typ == EventType::ASSISTANT_MESSAGE.as_str())
            .count();
        assert_eq!(answers, 2, "both turns must produce an answer");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The token is reset between turns.
    ///
    /// Without the reset this is the bug that only appears after an operator
    /// presses stop once: every later turn of that session begins cancelled and
    /// answers nothing, and the log says the operator stopped it.
    #[test]
    fn a_cancelled_turn_does_not_cancel_the_next_one() {
        let dir = std::env::temp_dir().join(format!("hearth-web-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let server = Arc::new(
            hearth_server::Server::new(ServerOptions {
                addr: "127.0.0.1:0".to_string(),
                data_dir: dir.clone(),
                version: "test".to_string(),
                token: "test-token".to_string(),
                ..ServerOptions::default()
            })
            .expect("server"),
        );
        let session = "web_reset_session";
        server
            .append_event(
                session,
                &Event::new(session, EventType::SESSION_START, Some(json!({}))),
            )
            .expect("session/start");

        let runner = WebRunner::new(
            Arc::clone(&server),
            Arc::new(FixedProvider("answer".to_string())),
            String::new(),
            "fixed-model".to_string(),
            dir.to_string_lossy().to_string(),
            SandboxMode::ReadOnly,
            WebApproveMode::Never,
            TurnEnv {
                cfg: Arc::new(Config::default()),
                data_dir: dir.clone(),
                system: String::new(),
            },
        );

        for (index, text) in ["first", "second"].iter().enumerate() {
            let turn = index as u32 + 1;
            for typ in [EventType::TURN_START, EventType::USER_MESSAGE] {
                let mut event = Event::new(session, typ, Some(json!({ "text": text })));
                event.turn = turn;
                server.append_event(session, &event).expect("open turn");
            }
            runner
                .run_turn(&TurnContext::default(), session, text)
                .expect("turn runs");
            if index == 0 {
                // The operator stops after the first turn has already finished,
                // which is exactly how a token left set would leak forward.
                runner.cancel().expect("cancel");
            }
        }

        let events = parse_log(&std::fs::read_to_string(server.path(session)).expect("log"))
            .expect("log parses");
        let reasons: Vec<String> = events
            .iter()
            .filter(|event| event.typ == EventType::TURN_END.as_str())
            .filter_map(|event| {
                event
                    .payload
                    .as_ref()?
                    .get("reason")?
                    .as_str()
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            reasons,
            vec!["completed".to_string(), "completed".to_string()],
            "the second turn inherited a cancellation"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
