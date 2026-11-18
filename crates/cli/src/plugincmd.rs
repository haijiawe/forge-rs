//! `forge plugin`: create, list, start, stop and call plugins in
//! `<dataDir>/plugins`.
//!
//! One `Host` per invocation, created and dropped inside this command: a
//! subcommand is a process, so the lifetime is the process. This is deliberately
//! not the object `chat` holds for a turn — `chat` builds its own host and is the
//! only place that keeps children alive between turns.
//!
//! Nothing here spawns a plugin except `start` and `call`. `list` and `doctor`
//! read manifests and report; a listing that started every installed plugin to
//! describe it would be a listing that runs code the operator only wanted to
//! inspect. `new` writes files and starts nothing either.
//!
//! This module owns the whole command vocabulary. `pluginscaffold` owns the
//! bytes `new` writes and knows nothing about flags or exit codes, so there is
//! one flag table for six subcommands rather than one per subcommand.

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
  new <name>           write a working plugin skeleton under the plugin root
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
  --runtime <name>     runtime for `new`: node (default), python or exec
  --description <text> one-line description for `new`
  --force              let `new` replace a directory that already exists
  -h, --help           this message

`new` writes a plugin that answers the handshake and one tool, so this sequence
works with no edits:

  forge plugin new hello
  forge plugin call hello hello --args '{\"name\":\"world\"}'

stdout: the listing, the handshake, the tool output, or the doctor report.
Diagnostics go to stderr.

exit codes: 0 ok, 1 failure, 2 usage
";

const VALUE_FLAGS: &[&str] = &[
    "data-dir",
    "dir",
    "args",
    "timeout",
    "runtime",
    "description",
];

