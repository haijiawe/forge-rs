//! `forge plugin`: list, start, stop and call plugins in `<dataDir>/plugins`.
//!
//! One `Host` per invocation, created and dropped inside this command: a
//! subcommand is a process, so the lifetime is the process. This is deliberately
//! not the object `chat` holds for a turn — `chat` builds its own host and is the
//! only place that keeps children alive between turns.
//!
//! Nothing here spawns a plugin except `start` and `call`. `list` and `doctor`
//! read manifests and report; a listing that started every installed plugin to
//! describe it would be a listing that runs code the operator only wanted to
//! inspect.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_config::Config;

use crate::one_line;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "usage: forge plugin <command> [flags]

Inspect and drive the plugins installed under <dataDir>/plugins. A plugin is a
directory with a plugin.json holding at least name, version, runtime and entry;
this harness speaks one protocol to it over stdin/stdout (see
crates/plugin/PROTOCOL.md). Nothing is started until `start` or `call` is passed:
listing a directory must never run the code in it.

commands:
  list                 show every installed plugin, its runtime and state
  start <name>         start the plugin and print its handshake
  stop <name>          send shutdown to a plugin started in this process
  call <name> <tool>   call one of the plugin's tools with JSON arguments
  doctor               show the plugin root and which runtimes exist

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory to read from (default: the forge home's
                       configured dataDir)
  --dir <dir>          plugin root (default: <dataDir>/plugins)
  --args <json>        JSON arguments for `call` (default: {})
  --timeout <ms>       per-request budget in milliseconds (default: 10000)
  -h, --help           this message

stdout: the listing, the handshake, the tool output, or the doctor report.
Diagnostics go to stderr.

exit codes: 0 ok, 1 failure, 2 usage
";

const VALUE_FLAGS: &[&str] = &["data-dir", "dir", "args", "timeout"];

#[derive(Debug, Default)]
struct Options {
    json: bool,
    data_dir: String,
    dir: String,
    args: String,
    timeout: u64,
}

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let reordered = crate::reorder_flags(args, VALUE_FLAGS);
    let Some(command) = crate::command_after_flags(&reordered, VALUE_FLAGS) else {
        let _ = stderr.write_all(USAGE.as_bytes());
        return EXIT_USAGE;
    };
    let opt = match parse(&reordered, stderr) {
        Ok(opt) => opt,
        Err(code) => return code,
    };

    match command.as_str() {
        "list" => list(&opt, stdout, stderr),
        "start" => start(&opt, &reordered, stdout, stderr),
        "stop" => stop(&opt, &reordered, stdout, stderr),
        "call" => call(&opt, &reordered, stdout, stderr),
        "doctor" => doctor(&opt, stdout, stderr),
        other => {
            let _ = writeln!(stderr, "forge plugin: unknown command {other:?}\n\n{USAGE}");
            EXIT_USAGE
        }
    }
}

