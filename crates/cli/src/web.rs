//! `forge web` — serve the browser UI on loopback.
//!
//! Layer L3. Ported from `forge-v2/cmd/forge/web.go`. The server owns every log
//! append; this runner owns only the model loop and answering approvals by
//! polling the log through a DecisionWatcher. Prompt events are written by the
//! HTTP layer, so the agent adopts the open turn (`prompt_recorded = true`).

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use forge_agent::{Agent, EventLog, Options as AgentOptions};
use forge_config::Config;
use forge_core::{event::parse_log, Event, Result};
use forge_llm::{Client as LlmClient, Options as LlmOptions, Provider};
use forge_protocol::decision;
use forge_protocol::{CreateSessionRequest, CreateSessionResponse};
use forge_sandbox::{mode_from_str, Mode as SandboxMode, Spec as SandboxSpec};
use forge_server::{CancelToken, Options as ServerOptions, TurnContext, TurnRunner};
use forge_session::DecisionWatcher;
use forge_tool::builtin;
use forge_tool::executor::{Executor, ExecutorOptions};
use forge_tool::policy::default_rules;
use forge_tool::registry::Registry;
use forge_tool::truncate::DefaultLimits;
use forge_tool::{ApprovalRequest, Approver, Policy};

use crate::chat::{AlwaysApprover, RecordingApprover, ToolRunnerAdapter};
use crate::reorder_flags;
use crate::VERSION;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

const WEB_USAGE: &str = "usage: forge web [flags]\n\n\
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
                    let _ = writeln!(stderr, "forge web: flag needs an argument: -{name}");
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
                let _ = writeln!(stderr, "forge web: unknown flag -{name}");
                let _ = write!(stderr, "{WEB_USAGE}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opts)
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "forge web: {message}");
    let _ = write!(stderr, "{WEB_USAGE}");
    EXIT_USAGE
}

fn runtime_error(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "forge web: {message}");
    EXIT_FAILURE
}

fn web_approve_mode(raw: &str, stderr: &mut dyn Write) -> std::result::Result<WebApproveMode, i32> {
    match raw {
        "" | "web" => Ok(WebApproveMode::Web),
        "never" => Ok(WebApproveMode::Never),
        "always" => Ok(WebApproveMode::Always),
        "prompt" => Err(usage_error(
            stderr,
            "--approve=prompt reads stdin, which `forge web` does not ask on: use --approve=web to answer in the browser",
        )),
        other => Err(usage_error(
            stderr,
            &format!("unknown --approve value {other:?}: use web, never or always"),
        )),
    }
}

fn load_config(stderr: &mut dyn Write) -> std::result::Result<Config, i32> {
    let path =
        forge_config::default_path().map_err(|e| runtime_error(stderr, &format!("config: {e}")))?;
    let cfg = Config::load(&path).map_err(|e| runtime_error(stderr, &format!("config: {e}")))?;
    if let Err(err) = cfg.validate() {
        if !err.message().contains("mcp") {
            return Err(runtime_error(stderr, &format!("config: {err}")));
        }
        let _ = writeln!(
            stderr,
            "forge web: {err} (continuing without those servers)"
        );
    }
    Ok(cfg)
}

/// An `EventLog` that routes every append through the server, so the HTTP and
/// WebSocket layers see the same lines the agent writes. `read_all` is a plain
/// file read: the log is append-only and the server is the only writer.
struct ServerLog {
    server: Arc<forge_server::Server>,
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
            Err(e) => Err(forge_core::Error::new(
                forge_core::ErrorKind::Io,
                format!("web: read log: {e}"),
            )),
        }
    }
}

