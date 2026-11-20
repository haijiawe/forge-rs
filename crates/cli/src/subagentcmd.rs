//! `forge subagent`: inspect, resume, and (in worker mode) run a child.
//!
//! The catalog lives in the parent's own log: every `subagent/catalog` row the
//! loop appended when it started a child. `list` only reads that log. `prompt`
//! appends a turn to a continuable child in the place it ran. `worker` is the
//! other end of [`forge_subagent::ProcessBackend`]: it reads one request line on
//! stdin, runs that child here, and writes one response line on stdout. Worker
//! is what makes "out of process" real — a second copy of *our own binary* with
//! its own memory and crash domain, not a call to any external service.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use forge_llm::{Client as LlmClient, Options as LlmOptions, Provider};

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "usage: forge subagent <command> [flags]

Inspect the children a session started, walk the tree below one, resume or stop a
child, or run a child in worker mode. The listings read `subagent/catalog` events
from the session logs; nothing here starts a model except `prompt` and `worker`.

commands:
  list                 the direct children of one session
  tree <root>          every descendant below a root session, one level per node
  resolve <root> <name>  the session id that a lead-relative name points at
  timing <child>       how long a child has been settled, and whether it is active
  prompt <child>       append a turn to a continuable child
  interrupt <child>    request that a running child stop (drops a marker file
                       that a Runner running that child honours)
  providers            the out-of-process backends named in config
  worker               run one child request read from stdin (used by the
                       process backend; not normally typed by hand)

flags:
  --session <id>       the parent session id (list, timing)
  --text <prompt>      the prompt for `prompt`
  --model <name>       override the model for `prompt`
  --as <id|user>       who is asking, for `interrupt` (default: user)
  --data-dir <dir>     data directory (default: the forge home's configured dataDir)
  -h, --help           this message

stdout: the listing, the tree, the resolved id, the timing, or the child's answer.
Diagnostics go to stderr.

exit codes: 0 ok, 1 failure, 2 usage
";

const VALUE_FLAGS: &[&str] = &["data-dir", "session", "text", "model", "as"];

#[derive(Debug, Default)]
struct Options {
    json: bool,
    data_dir: String,
    session: String,
    text: String,
    model: String,
    actor: String,
    positionals: Vec<String>,
}

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let reordered = crate::reorder_flags(args, VALUE_FLAGS);
    let opt = match parse(&reordered, stderr) {
        Ok(opt) => opt,
        Err(code) => return code,
    };
    let Some(command) = opt.positionals.first().cloned() else {
        let _ = stderr.write_all(USAGE.as_bytes());
        return EXIT_USAGE;
    };
    let rest = opt.positionals[1..].to_vec();

    match command.as_str() {
        "list" => list(&opt, stdout, stderr),
        "tree" => tree(&opt, &rest, stdout, stderr),
        "resolve" => resolve(&opt, &rest, stdout, stderr),
        "timing" => timing(&opt, &rest, stdout, stderr),
        "prompt" => prompt(&opt, &rest, stdout, stderr),
        "providers" => providers(&opt, stdout),
        "interrupt" => interrupt(&opt, &rest, stdout, stderr),
        "worker" => worker(stdout),
        other => {
            let _ = writeln!(
                stderr,
                "forge subagent: unknown command {other:?}\n\n{USAGE}"
            );
            EXIT_USAGE
        }
    }
}

