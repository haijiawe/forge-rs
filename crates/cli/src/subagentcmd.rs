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

Inspect the direct children a session started, resume one, or run a child in
worker mode. The listing reads `subagent/catalog` events from that session's log.

commands:
  list                 the direct children of one session
  prompt <child>       append a turn to a continuable child
  interrupt <child>    request that a running child stop (drops a marker file
                       that a Runner running that child honours)
  worker               run one child request read from stdin (used by the
                       process backend; not normally typed by hand)

flags:
  --session <id>       the parent session id whose children to list
  --text <prompt>      the prompt for `prompt`
  --model <name>       override the model for `prompt`
  --data-dir <dir>     data directory (default: the forge home's configured dataDir)
  -h, --help           this message

stdout: the listing or the child's answer. Diagnostics go to stderr.

exit codes: 0 ok, 1 failure, 2 usage
";

const VALUE_FLAGS: &[&str] = &["data-dir", "session", "text", "model"];

#[derive(Debug, Default)]
struct Options {
    json: bool,
    data_dir: String,
    session: String,
    text: String,
    model: String,
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
        "prompt" => prompt(&opt, &rest, stdout, stderr),
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
        for cmd in ["list", "prompt", "interrupt", "worker"] {
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
}