#[derive(Debug, Default)]
struct Options {
    json: bool,
    data_dir: String,
    dir: String,
    args: String,
    timeout: u64,
    runtime: String,
    description: String,
    force: bool,
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
        "new" => new_plugin(&opt, &reordered, stdout, stderr),
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
            "runtime" => opt.runtime = take(&mut i)?,
            "description" => opt.description = take(&mut i)?,
            "force" if inline.is_none() => opt.force = true,
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

/// `new`: write a plugin, then say what to do with it.
///
/// The next-step lines are part of the output on purpose. A generated directory
/// that nobody knows how to run is not a working starting point, and the two
/// facts a first-time author needs — that `call` works immediately, and that
/// `chat` needs `plugins.enabled` — are exactly the two that are not obvious
/// from the files.
fn new_plugin(
    opt: &Options,
    args: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let Some(name) = positional(args, "new")
        .first()
        .map(|name| (**name).to_string())
    else {
        return usage_error(stderr, "new needs a plugin name");
    };
    let kind = match crate::pluginscaffold::kind_of(&opt.runtime) {
        Ok(kind) => kind,
        Err(crate::pluginscaffold::Reject::Usage(message)) => return usage_error(stderr, &message),
        Err(crate::pluginscaffold::Reject::Failed(message)) => {
            return fail(stderr, &format!("forge plugin: {message}"))
        }
    };
    let root = match root_of(opt) {
        Ok(root) => root,
        Err(err) => return fail(stderr, &format!("forge plugin: {}", err.message())),
    };
    let outcome =
        match crate::pluginscaffold::generate(&root, &name, &kind, &opt.description, opt.force) {
            Ok(outcome) => outcome,
            Err(crate::pluginscaffold::Reject::Usage(message)) => {
                return usage_error(stderr, &message)
            }
            Err(crate::pluginscaffold::Reject::Failed(message)) => {
                return fail(stderr, &format!("forge plugin: {message}"))
            }
        };
    if opt.json {
        return crate::write_json(
            stdout,
            stderr,
            "plugin new",
            &serde_json::json!({
                "name": name,
                "runtime": outcome.runtime,
                "directory": outcome.directory.to_string_lossy(),
                "files": outcome.files,
            }),
        );
    }
    let _ = writeln!(
        stdout,
        "created {} plugin {} in {}",
        outcome.runtime,
        name,
        outcome.directory.display()
    );
    for file in &outcome.files {
        let _ = writeln!(stdout, "  {file}");
    }
    let _ = writeln!(stdout);
    let _ = writeln!(
        stdout,
        "next: forge plugin call {name} hello --args '{{\"name\":\"world\"}}'"
    );
    let _ = writeln!(
        stdout,
        "      add {name:?} to plugins.enabled in config.json to let chat use it"
    );
    EXIT_OK
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
    let probes = runtime_probes(&found);
    let entries = exec_entries(&found);
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
                "runtimes": probes
                    .iter()
                    .map(|probe| serde_json::json!({
                        "runtime": probe.runtime,
                        "available": probe.available,
                        "plugins": probe.plugins,
                    }))
                    .collect::<Vec<_>>(),
                "entries": entries
                    .iter()
                    .map(|entry| serde_json::json!({
                        "plugin": entry.plugin,
                        "entry": entry.entry,
                        "exists": entry.exists,
                    }))
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
    if probes.is_empty() && entries.is_empty() {
        let _ = writeln!(stdout, "runtimes:  none needed (nothing installed)");
    }
    for probe in &probes {
        let _ = writeln!(
            stdout,
            "runtime:   {:<8} {}",
            probe.runtime,
            if probe.available {
                "found".to_string()
            } else {
                format!(
                    "missing ({}) cannot start",
                    one_line(&probe.plugins.join(", "))
                )
            }
        );
    }
    for entry in &entries {
        let _ = writeln!(
            stdout,
            "entry:     {:<8} {} {}",
            entry.plugin,
            entry.entry,
            if entry.exists {
                "found"
            } else {
                "MISSING (this plugin cannot start)"
            }
        );
    }
    let _ = writeln!(
        stdout,
        "note:      this command starts nothing; `forge plugin start <name>` does."
    );
    // Exit 1 when something installed cannot start. A report that counted three
    // problems and exited 0 would be read as "all good" by every script that
    // only looks at the status.
    if !broken.is_empty()
        || probes.iter().any(|probe| !probe.available)
        || entries.iter().any(|entry| !entry.exists)
    {
        return EXIT_FAILURE;
    }
    EXIT_OK
}

/// One interpreter runtime the installed plugins ask for.
struct RuntimeProbe {
    runtime: &'static str,
    available: bool,
    /// Which plugins need it, so a missing interpreter names its victims.
    plugins: Vec<String>,
}

/// The interpreters to probe, derived from the manifests actually installed.
///
/// Hardcoding `node` and `python` was wrong in both directions: on a machine with
/// only native plugins it printed two runtimes nothing needed, and on a machine
/// without them it reported two failures while saying nothing about the binaries
/// the install really depends on. `exec` is absent here on purpose — it has no
/// interpreter, and its risk is a missing entry, which `exec_entries` checks.
fn runtime_probes(found: &[forge_plugin::Manifest]) -> Vec<RuntimeProbe> {
    let mut probes: Vec<RuntimeProbe> = Vec::new();
    for manifest in found {
        let program = manifest.runtime.command();
        if program.is_empty() {
            continue;
        }
        if let Some(probe) = probes
            .iter_mut()
            .find(|probe| probe.runtime == manifest.runtime.as_str())
        {
            probe.plugins.push(manifest.name.clone());
            continue;
        }
        probes.push(RuntimeProbe {
            runtime: manifest.runtime.as_str(),
            available: interpreter_available(program),
            plugins: vec![manifest.name.clone()],
        });
    }
    probes.sort_by(|left, right| left.runtime.cmp(right.runtime));
    for probe in &mut probes {
        probe.plugins.sort();
    }
    probes
}

/// One `exec` plugin's entry, and whether the file is there.
struct ExecEntry {
    plugin: String,
    entry: String,
    exists: bool,
}

/// Existence, not a spawn.
///
/// `--version` is not part of the plugin protocol: a native plugin reads frames
/// from stdin and would treat that argument as nothing at all, so probing it
/// would either hang or measure something unrelated. The failure this can
/// actually catch is an entry that was never copied in, or was deleted.
fn exec_entries(found: &[forge_plugin::Manifest]) -> Vec<ExecEntry> {
    let mut entries: Vec<ExecEntry> = found
        .iter()
        .filter(|manifest| manifest.runtime == forge_plugin::Runtime::Exec)
        .map(|manifest| ExecEntry {
            plugin: manifest.name.clone(),
            entry: manifest.entry.clone(),
            exists: manifest.directory.join(&manifest.entry).is_file(),
        })
        .collect();
    entries.sort_by(|left, right| left.plugin.cmp(&right.plugin));
    entries
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
        for command in ["new", "list", "start", "stop", "call", "doctor"] {
            assert!(USAGE.contains(command), "usage must name {command}");
        }
        for flag in ["--runtime", "--description", "--force"] {
            assert!(USAGE.contains(flag), "usage must name {flag}");
        }
    }