fn parse(args: &[String], stderr: &mut dyn Write) -> std::result::Result<Options, i32> {
    let mut opt = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // A bare word is a positional (the command, or a child id), never a flag.
        if !arg.starts_with('-') {
            opt.positionals.push(arg.clone());
            i += 1;
            continue;
        }
        let name = arg.trim_start_matches('-');
        let (name, inline) = match name.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (name.to_string(), None),
        };
        let mut take = |index: &mut usize| -> std::result::Result<String, i32> {
            match inline.clone() {
                Some(value) => Ok(value),
                None => {
                    *index += 1;
                    match args.get(*index) {
                        Some(value) => Ok(value.clone()),
                        None => {
                            let _ = writeln!(stderr, "forge subagent: --{name} needs a value");
                            Err(EXIT_USAGE)
                        }
                    }
                }
            }
        };
        match name.as_str() {
            "json" => opt.json = true,
            "data-dir" => opt.data_dir = take(&mut i)?,
            "session" => opt.session = take(&mut i)?,
            "text" => opt.text = take(&mut i)?,
            "model" => opt.model = take(&mut i)?,
            "as" => opt.actor = take(&mut i)?,
            other => {
                let _ = writeln!(stderr, "forge subagent: unknown flag --{other}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opt)
}

fn resolve_data_dir(opt: &Options) -> std::result::Result<PathBuf, i32> {
    if !opt.data_dir.is_empty() {
        return Ok(PathBuf::from(&opt.data_dir));
    }
    Ok(crate::default_data_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Best-effort config; a missing or broken config is not fatal for these
/// commands because the data dir and model can be given on the command line.
fn load_config() -> Option<forge_config::Config> {
    let path = forge_config::default_path().ok()?;
    forge_config::Config::load(&path).ok()
}

/// The named backends from config, and whether each program can be found. The
/// question this answers is "which of these will actually run", which a config
/// file on its own cannot: a program that is not on PATH looks exactly like one
/// that is until the first delegation fails.
fn providers(_opt: &Options, stdout: &mut dyn Write) -> i32 {
    let Some(cfg) = load_config() else {
        let _ = writeln!(stdout, "no config found; every child runs in this process");
        return EXIT_OK;
    };
    let entries = &cfg.subagent.providers;
    let default = cfg.subagent.backend.trim();
    if entries.is_empty() {
        // A default that names nothing is a broken configuration, and saying
        // "everything runs in this process" without mentioning it would hide the
        // one thing the operator asked about.
        if !default.is_empty() {
            let _ = writeln!(
                stdout,
                "subagent.backend names {default:?} but no providers are configured; every child runs in this process"
            );
            return EXIT_FAILURE;
        }
        let _ = writeln!(
            stdout,
            "no subagent providers configured; every child runs in this process"
        );
        return EXIT_OK;
    }
    let _ = writeln!(stdout, "name\tprogram\tfound\tdefault");
    for entry in entries {
        let found = if which(&entry.program).is_some() {
            "yes"
        } else {
            "no"
        };
        let is_default = if entry.name.trim() == default {
            "yes"
        } else {
            ""
        };
        let _ = writeln!(
            stdout,
            "{}\t{}\t{}\t{}",
            entry.name.trim(),
            entry.program.trim(),
            found,
            is_default
        );
    }
    if !default.is_empty() {
        let _ = writeln!(stdout, "default backend: {default}");
    }
    EXIT_OK
}

/// Resolve a program the way the spawner will: an explicit path is used as
/// given, a bare name is looked up on PATH.
fn which(program: &str) -> Option<PathBuf> {
    let program = program.trim();
    if program.is_empty() {
        return None;
    }
    let path = std::path::Path::new(program);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_string())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let dirs = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&dirs) {
        for ext in &exts {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        // On Windows a program may be written without its extension.
        let bare = dir.join(program);
        if bare.is_file() {
            return Some(bare);
        }
    }
    None
}

fn list(opt: &Options, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if opt.session.is_empty() {
        let _ = writeln!(stderr, "forge subagent: list needs --session <id>");
        return EXIT_USAGE;
    }
    let data_dir = match resolve_data_dir(opt) {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    let path = data_dir
        .join("sessions")
        .join(format!("{}.jsonl", opt.session));
    if !path.is_file() {
        // A missing log is not "no children": it means there is no such session.
        let _ = writeln!(
            stderr,
            "forge subagent: no such session log: {}",
            path.display()
        );
        return EXIT_FAILURE;
    }
    let entries = match forge_subagent::list::children_in_log(&path) {
        Ok(entries) => entries,
        Err(err) => {
            let _ = writeln!(stderr, "forge subagent: {}: {err}", path.display());
            return EXIT_FAILURE;
        }
    };
    if opt.json {
        let rows: Vec<serde_json::Value> = entries.iter().map(|entry| entry.to_payload()).collect();
        let text = serde_json::to_string_pretty(&serde_json::Value::Array(rows))
            .unwrap_or_else(|_| "[]".to_string());
        let _ = writeln!(stdout, "{text}");
    } else {
        let _ = stdout.write_all(forge_subagent::list::render(&entries).as_bytes());
    }
    EXIT_OK
}

fn tree(opt: &Options, rest: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(root) = rest.first() else {
        let _ = writeln!(stderr, "forge subagent: tree needs a root session id");
        return EXIT_USAGE;
    };
    let data_dir = match resolve_data_dir(opt) {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    // A root that never existed is not "a tree with no children" — it is a
    // typo. `list` makes the same distinction, so the two commands agree about
    // what a missing log means.
    let log = data_dir.join("sessions").join(format!("{root}.jsonl"));
    if !log.is_file() {
        let _ = writeln!(
            stderr,
            "forge subagent: no such session log: {}",
            log.display()
        );
        return EXIT_FAILURE;
    }
    let nodes = match forge_subagent::list::descendants(&data_dir, root) {
        Ok(nodes) => nodes,
        Err(err) => {
            let _ = writeln!(stderr, "forge subagent: {root}: {}", err.message());
            return EXIT_FAILURE;
        }
    };
    if opt.json {
        let rows: Vec<serde_json::Value> = nodes.iter().map(node_json).collect();
        let text = serde_json::to_string_pretty(&serde_json::Value::Array(rows))
            .unwrap_or_else(|_| "[]".to_string());
        let _ = writeln!(stdout, "{text}");
        return EXIT_OK;
    }
    let rendered = forge_subagent::list::render_tree(&nodes);
    let _ = stdout.write_all(rendered.as_bytes());
    EXIT_OK
}

fn node_json(node: &forge_subagent::list::Node) -> serde_json::Value {
    let children: Vec<serde_json::Value> = node.children.iter().map(node_json).collect();
    let mut row = node.entry.to_payload();
    if let Some(object) = row.as_object_mut() {
        object.insert("path".to_string(), serde_json::json!(node.path));
        object.insert("depth".to_string(), serde_json::json!(node.depth));
        object.insert("children".to_string(), serde_json::Value::Array(children));
        if let Some(diagnostic) = &node.diagnostic {
            object.insert(
                "diagnostic".to_string(),
                serde_json::json!({
                    "kind": diagnostic.as_str(),
                    "detail": diagnostic.detail(),
                }),
            );
        }
    }
    row
}

fn resolve(opt: &Options, rest: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let (Some(root), Some(name)) = (rest.first(), rest.get(1)) else {
        let _ = writeln!(
            stderr,
            "forge subagent: resolve needs a root session id and a name"
        );
        return EXIT_USAGE;
    };
    let data_dir = match resolve_data_dir(opt) {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    match forge_subagent::list::resolve_name(&data_dir, root, name) {
        Ok(session_id) => {
            if opt.json {
                let row = serde_json::json!({
                    "root": root,
                    "name": name,
                    "sessionId": session_id,
                });
                let text = serde_json::to_string_pretty(&row).unwrap_or_else(|_| "{}".to_string());
                let _ = writeln!(stdout, "{text}");
            } else {
                let _ = writeln!(stdout, "{session_id}");
            }
            EXIT_OK
        }
        Err(err) => {
            let _ = writeln!(stderr, "forge subagent: {name}: {}", err.message());
            EXIT_FAILURE
        }
    }
}

fn timing(opt: &Options, rest: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let data_dir = match resolve_data_dir(opt) {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    if let Some(child) = rest.first() {
        return match forge_subagent::timing::of_child(&data_dir, child) {
            Ok(timing) => {
                if opt.json {
                    let text = serde_json::to_string_pretty(&timing_json(&timing))
                        .unwrap_or_else(|_| "{}".to_string());
                    let _ = writeln!(stdout, "{text}");
                } else {
                    let _ = writeln!(stdout, "{}", timing_line(&timing));
                }
                EXIT_OK
            }
            Err(err) => {
                let _ = writeln!(stderr, "forge subagent: {child}: {}", err.message());
                EXIT_FAILURE
            }
        };
    }
    if !opt.session.is_empty() {
        return match forge_subagent::timing::children(&data_dir, &opt.session) {
            Ok(all) => {
                if opt.json {
                    let rows: Vec<serde_json::Value> = all.iter().map(timing_json).collect();
                    let text = serde_json::to_string_pretty(&serde_json::Value::Array(rows))
                        .unwrap_or_else(|_| "[]".to_string());
                    let _ = writeln!(stdout, "{text}");
                } else if all.is_empty() {
                    let _ = writeln!(stdout, "forge subagent: {} has no children", opt.session);
                } else {
                    for timing in &all {
                        let _ = writeln!(stdout, "{}", timing_line(timing));
                    }
                }
                EXIT_OK
            }
            Err(err) => {
                let _ = writeln!(stderr, "forge subagent: {}: {}", opt.session, err.message());
                EXIT_FAILURE
            }
        };
    }
    let _ = writeln!(
        stderr,
        "forge subagent: timing needs a child id or --session <id>"
    );
    EXIT_USAGE
}

fn timing_json(timing: &forge_subagent::Timing) -> serde_json::Value {
    serde_json::json!({
        "sessionId": timing.child_id,
        "settledMs": timing.settled_ms,
        "active": timing.active,
        "lastTurnCompleted": timing.last_turn_completed,
        "turns": timing.turns,
    })
}

fn timing_line(timing: &forge_subagent::Timing) -> String {
    let state = if timing.active { "active" } else { "idle" };
    let closed = match timing.last_turn_completed {
        Some(ts) => format!("last closed at {ts}"),
        None => "no turn closed yet".to_string(),
    };
    format!(
        "{} {state}, {} turn(s), settled {} ms, {closed}",
        timing.child_id, timing.turns, timing.settled_ms
    )
}

fn build_runner(
    opt: &Options,
    workspace: &str,
    model: &str,
) -> std::result::Result<forge_subagent::Runner, String> {
    let client = LlmClient::new(LlmOptions::default())
        .map_err(|e| format!("no LLM provider: {}", e.message()))?;
    let provider: Arc<dyn Provider> = Arc::new(client);
    let data_dir =
        resolve_data_dir(opt).map_err(|_| "cannot resolve the data directory".to_string())?;
    let mut sub =
        forge_subagent::Options::new(provider, data_dir, workspace.to_string(), model.to_string());
    sub.system = String::new();
    forge_subagent::Runner::new(sub).map_err(|e| e.message().to_string())
}

fn prompt(opt: &Options, rest: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(child) = rest.first() else {
        let _ = writeln!(stderr, "forge subagent: prompt needs a child id");
        return EXIT_USAGE;
    };
    if opt.text.trim().is_empty() {
        let _ = writeln!(stderr, "forge subagent: prompt needs --text <prompt>");
        return EXIT_USAGE;
    }
    let cfg = load_config();
    let model = if !opt.model.is_empty() {
        opt.model.clone()
    } else {
        cfg.as_ref().map(|c| c.model.clone()).unwrap_or_default()
    };
    let workspace = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let runner = match build_runner(opt, &workspace, &model) {
        Ok(runner) => runner,
        Err(message) => {
            let _ = writeln!(stderr, "forge subagent: {message}");
            return EXIT_FAILURE;
        }
    };
    match runner.resume(child, &opt.text, "", "") {
        Ok(run) => {
            let _ = writeln!(stdout, "{}", run.text.trim_end());
            EXIT_OK
        }
        Err(err) => {
            let _ = writeln!(stderr, "forge subagent: {}", err.message());
            EXIT_FAILURE
        }
    }
}

fn interrupt(
    opt: &Options,
    rest: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let Some(child) = rest.first() else {
        let _ = writeln!(stderr, "forge subagent: interrupt needs a child id");
        return EXIT_USAGE;
    };
    let data_dir = match resolve_data_dir(opt) {
        Ok(dir) => dir,
        Err(code) => return code,
    };
    // Who is asking decides whether the request is allowed at all: a child may
    // not stop its own ancestor, and a sibling may not stop a sibling. The
    // marker file is just transport; this check is the policy. Default is the
    // operator, because a person typing this command is one.
    let caller = if opt.actor.is_empty() || opt.actor == "user" {
        forge_subagent::authority::Caller::User
    } else {
        forge_subagent::authority::Caller::Session(opt.actor.clone())
    };
    if let forge_subagent::authority::Verdict::Deny(reason) =
        forge_subagent::authority::check(&data_dir, &caller, child)
    {
        let _ = writeln!(stderr, "forge subagent: {reason}");
        return EXIT_FAILURE;
    }
    match forge_subagent::cancel_file::request(&data_dir, child) {
        Ok(path) => {
            let _ = writeln!(
                stdout,
                "forge subagent: interrupt requested for {child} (marker: {})",
                path.display()
            );
            EXIT_OK
        }
        Err(err) => {
            let _ = writeln!(stderr, "forge subagent: {}", err.message());
            EXIT_FAILURE
        }
    }
}

/// Read one request line from stdin, run that child in this process, write one
/// response line to stdout. A failure still writes a response line (with
/// `ok:false`) so the caller gets a clean verdict instead of a bare EOF.
fn worker(stdout: &mut dyn Write) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        let line =
            forge_subagent::BackendOutcome::failure("io", "cannot read the request from stdin");
        let _ = writeln!(
            stdout,
            "{}",
            serde_json::to_string(&line).unwrap_or_default()
        );
        return EXIT_FAILURE;
    }
    let Some(first) = input.lines().next() else {
        let line = forge_subagent::BackendOutcome::failure("usage", "empty request");
        let _ = writeln!(
            stdout,
            "{}",
            serde_json::to_string(&line).unwrap_or_default()
        );
        return EXIT_USAGE;
    };
    let request: forge_subagent::BackendRequest = match serde_json::from_str(first) {
        Ok(request) => request,
        Err(err) => {
            let line = forge_subagent::BackendOutcome::failure("bad_request", format!("{err}"));
            let _ = writeln!(
                stdout,
                "{}",
                serde_json::to_string(&line).unwrap_or_default()
            );
            return EXIT_USAGE;
        }
    };

    let outcome = run_worker(&request);
    let _ = writeln!(
        stdout,
        "{}",
        serde_json::to_string(&outcome).unwrap_or_default()
    );
    EXIT_OK
}

fn run_worker(request: &forge_subagent::BackendRequest) -> forge_subagent::BackendOutcome {
    let cfg = load_config();
    let model = request
        .model
        .clone()
        .or_else(|| cfg.as_ref().map(|c| c.model.clone()))
        .unwrap_or_default();
    let workspace = request
        .workspace
        .clone()
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let opt = Options::default();
    let runner = match build_runner(&opt, &workspace, &model) {
        Ok(runner) => runner,
        Err(message) => return forge_subagent::BackendOutcome::failure("provider", message),
    };

    if request.op == "resume" {
        let Some(child) = &request.child else {
            return forge_subagent::BackendOutcome::failure(
                "bad_request",
                "resume needs a child id",
            );
        };
        return match runner.resume(
            child,
            &request.prompt,
            request.system.as_deref().unwrap_or(""),
            "",
        ) {
            Ok(run) => {
                let mut out = forge_subagent::BackendOutcome::success(
                    &run.id, &run.text, run.turns, run.steps,
                );
                if let Some(value) = run.structured {
                    out = out.with_structured(value);
                }
                out
            }
            Err(err) => forge_subagent::BackendOutcome::failure("resume_failed", err.message()),
        };
    }

    // op == "run": a fresh child. A worker only ever starts one-shot children;
    // a continuable child that a later *process* must resume is the local path,
    // because the worker's process is gone the moment this line is written.
    let mode = request
        .mode
        .as_deref()
        .and_then(forge_subagent::Mode::parse)
        .unwrap_or(forge_subagent::Mode::OneShot);
    let ctx = forge_subagent::CallContext {
        cancel: forge_agent::Cancel::default(),
        parent_session_id: String::new(),
        workspace: workspace.clone(),
    };
    match runner.run_child(
        &ctx,
        &request.prompt,
        request.system.as_deref().unwrap_or(""),
        "",
        request.schema.clone(),
        forge_subagent::Mode::OneShot,
        None,
    ) {
        Ok(run) => {
            let _ = mode;
            let mut out =
                forge_subagent::BackendOutcome::success(&run.id, &run.text, run.turns, run.steps);
            if let Some(value) = run.structured {
                out = out.with_structured(value);
            }
            out
        }
        Err(err) => forge_subagent::BackendOutcome::failure("run_failed", err.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn a_session_id_is_a_flag_value_not_the_command() {
        let mut err = Vec::new();
        let opt = parse(&args(&["--session", "s1", "list"]), &mut err).expect("parses");
        assert_eq!(opt.positionals, vec!["list".to_string()]);
        assert_eq!(opt.session, "s1");
        assert!(!opt.json);
    }

    #[test]
    fn prompt_reads_a_child_and_a_text() {
        let mut err = Vec::new();
        let opt = parse(
            &args(&["prompt", "abc-123", "--text", "hi there"]),
            &mut err,
        )
        .expect("parses");
        assert_eq!(
            opt.positionals,
            vec!["prompt".to_string(), "abc-123".to_string()]
        );
        assert_eq!(opt.text, "hi there");
    }

    #[test]
    fn the_usage_names_every_command() {
        for cmd in [
            "list",
            "tree",
            "resolve",
            "timing",
            "prompt",
            "interrupt",
            "providers",
            "worker",
        ] {
            assert!(USAGE.contains(cmd), "usage must mention {cmd}");
        }
    }

    #[test]
    fn interrupt_needs_a_child_id() {
        let opt = Options::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = interrupt(&opt, &[], &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8(err).unwrap().contains("child id"));
    }

    #[test]
    fn interrupt_writes_a_marker_next_to_the_child_log() {
        let root = std::env::temp_dir().join(format!("forge-cli-int-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let opt = Options {
            data_dir: root.to_string_lossy().into_owned(),
            ..Options::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = interrupt(
            &opt,
            &["20260101-000000-deadbeef".to_string()],
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK, "stderr: {}", String::from_utf8_lossy(&err));
        assert!(root
            .join("sessions")
            .join("20260101-000000-deadbeef.cancel")
            .is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unknown_flag_is_a_usage_error() {
        let mut err = Vec::new();
        let code = parse(&args(&["--bogus"]), &mut err).unwrap_err();
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("unknown flag --bogus"));
    }

    #[test]
    fn a_missing_session_is_a_usage_error_not_an_empty_table() {
        let opt = Options::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = list(&opt, &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8(err).unwrap().contains("--session"));
        assert!(out.is_empty());
    }

    #[test]
    fn prompt_without_text_is_a_usage_error() {
        let opt = Options::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = prompt(&opt, &["abc".to_string()], &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8(err).unwrap().contains("--text"));
    }

    /// A data directory holding one session log with only its `session/start`
    /// row: a session that exists and started no children.
    fn seeded_root(label: &str) -> (PathBuf, String) {
        let root = std::env::temp_dir().join(format!("forge-cli-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        let session = "root-session-0001";
        let path = sessions.join(format!("{session}.jsonl"));
        let store = forge_session::Store::open(&path, forge_session::Options::default())
            .expect("store opens");
        store
            .append(&forge_core::Event::new(
                session,
                forge_core::EventType::SESSION_START,
                Some(serde_json::json!({
                    "workspace": ".",
                    "model": "m",
                    "startedBy": "cli",
                })),
            ))
            .expect("head appended");
        (root, session.to_string())
    }

    #[test]
    fn tree_of_a_root_with_no_children_prints_the_empty_tree_line() {
        let (root, session) = seeded_root("tree-empty");
        let opt = Options {
            data_dir: root.to_string_lossy().into_owned(),
            ..Options::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = tree(&opt, &[session], &mut out, &mut err);
        assert_eq!(code, EXIT_OK, "stderr: {}", String::from_utf8_lossy(&err));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("no subagents"), "stdout: {text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tree_needs_a_root_session() {
        let opt = Options::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = tree(&opt, &[], &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8(err).unwrap().contains("root session"));
    }

    #[test]
    fn tree_of_a_session_that_never_existed_names_it_in_the_failure() {
        let root = std::env::temp_dir().join(format!("forge-cli-tree-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let opt = Options {
            data_dir: root.to_string_lossy().into_owned(),
            ..Options::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = tree(
            &opt,
            &["ghost-session-0001".to_string()],
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_FAILURE);
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("ghost-session-0001"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_needs_a_root_and_a_name() {
        let opt = Options::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = resolve(&opt, &["root-0001".to_string()], &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8(err).unwrap().contains("name"));
    }

    #[test]
    fn timing_needs_a_child_or_a_session() {
        let opt = Options::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = timing(&opt, &[], &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8(err).unwrap().contains("--session"));
    }

    #[test]
    fn an_actor_is_a_flag_value_not_a_command() {
        let mut err = Vec::new();
        let opt = parse(
            &args(&["interrupt", "20260101-000000-deadbeef", "--as", "s1"]),
            &mut err,
        )
        .expect("parses");
        assert_eq!(
            opt.positionals,
            vec![
                "interrupt".to_string(),
                "20260101-000000-deadbeef".to_string()
            ]
        );
        assert_eq!(opt.actor, "s1");
    }

    #[test]
    fn a_caller_that_cannot_be_identified_is_refused_before_the_marker_is_written() {
        let root = std::env::temp_dir().join(format!("forge-cli-int-deny-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let opt = Options {
            data_dir: root.to_string_lossy().into_owned(),
            actor: "ghost-session-0001".to_string(),
            ..Options::default()
        };
        let child = "20260101-000000-deadbeef".to_string();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = interrupt(&opt, std::slice::from_ref(&child), &mut out, &mut err);
        assert_eq!(code, EXIT_FAILURE);
        assert!(!String::from_utf8(err).unwrap().is_empty());
        assert!(
            !root
                .join("sessions")
                .join(format!("{child}.cancel"))
                .is_file(),
            "a denied request must not leave transport behind"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