fn parse(args: &[String], stderr: &mut dyn Write) -> std::result::Result<Options, i32> {
    let mut opt = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // Positionals are not this function's business: `parse` only reads flags,
        // and a bare word here is either the command or a plugin/tool name that the
        // dispatch arm reads again. Treating them as flags made `forge plugin start
        // reference` fail with "unknown flag --reference", which is exactly the
        // shape of command a user types first.
        if !arg.starts_with('-') {
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
                            let _ = writeln!(stderr, "forge plugin: --{name} needs a value");
                            Err(EXIT_USAGE)
                        }
                    }
                }
            }
        };
        match name.as_str() {
            "json" if inline.is_none() => opt.json = true,
            "data-dir" => opt.data_dir = take(&mut i)?,
            "dir" => opt.dir = take(&mut i)?,
            "args" => opt.args = take(&mut i)?,
            "timeout" => {
                let raw = take(&mut i)?;
                match raw.parse::<u64>() {
                    Ok(value) if value > 0 => opt.timeout = value,
                    _ => {
                        let _ = writeln!(
                            stderr,
                            "forge plugin: --timeout wants a positive number of milliseconds, got {raw:?}"
                        );
                        return Err(EXIT_USAGE);
                    }
                }
            }
            "" => {}
            other => {
                let _ = writeln!(stderr, "forge plugin: unknown flag --{other}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opt)
}

/// The plugin root, plus the data directory it was derived from.
fn root_of(opt: &Options) -> forge_core::Result<PathBuf> {
    if !opt.dir.trim().is_empty() {
        return Ok(PathBuf::from(opt.dir.trim()));
    }
    let data_dir = if opt.data_dir.trim().is_empty() {
        crate::default_data_dir()?
    } else {
        PathBuf::from(opt.data_dir.trim())
    };
    Ok(data_dir.join("plugins"))
}

fn host_of(opt: &Options) -> forge_core::Result<Arc<forge_plugin::Host>> {
    let root = root_of(opt)?;
    let mut host = forge_plugin::Host::start(&root);
    if opt.timeout > 0 {
        host.set_timeout(std::time::Duration::from_millis(opt.timeout));
    }
    Ok(Arc::new(host))
}

/// Positional arguments of the subcommand, skipping flag values so that
/// `forge plugin start --dir X foo` yields `foo` and not `X`.
fn positional<'a>(args: &'a [String], command: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut seen_command = false;
    let mut stop_flags = false;
    while i < args.len() {
        let arg = args[i].as_str();
        if stop_flags {
            out.push(arg);
            i += 1;
            continue;
        }
        if arg == command && !seen_command {
            seen_command = true;
            i += 1;
            continue;
        }
        if arg == "--" {
            stop_flags = true;
            i += 1;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            if !name.contains('=') && VALUE_FLAGS.contains(&name) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        out.push(arg);
        i += 1;
    }
    out
}

fn list(opt: &Options, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let host = match host_of(opt) {
        Ok(host) => host,
        Err(err) => return fail(stderr, &format!("forge plugin: {}", err.message())),
    };
    let root = host.root().to_path_buf();
    let (found, broken) = host.discover();
    if opt.json {
        let plugins: Vec<serde_json::Value> = found
            .iter()
            .map(|manifest| {
                serde_json::json!({
                    "name": manifest.name,
                    "version": manifest.version,
                    "description": manifest.description,
                    "runtime": manifest.runtime.as_str(),
                    "entry": manifest.entry,
                    "directory": manifest.directory.to_string_lossy(),
                })
            })
            .collect();
        let broken_entries: Vec<serde_json::Value> = broken
            .iter()
            .map(|(path, error)| {
                serde_json::json!({
                    "directory": path.to_string_lossy(),
                    "code": error.code(),
                    "error": error.message(),
                })
            })
            .collect();
        return crate::write_json(
            stdout,
            stderr,
            "plugin list",
            &serde_json::json!({
                "root": root.to_string_lossy(),
                "plugins": plugins,
                "broken": broken_entries,
            }),
        );
    }
    if found.is_empty() && broken.is_empty() {
        let _ = writeln!(stdout, "no plugins installed under {}", root.display());
        return EXIT_OK;
    }
    if !found.is_empty() {
        let _ = writeln!(
            stdout,
            "{:<20} {:<10} {:<8} description",
            "name", "version", "runtime"
        );
        for manifest in &found {
            let _ = writeln!(
                stdout,
                "{:<20} {:<10} {:<8} {}",
                manifest.name,
                manifest.version,
                manifest.runtime.as_str(),
                one_line(&manifest.description)
            );
        }
    }
    for (path, error) in &broken {
        // Never silently skipped: a plugin that stopped parsing is the reason
        // its tools are missing, and that has to be visible.
        let _ = writeln!(
            stderr,
            "forge plugin: {}: {} ({})",
            path.display(),
            error.message(),
            error.code()
        );
    }
    if found.is_empty() {
        let _ = writeln!(
            stderr,
            "forge plugin: no readable plugin under {}",
            root.display()
        );
        return EXIT_FAILURE;
    }
    EXIT_OK
}

fn start(opt: &Options, args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(name) = positional(args, "start")
        .first()
        .map(|name| (**name).to_string())
    else {
        return usage_error(stderr, "start needs a plugin name");
    };
    let host = match host_of(opt) {
        Ok(host) => host,
        Err(err) => return fail(stderr, &format!("forge plugin: {}", err.message())),
    };
    match host.load(&name) {
        Ok(loaded) => {
            let tools: Vec<String> = host
                .tools(&loaded.manifest.name)
                .unwrap_or_default()
                .into_iter()
                .map(|spec| spec.name)
                .collect();
            if opt.json {
                return crate::write_json(
                    stdout,
                    stderr,
                    "plugin start",
                    &serde_json::json!({
                        "name": loaded.manifest.name,
                        "version": loaded.manifest.version,
                        "protocolVersion": loaded.handshake.protocol_version,
                        "capabilities": {
                            "tools": loaded.handshake.capabilities.tools,
                            "prompts": loaded.handshake.capabilities.prompts,
                            "hooks": loaded.handshake.capabilities.hooks,
                        },
                        "tools": tools,
                    }),
                );
            }
            let _ = writeln!(
                stdout,
                "{} {} speaks protocol {}",
                loaded.manifest.name, loaded.manifest.version, loaded.handshake.protocol_version
            );
            let _ = writeln!(
                stdout,
                "  tools:{}  prompts:{}  hooks:{}",
                if loaded.handshake.capabilities.tools {
                    String::new()
                } else {
                    " (none)".to_string()
                },
                if loaded.handshake.capabilities.prompts {
                    ""
                } else {
                    " (none)"
                },
                if loaded.handshake.capabilities.hooks.is_empty() {
                    " (none)".to_string()
                } else {
                    format!(" {}", loaded.handshake.capabilities.hooks.join(", "))
                }
            );
            for tool in tools {
                let _ = writeln!(stdout, "  - {tool}");
            }
            EXIT_OK
        }
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge plugin: start {name:?}: {} ({})",
                err.message(),
                err.code()
            );
            EXIT_FAILURE
        }
    }
}