    /// A scratch plugin root that no other test shares.
    fn scratch(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("forge-plugin-new-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn new_writes_a_plugin_the_same_command_can_then_list() {
        // The point of folding `new` into this command rather than giving it its
        // own: the root it writes to is the root `list` reads, resolved by the
        // same `root_of`. Two resolvers would eventually disagree.
        let root = scratch("roundtrip");
        let root_text = root.to_string_lossy().to_string();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["new", "hello", "--dir", &root_text]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&err));
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("created node plugin hello"), "{text}");
        assert!(text.contains("plugins.enabled"), "{text}");

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&args(&["list", "--dir", &root_text]), &mut out, &mut err);
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&err));
        assert!(String::from_utf8_lossy(&out).contains("hello"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_accepts_json_like_every_other_subcommand() {
        // This is what the duplicated flag table could not do: `--json` belonged
        // to `plugincmd` and the scaffold's own parser rejected it.
        let root = scratch("json");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["new", "jsonly", "--json", "--dir", &root.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&err));
        let parsed: serde_json::Value =
            serde_json::from_slice(&out).expect("new --json must emit JSON");
        assert_eq!(parsed["name"], "jsonly");
        assert_eq!(parsed["runtime"], "node");
        assert_eq!(parsed["files"][0], "README.md");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_reads_its_name_past_flags_and_accepts_python() {
        let root = scratch("flags");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&[
                "--dir",
                &root.to_string_lossy(),
                "new",
                "--runtime",
                "python",
                "flagless",
            ]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&err));
        assert!(root.join("flagless/main.py").is_file());
        assert!(String::from_utf8_lossy(&out).contains("created python plugin flagless"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_without_a_name_is_a_usage_error_that_writes_nothing() {
        let root = scratch("noname");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["new", "--dir", &root.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("needs a plugin name"));
        assert!(!root.exists());
    }

    #[test]
    fn an_illegal_name_from_the_command_line_creates_nothing() {
        let root = scratch("illegal");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["new", "Bad Name", "--dir", &root.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_USAGE);
        assert!(!root.exists());
    }

    #[test]
    fn a_second_new_without_force_fails_and_keeps_the_first() {
        let root = scratch("twice");
        let root_text = root.to_string_lossy().to_string();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let path = args(&["new", "twice", "--dir", &root_text]);
        assert_eq!(run(&path, &mut out, &mut err), EXIT_OK);
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(run(&path, &mut out, &mut err), EXIT_FAILURE);
        assert!(String::from_utf8_lossy(&err).contains("--force"));
        assert!(root.join("twice/index.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
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

    /// Write a plugin directory with an explicit runtime and entry.
    fn install(root: &std::path::Path, name: &str, runtime: &str, entry: &str, body: Option<&str>) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("plugin dir");
        let manifest = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "description": "a doctor fixture",
            "runtime": runtime,
            "entry": entry,
        });
        std::fs::write(
            dir.join("plugin.json"),
            serde_json::to_vec(&manifest).expect("manifest"),
        )
        .expect("manifest write");
        if let Some(body) = body {
            std::fs::write(dir.join(entry), body).expect("entry write");
        }
    }

    #[test]
    fn doctor_probes_only_the_runtimes_the_installed_plugins_ask_for() {
        // A native-only install has no use for a Node probe. Printing "node
        // missing" here would name a problem that does not exist, and on a
        // machine without Node it would do that while saying nothing about the
        // binary the install actually depends on.
        let root = scratch("doctor-exec");
        install(
            &root,
            "native",
            "exec",
            "native.exe",
            Some("not really a binary"),
        );
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["doctor", "--dir", &root.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        let text = String::from_utf8_lossy(&out).to_string();
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(!text.contains("runtime:   node"), "{text}");
        assert!(!text.contains("runtime:   python"), "{text}");
        assert!(text.contains("entry:     native"), "{text}");
        // The entry is a text file, and doctor still calls it found: it checks
        // presence, never runs it. Spawning it would execute code the operator
        // only asked to inspect.
        assert!(text.contains("native.exe found"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn doctor_names_a_missing_exec_entry_and_exits_one() {
        let root = scratch("doctor-missing-entry");
        install(&root, "ghost", "exec", "absent.exe", None);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["doctor", "--dir", &root.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        let text = String::from_utf8_lossy(&out).to_string();
        assert_eq!(
            code, EXIT_FAILURE,
            "a plugin that cannot start is a failure"
        );
        assert!(text.contains("absent.exe MISSING"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn doctor_needs_no_runtime_when_nothing_is_installed() {
        let root = scratch("doctor-empty");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["doctor", "--dir", &root.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        let text = String::from_utf8_lossy(&out).to_string();
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("none needed (nothing installed)"), "{text}");
        assert!(!text.contains("runtime:   node"), "{text}");
        assert!(!root.exists(), "doctor must not create the plugin root");
    }

    #[test]
    fn doctor_lists_an_interpreter_once_with_every_plugin_that_needs_it() {
        let root = scratch("doctor-shared-runtime");
        install(&root, "alpha", "node", "index.js", Some("// alpha"));
        install(&root, "beta", "node", "index.js", Some("// beta"));
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["doctor", "--dir", &root.to_string_lossy(), "--json"]),
            &mut out,
            &mut err,
        );
        let value: serde_json::Value =
            serde_json::from_slice(&out).expect("doctor --json is one JSON value");
        let runtimes = value["runtimes"].as_array().expect("runtimes array");
        assert_eq!(runtimes.len(), 1, "one probe per runtime, not per plugin");
        assert_eq!(runtimes[0]["runtime"], "node");
        assert_eq!(
            runtimes[0]["plugins"],
            serde_json::json!(["alpha", "beta"]),
            "a missing interpreter has to name its victims"
        );
        assert_eq!(value["entries"], serde_json::json!([]));
        // Whether Node exists is a property of the machine, not of this code, so
        // the exit code is asserted against the probe rather than assumed.
        let available = runtimes[0]["available"].as_bool().expect("available bool");
        assert_eq!(code, if available { EXIT_OK } else { EXIT_FAILURE });
        let _ = std::fs::remove_dir_all(&root);
    }
}
