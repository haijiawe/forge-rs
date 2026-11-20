//! The shell builtin. All process creation goes through `hearth-exec`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hearth_exec::{Command, Pipe, Result as ExecResult, Runner, Sandbox, ERR_SANDBOX, ERR_TIMEOUT};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::builtin::danger::dangerous;
use crate::contract::{decode_args, Call, Result, CODE_CANCELLED, CODE_TIMEOUT};
use crate::truncate::{truncate, Limits};
use crate::Tool;

pub const SHELL_CODE_EMPTY_COMMAND: &str = "empty_command";
pub const SHELL_CODE_BLOCKED: &str = "blocked";
pub const SHELL_CODE_EXEC_FAILED: &str = "exec_failed";
pub const SHELL_CODE_SANDBOX: &str = "sandbox_refused";

const DEFAULT_TIMEOUT_MS: i64 = 120_000;
const MAX_TIMEOUT_MS: i64 = 600_000;
const STDERR_MARKER: &str = "[stderr]";

/// One command line in, one bounded result out.
pub struct ShellTool {
    limits: Limits,
    runner: Option<Arc<dyn Runner + Send + Sync>>,
    sandbox: Option<Arc<dyn Sandbox>>,
    cancelled: Option<Arc<AtomicBool>>,
}

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
    #[serde(default, rename = "timeout_ms")]
    timeout_ms: i64,
    #[serde(default)]
    description: String,
}