/// Answers one approval by polling the session log for the decision the HTTP
/// handler appended for a browser click (`decidedBy: user`).
struct WebApprover {
    path: PathBuf,
    cancel: forge_agent::Cancel,
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

/// One turn runner per session, supplied to the server by the factory closure.
struct WebRunner {
    server: Arc<forge_server::Server>,
    provider: Arc<dyn Provider>,
    model: String,
    workspace: String,
    sandbox: SandboxMode,
    approve: WebApproveMode,
    /// The agent's cancellation flag; `cancel()` flips it without appending.
    agent_cancel: Arc<Mutex<forge_agent::Cancel>>,
    /// Bridges the server's per-turn token to the agent's flag.
    turn_cancel: Arc<Mutex<Option<CancelToken>>>,
}

impl WebRunner {
    fn new(
        server: Arc<forge_server::Server>,
        provider: Arc<dyn Provider>,
        model: String,
        workspace: String,
        sandbox: SandboxMode,
        approve: WebApproveMode,
    ) -> Self {
        Self {
            server,
            provider,
            model,
            workspace,
            sandbox,
            approve,
            agent_cancel: Arc::new(Mutex::new(forge_agent::Cancel::default())),
            turn_cancel: Arc::new(Mutex::new(None)),
        }
    }
}

impl TurnRunner for WebRunner {
    fn run_turn(&self, ctx: &TurnContext, session_id: &str, text: &str) -> Result<()> {
        // Bridge the server's cancellation token into the agent's flag. The
        // server calls `cancel()` before flipping ctx.cancel, so this is mostly
        // a belt-and-braces path for in-process callers.
        *self.turn_cancel.lock().expect("web: turn_cancel poisoned") = Some(ctx.cancel.clone());
        let cancel = forge_agent::Cancel::default();
        *self
            .agent_cancel
            .lock()
            .expect("web: agent_cancel poisoned") = cancel.clone();

        let log: Arc<dyn EventLog> = Arc::new(ServerLog {
            server: Arc::clone(&self.server),
            session: session_id.to_string(),
        });
        let log_path = self.server.path(session_id);

        let registry = Registry::new(builtin::all(DefaultLimits));
        let policy: Arc<dyn Policy> = Arc::new(default_rules(self.sandbox.as_str()));
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
                    path: log_path,
                    cancel: cancel.clone(),
                }),
                "web",
                true,
            ))),
        };
        let executor = Executor::new(
            Arc::new(registry),
            ExecutorOptions {
                policy: Some(policy),
                approver,
                workspace: self.workspace.clone(),
                ..ExecutorOptions::default()
            },
        );
        let tools: Option<Arc<dyn forge_agent::ToolRunner>> =
            Some(Arc::new(ToolRunnerAdapter { executor }));

        let delta_ctx = ctx.clone();
        let on_delta: forge_agent::DeltaHook = Arc::new(move |turn, step, text| {
            delta_ctx.delta(turn, step, text);
        });
        let steer_ctx = ctx.clone();
        let steering: forge_agent::SteeringHook = Arc::new(move || steer_ctx.take_steering());

        let mut opts = AgentOptions::new(
            Arc::clone(&self.provider),
            Arc::clone(&log),
            self.model.clone(),
            session_id.to_string(),
            self.workspace.clone(),
        );
        opts.prompt_recorded = true; // the HTTP layer wrote turn/start + user/message
        opts.tools = tools;
        opts.on_delta = Some(on_delta);
        opts.steering = Some(steering);
        opts.cancel = cancel;

        match Agent::new(opts) {
            Ok(agent) => {
                // The agent writes turn/end itself, including on cancellation.
                let _ = agent.run(text);
                Ok(())
            }
            Err(e) => Err(forge_core::Error::new(
                forge_core::ErrorKind::Invalid,
                format!("web: agent: {}", e.message()),
            )),
        }
    }

    fn cancel(&self) -> Result<()> {
        if let Ok(guard) = self.agent_cancel.lock() {
            guard.cancel();
        }
        Ok(())
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

/// Run `forge web`.
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

    let provider: Arc<dyn Provider> = match LlmClient::new(LlmOptions::default()) {
        Ok(client) => Arc::new(client),
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
    let _enforcer = match forge_sandbox::new(enforcer_spec, None) {
        Ok(enforcer) => enforcer,
        Err(e) => return runtime_error(stderr, &format!("sandbox: {}", e.message())),
    };

    let mut server_options = ServerOptions {
        addr: opts.addr.trim().to_string(),
        data_dir: PathBuf::from(cfg.data_dir.trim()),
        version: VERSION.to_string(),
        ..Default::default()
    };

    // The runner factory needs the server, which does not exist until after
    // Server::new; share a slot and fill it immediately after construction.
    let server_slot: Arc<Mutex<Option<Arc<forge_server::Server>>>> = Arc::new(Mutex::new(None));
    {
        let slot = Arc::clone(&server_slot);
        let provider = Arc::clone(&provider);
        let model = model.clone();
        let workspace_str = workspace_str.clone();
        server_options.new_runner = Some(Arc::new(move |_session_id: &str| {
            let server = slot
                .lock()
                .expect("web: server slot poisoned")
                .as_ref()
                .cloned()
                .expect("web: runner created before server was ready");
            Arc::new(WebRunner::new(
                server,
                Arc::clone(&provider),
                model.clone(),
                workspace_str.clone(),
                sandbox,
                approve,
            )) as Arc<dyn TurnRunner>
        }));
    }

    let server = match forge_server::Server::new(server_options) {
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

    let _ = writeln!(stderr, "forge web: serving {base_url}");
    let _ = writeln!(
        stderr,
        "forge web: session={session_id} model={model} workspace={workspace_str} sandbox={} approve={}",
        sandbox.as_str(),
        approve.as_str()
    );
    let _ = writeln!(stderr, "forge web: open {open_url}");

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
}