fn stop(opt: &Options, args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(name) = positional(args, "stop")
        .first()
        .map(|name| (**name).to_string())
    else {
        return usage_error(stderr, "stop needs a plugin name");
    };
    let host = match host_of(opt) {
        Ok(host) => host,
        Err(err) => return fail(stderr, &format!("forge plugin: {}", err.message())),
    };
    // `stop` runs in a fresh process, so in practice nothing is loaded: it says
    // that plainly rather than pretending the plugin had been running.
    match host.unload(&name) {
        Ok(Some(stopped)) => {
            let _ = writeln!(
                stdout,
                "{} stopped (clean: {}, exited: {}){}",
                name,
                stopped.clean,
                stopped.exited,
                if stopped.detail.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", one_line(&stopped.detail))
                }
            );
            EXIT_OK
        }
        Ok(None) => {
            let _ = writeln!(stdout, "{name} is not running in this process");
            EXIT_OK
        }
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge plugin: stop {name:?}: {} ({})",
                err.message(),
                err.code()
            );
            EXIT_FAILURE
        }
    }
}

fn call(opt: &Options, args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let positionals = positional(args, "call");
    let (Some(plugin), Some(tool)) = (
        positionals.first().map(|name| (**name).to_string()),
        positionals.get(1).map(|name| (**name).to_string()),
    ) else {
        return usage_error(stderr, "call needs a plugin name and a tool name");
    };
    let arguments: serde_json::Value = if opt.args.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(opt.args.trim()) {
            Ok(value) => value,
            Err(err) => {
                let _ = writeln!(stderr, "forge plugin: --args is not JSON: {err}");
                return EXIT_USAGE;
            }
        }
    };
    let host = match host_of(opt) {
        Ok(host) => host,
        Err(err) => return fail(stderr, &format!("forge plugin: {}", err.message())),
    };
    // Every CLI invocation is a fresh process, so nothing is loaded yet: the call
    // has to load its own plugin. Without this, `call` always failed with
    // "not_initialized" — a command whose only possible outcome is an error is not
    // a command, it is a trap.
    if let Err(err) = host.load(&plugin) {
        let _ = writeln!(
            stderr,
            "forge plugin: load {plugin:?}: {} ({})",
            err.message(),
            err.code()
        );
        return EXIT_FAILURE;
    }
    let outcome = match host.call_tool(&plugin, &tool, arguments) {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "forge plugin: call {plugin}.{tool}: {} ({})",
                err.message(),
                err.code()
            );
            return EXIT_FAILURE;
        }
    };
    if opt.json {
        return crate::write_json(
            stdout,
            stderr,
            "plugin call",
            &serde_json::json!({
                "plugin": plugin,
                "tool": tool,
                "output": outcome.output,
                "isError": outcome.is_error,
            }),
        );
    }
    let _ = writeln!(stdout, "{}", outcome.output);
    if outcome.is_error {
        // The host answered; the tool said no. stdout still carries the tool's
        // own text, because that text is usually the only explanation.
        let _ = writeln!(stderr, "forge plugin: the tool itself reported a failure");
        return EXIT_FAILURE;
    }
    EXIT_OK
}