impl ShellTool {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            runner: None,
            sandbox: None,
            cancelled: None,
        }
    }

    pub fn with_runner(mut self, runner: Arc<dyn Runner + Send + Sync>) -> Self {
        self.runner = Some(runner);
        self
    }

    pub fn with_sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    pub fn with_cancel(mut self, cancelled: Arc<AtomicBool>) -> Self {
        self.cancelled = Some(cancelled);
        self
    }

    fn effective_limits(&self) -> Limits {
        if self.limits.max_bytes <= 0 && self.limits.max_lines <= 0 {
            Limits::default()
        } else {
            self.limits
        }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Tool for ShellTool {
    fn spec(&self) -> hearth_llm::ToolSpec {
        let containment = match &self.sandbox {
            Some(sandbox) => format!(
                "Sandboxed: every command is submitted to the {} enforcer before the process starts, so the sandbox mode decides which filesystem paths and network endpoints the command can reach. If the enforcer refuses the process, nothing runs and the call reports that refusal instead of output. Do not assume a specific limit beyond what the sandbox mode promises.",
                sandbox.name()
            ),
            None => "Not sandboxed: once the policy layer allows the call, the command runs with the user's own privileges and can read, write or delete anything the user can, and reach the network. This tool contains nothing. Do not claim otherwise, and do not run commands you would not run yourself.".to_string(),
        };
        let description = format!(
            "Run a command line in the workspace shell and return its combined output.\n\nWhen to use: builds, test suites, package managers, git, and any other job that only exists as a CLI command. Use the fs.* tools to read and edit files instead: their arguments are structured, they enforce workspace containment, and their effects are easier for the user to audit.\n\nShell: cmd.exe /c on Windows, /bin/sh -c everywhere else. Write sh syntax; it is the lowest common denominator and also runs under bash. The command line reaches the shell exactly as written, so quotes and the metacharacters inside them (&, |, <, >) are delivered verbatim, not re-escaped.\n\nExit status is data, not an error: a non-zero exit returns normally, with the code on the first line and Meta.exitCode set, so a failing test run is something you read and act on. Only a timeout, a cancellation, or a shell that could not start is reported as an error.\n\n{containment}\n\nEnvironment: the child gets a minimal environment (PATH, HOME, TEMP, SystemRoot, ...), not the harness's full environment, so credentials held by the harness are not exposed to the command.\n\nWindows: quotes are not a problem — write them as you would in a terminal, and a quoted path with spaces works. A line break still is: cmd.exe ends the command at the first one and quietly runs only the first line, so chain steps with && (or &), or make separate calls, or put multi-line scripts in a .cmd file and run that file.\n\nExample arguments:\n{{\"command\":\"go test ./...\",\"timeout_ms\":120000,\"description\":\"Run the full test suite\"}}"
        );
        hearth_llm::ToolSpec {
            name: "shell".into(),
            description,
            schema: json!({
                "type":"object",
                "properties":{
                    "command":{"type":"string","description":"Command line to run. Parsed by cmd.exe on Windows and by /bin/sh elsewhere, so prefer sh-compatible syntax. Empty or whitespace-only is rejected."},
                    "timeout_ms":{"type":"integer","description":"Timeout in milliseconds for the whole command, including its child processes. Defaults to 120000; values above 600000 are clamped to 600000 and the output says so. A value of 0 or less means the default."},
                    "description":{"type":"string","description":"Short human-readable label for the command (5-10 words), shown in the transcript and in the approval prompt. It does not affect execution."}
                },
                "required":["command"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: ShellArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        let command = args.command.trim().to_string();
        let limits = self.effective_limits();
        if command.is_empty() {
            return Result::error(
                SHELL_CODE_EMPTY_COMMAND,
                "shell: command is empty; pass the command line to run in \"command\", for example {\"command\":\"go test ./...\"}",
            );
        }

        // Danger is checked before any process starts and never consults policy
        // or sandbox mode. Nothing above this line can turn the refusal off.
        if let Some(reason) = dangerous(&command) {
            let (output, truncated) = truncate(&format!("blocked: {reason}\n\n{command}"), limits);
            return Result {
                output,
                truncated,
                is_error: true,
                code: SHELL_CODE_BLOCKED.to_string(),
                meta: meta_of([("blocked", json!(reason))]),
                parts: Vec::new(),
            };
        }

        let (timeout_ms, requested_ms, clamped) = shell_timeout(args.timeout_ms);
        let dir = if call.workspace.is_empty() {
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            call.workspace.clone()
        };
        let (path, argv, cmd_line) = shell_invocation(&command);
        let mut exec_command = Command::new(path)
            .args(argv)
            .dir(dir)
            .timeout(Duration::from_millis(timeout_ms as u64));
        if let Some(line) = cmd_line {
            exec_command = exec_command.cmd_line(line);
        }
        if let Some(sandbox) = &self.sandbox {
            exec_command = exec_command.sandbox(Arc::clone(sandbox));
        }

        let cancelled = self
            .cancelled
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let (executed, run_error) = match &self.runner {
            Some(runner) => match runner.run(exec_command) {
                Ok(result) => (result, None),
                Err(err) => (empty_exec_result(), Some(err)),
            },
            None => match Pipe.run_with_cancel(exec_command, &cancelled) {
                Ok(result) => (result, None),
                Err(err) => (empty_exec_result(), Some(err)),
            },
        };

        let mut meta = vec![
            ("exitCode", json!(executed.exit_code)),
            ("timeoutMs", json!(timeout_ms)),
            ("durationMs", json!(executed.duration.as_millis() as i64)),
            ("stdoutBytes", json!(executed.stdout.len() as i64)),
            ("stderrBytes", json!(executed.stderr.len() as i64)),
        ];
        if let Some(sandbox) = &self.sandbox {
            meta.push(("sandbox", json!(sandbox.name())));
        }
        if !args.description.is_empty() {
            meta.push(("description", json!(args.description)));
        }
        if clamped {
            meta.push(("timeoutRequestedMs", json!(requested_ms)));
            meta.push(("timeoutClamped", json!(true)));
        }
        if executed.truncated {
            meta.push(("captureTruncated", json!(true)));
        }
        let note = shell_line_break_warning(&command);
        if !note.is_empty() {
            meta.push(("lineBreakTruncated", json!(true)));
        }
        let meta = meta_of(meta);

        match run_error {
            None => {
                let header = with_note(
                    &note,
                    &success_header(&executed, clamped, requested_ms, timeout_ms),
                );
                let (output, cut) = truncate(
                    &compose(&header, &executed.stdout, &executed.stderr),
                    limits,
                );
                Result {
                    output,
                    truncated: cut || executed.truncated,
                    is_error: false,
                    code: String::new(),
                    meta,
                    parts: Vec::new(),
                }
            }
            Some(err) => {
                let message = err.message();
                let (code, header) = if cancelled.load(Ordering::Relaxed) {
                    (
                        CODE_CANCELLED,
                        format!(
                            "[cancelled] the command was stopped after {} ms; it and every process it started were killed",
                            executed.duration.as_millis()
                        ),
                    )
                } else if message.starts_with(ERR_TIMEOUT) {
                    (
                        CODE_TIMEOUT,
                        format!(
                            "[timeout] the command was stopped after {} ms (timeout_ms={})\n[hint] if it was still doing useful work, raise timeout_ms (maximum {}) or run it in the background",
                            executed.duration.as_millis(),
                            timeout_ms,
                            MAX_TIMEOUT_MS
                        ),
                    )
                } else if message.starts_with(ERR_SANDBOX) {
                    (
                        SHELL_CODE_SANDBOX,
                        format!("[sandbox] the command was not run: {message}\n[hint] the sandbox enforcer refused this process, so nothing executed. Fix the enforcer or the command; do not retry expecting a different result"),
                    )
                } else {
                    (
                        SHELL_CODE_EXEC_FAILED,
                        format!("[error] the command could not be run: {message}"),
                    )
                };
                let header = with_note(&note, &header);
                let (output, cut) = truncate(
                    &compose(&header, &executed.stdout, &executed.stderr),
                    limits,
                );
                Result {
                    output,
                    truncated: cut || executed.truncated,
                    is_error: true,
                    code: code.to_string(),
                    meta,
                    parts: Vec::new(),
                }
            }
        }
    }
}

fn meta_of<'a>(entries: impl IntoIterator<Item = (&'a str, Value)>) -> crate::contract::Meta {
    let mut meta = crate::contract::Meta::new();
    for (key, value) in entries {
        meta.insert(key.to_string(), value);
    }
    meta
}

fn empty_exec_result() -> ExecResult {
    ExecResult {
        exit_code: -1,
        stdout: Vec::new(),
        stderr: Vec::new(),
        duration: Duration::ZERO,
        truncated: false,
    }
}

fn shell_timeout(ms: i64) -> (i64, i64, bool) {
    if ms <= 0 {
        (DEFAULT_TIMEOUT_MS, 0, false)
    } else if ms > MAX_TIMEOUT_MS {
        (MAX_TIMEOUT_MS, ms, true)
    } else {
        (ms, ms, false)
    }
}

/// Returns the executable, argv, and (on Windows only) the raw command line.
fn shell_invocation(command: &str) -> (String, Vec<String>, Option<String>) {
    if cfg!(windows) {
        let shell = std::env::var("COMSPEC").unwrap_or_default();
        let shell = if shell.is_empty() {
            "cmd.exe".to_string()
        } else {
            shell
        };
        (shell, Vec::new(), Some(shell_cmd_line(command)))
    } else {
        (
            "/bin/sh".to_string(),
            vec!["-c".to_string(), command.to_string()],
            None,
        )
    }
}

/// Compose the raw line cmd.exe receives. Quotes are not backslash-escaped.
fn shell_cmd_line(command: &str) -> String {
    if command.starts_with('"') {
        format!("/c \"{command}\"")
    } else {
        format!("/c {command}")
    }
}

fn shell_line_break_warning(command: &str) -> String {
    if !cfg!(windows) {
        return String::new();
    }
    let trimmed = command.trim_end_matches([' ', '\t', '\r', '\n']);
    if !trimmed.contains(['\r', '\n']) {
        return String::new();
    }
    "[note] cmd.exe ends a command line at the first line break, so only the first line of this command ran. Join the steps with && (or &) to run them as one line, or make separate calls, or put the lines in a .cmd file and run that file.".to_string()
}

fn with_note(note: &str, header: &str) -> String {
    if note.is_empty() {
        header.to_string()
    } else {
        format!("{note}\n{header}")
    }
}

fn success_header(
    result: &ExecResult,
    clamped: bool,
    requested_ms: i64,
    timeout_ms: i64,
) -> String {
    let mut lines = Vec::new();
    if clamped {
        lines.push(format!(
            "[note] timeout_ms {requested_ms} is above the {MAX_TIMEOUT_MS} ms maximum; using {timeout_ms} ms"
        ));
    }
    if result.truncated {
        lines.push(format!(
            "[note] the command wrote more than the {} MiB capture cap on a stream; what follows is a prefix",
            hearth_exec::DEFAULT_MAX_OUTPUT_BYTES >> 20
        ));
    }
    lines.push(format!("[exit code {}]", result.exit_code));
    lines.join("\n")
}

fn compose(header: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let mut out = header.to_string();
    let stdout = String::from_utf8_lossy(stdout);
    let stdout = stdout.trim_end_matches('\n');
    if !stdout.is_empty() {
        out.push_str("\n\n");
        out.push_str(stdout);
    }
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim_end_matches('\n');
    if !stderr.is_empty() {
        out.push_str("\n\n");
        out.push_str(STDERR_MARKER);
        out.push('\n');
        out.push_str(stderr);
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeRunner {
        result: Mutex<Option<hearth_core::Result<ExecResult>>>,
        calls: AtomicBool,
    }

    impl Runner for FakeRunner {
        fn run(&self, _command: Command) -> hearth_core::Result<ExecResult> {
            self.calls.store(true, Ordering::SeqCst);
            self.result
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .take()
                .unwrap_or_else(|| Ok(empty_exec_result()))
        }
    }

    fn call(command: &str) -> Call {
        Call {
            id: "c1".into(),
            name: "shell".into(),
            args: json!({ "command": command }),
            workspace: std::path::PathBuf::from(".").to_string_lossy().into_owned(),
        }
    }

    #[test]
    fn non_zero_exit_is_data() {
        let runner = Arc::new(FakeRunner {
            result: Mutex::new(Some(Ok(ExecResult {
                exit_code: 7,
                stdout: b"doing work\n".to_vec(),
                stderr: Vec::new(),
                duration: Duration::from_millis(12),
                truncated: false,
            }))),
            calls: AtomicBool::new(false),
        });
        let tool = ShellTool::new(Limits::default()).with_runner(runner.clone());
        let result = tool.call(call("go test ./..."));
        assert!(result.is_ok());
        assert_eq!(result.code, "");
        assert_eq!(result.meta.get("exitCode"), Some(&json!(7)));
        assert!(result.output.contains("[exit code 7]"));
        assert!(result.output.contains("doing work"));
        assert!(runner.calls.load(Ordering::SeqCst));
    }

    #[test]
    fn dangerous_command_never_reaches_exec() {
        let runner = Arc::new(FakeRunner::default());
        let tool = ShellTool::new(Limits::default()).with_runner(runner.clone());
        let result = tool.call(call("rm -rf /"));
        assert_eq!(result.code, SHELL_CODE_BLOCKED);
        assert!(result.is_error);
        assert_eq!(
            result.meta.get("blocked"),
            Some(&json!("recursively deletes a root or home directory"))
        );
        assert!(!runner.calls.load(Ordering::SeqCst));
    }

    #[test]
    fn empty_command_is_rejected_before_exec() {
        let runner = Arc::new(FakeRunner::default());
        let tool = ShellTool::new(Limits::default()).with_runner(runner.clone());
        let result = tool.call(call("   "));
        assert_eq!(result.code, SHELL_CODE_EMPTY_COMMAND);
        assert!(!runner.calls.load(Ordering::SeqCst));
    }

    #[cfg(windows)]
    #[test]
    fn windows_cmd_line_keeps_quotes_verbatim() {
        assert_eq!(shell_cmd_line("echo \"a b\""), "/c echo \"a b\"");
        assert_eq!(
            shell_cmd_line("\"C:\\Program Files\\Go\\bin\\go.exe\" env \"GOROOT\""),
            "/c \"\"C:\\Program Files\\Go\\bin\\go.exe\" env \"GOROOT\"\""
        );
    }
}