fn doctor(opt: &Options, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let host = match host_of(opt) {
        Ok(host) => host,
        Err(err) => return fail(stderr, &format!("forge plugin: {}", err.message())),
    };
    let root = host.root().to_path_buf();
    let (found, broken) = host.discover();
    let runtimes = [
        ("node", interpreter_available("node")),
        ("python", interpreter_available("python")),
    ];
    if opt.json {
        return crate::write_json(
            stdout,
            stderr,
            "plugin doctor",
            &serde_json::json!({
                "root": root.to_string_lossy(),
                "exists": root.is_dir(),
                "plugins": found.len(),
                "broken": broken.len(),
                "runtimes": runtimes
                    .iter()
                    .map(|(name, available)| serde_json::json!({"runtime": name, "available": available}))
                    .collect::<Vec<_>>(),
            }),
        );
    }
    let _ = writeln!(stdout, "root:      {}", root.display());
    let _ = writeln!(
        stdout,
        "exists:    {}",
        if root.is_dir() {
            "yes"
        } else {
            "no (nothing installed yet)"
        }
    );
    let _ = writeln!(stdout, "plugins:   {}", found.len());
    let _ = writeln!(stdout, "broken:    {}", broken.len());
    for (name, available) in runtimes {
        let _ = writeln!(
            stdout,
            "runtime:   {name:<8} {}",
            if available {
                "found"
            } else {
                "missing (plugins using it cannot start)"
            }
        );
    }
    let _ = writeln!(
        stdout,
        "note:      this command starts nothing; `forge plugin start <name>` does."
    );
    EXIT_OK
}

/// Whether `program --version` runs at all.
///
/// The probe is a spawn, not a lookup on PATH: an interpreter found on PATH but
/// broken by a missing DLL looks identical in a listing and fails only when a
/// plugin tries to start, which is the wrong moment to learn it.
fn interpreter_available(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "forge plugin: {message}\n\n{USAGE}");
    EXIT_USAGE
}

fn fail(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "{}", one_line(message));
    EXIT_FAILURE
}

/// The plugin root a turn would use, for callers that already have a config.
pub fn root_for(cfg: &Config) -> PathBuf {
    PathBuf::from(cfg.plugin_root())
}

/// True when a root directory exists. Exposed for tests and for reporting.
pub fn root_exists(root: &Path) -> bool {
    root.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn positionals_skip_flag_values() {
        // `--dir X` must not make X look like the plugin name.
        let source = args(&["start", "--dir", "/plugins", "alpha"]);
        assert_eq!(positional(&source, "start"), vec!["alpha"]);
        let source = args(&["call", "--args", "{}", "alpha", "echo"]);
        assert_eq!(positional(&source, "call"), vec!["alpha", "echo"]);
        let source = args(&["start", "--dir=/plugins", "alpha"]);
        assert_eq!(positional(&source, "start"), vec!["alpha"]);
        let source = args(&["list", "--json"]);
        assert!(positional(&source, "list").is_empty());
    }

    #[test]
    fn a_plugin_name_is_not_a_flag() {
        // The real invocation is `forge plugin start reference`: the plugin name is
        // a bare word. Reading it as a flag made every concrete command a usage
        // error, so this asserts the parser only looks at things that start with
        // `-`.
        let mut err = Vec::new();
        let outcome = parse(&args(&["start", "reference"]), &mut err);
        assert!(outcome.is_ok(), "stderr: {}", String::from_utf8_lossy(&err));
        let mut err = Vec::new();
        let outcome = parse(
            &args(&["call", "reference", "echo", "--args", "{}"]),
            &mut err,
        );
        let opt = outcome.expect("positionals parse");
        assert_eq!(opt.args, "{}");
        assert!(err.is_empty());
    }

    #[test]
    fn a_negative_or_broken_timeout_is_a_usage_error() {
        let mut err = Vec::new();
        let outcome = parse(&args(&["list", "--timeout", "0"]), &mut err);
        assert!(
            outcome.is_err(),
            "zero must be refused, not treated as default"
        );
        let mut err = Vec::new();
        let outcome = parse(&args(&["list", "--timeout", "soon"]), &mut err);
        assert!(outcome.is_err());
        let mut err = Vec::new();
        let outcome = parse(&args(&["list", "--timeout", "250"]), &mut err);
        assert_eq!(outcome.expect("a number parses").timeout, 250);
    }

    #[test]
    fn the_usage_names_every_command() {
        for command in ["list", "start", "stop", "call", "doctor"] {
            assert!(USAGE.contains(command), "usage must name {command}");
        }
    }

    #[test]
    fn an_unknown_command_is_a_usage_error_and_prints_the_usage() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&args(&["nope"]), &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("unknown command"));
        assert!(out.is_empty());
    }

    #[test]
    fn listing_an_empty_root_is_success_and_says_so() {
        let dir = std::env::temp_dir().join(format!(
            "forge-plugin-cmd-{}-{}",
            std::process::id(),
            forge_core::new_id().unwrap_or_default()
        ));
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["list", "--dir", &dir.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK);
        assert!(String::from_utf8_lossy(&out).contains("no plugins installed"));
        assert!(!dir.exists(), "listing must not create the plugin root");
    }
}
