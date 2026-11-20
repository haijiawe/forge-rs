//! process spawn, tree kill, sandbox hook.
//!
//! Layer L1. The crate knows how to start one child process, capture a bounded
//! amount of its output, and guarantee that the whole process tree is gone when
//! the call returns (on timeout or cancellation). It deliberately does not know
//! what the process means: no shell parsing, no tools, no sessions.
//!
//! A non-zero exit code is data, not an error. This matters because the tool
//! layer reports the child's own failure to the model; turning it into a Rust
//! `Err` here would collapse "the command ran and said 3" into "the harness
//! could not run the command".

#![forbid(unsafe_op_in_unsafe_fn)]

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use hearth_core::Result as CoreResult;
pub use hearth_core::{Error, ErrorKind};

mod buffer;
mod cmdline;
mod env;
mod tree;

use buffer::CappedBuffer;

/// Default per-stream output cap: 1 MiB. The tool layer may raise it per call,
/// but every caller gets a finite cap so one chatty child cannot exhaust the
/// harness's memory.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1 << 20;

/// The one explicit opt-in to inherit the parent environment.
///
/// It is a NUL-prefixed string because no real environment variable can contain
/// a NUL; an ordinary-looking marker such as "inherit" could collide with a
/// user variable and silently widen the child's environment.
pub const ENV_INHERIT: &str = "\0forge:inherit";

/// Stable error text for an empty `Command::path`.
pub const ERR_EMPTY_PATH: &str = "exec: empty command path";
/// Stable error text for a timeout.
pub const ERR_TIMEOUT: &str = "exec: command timed out";
/// Stable error text for `cmd_line` on a platform without a raw command line.
pub const ERR_CMDLINE_UNSUPPORTED: &str = "exec: CmdLine is only supported on Windows";
/// Stable error text for both `args` and `cmd_line` being supplied.
pub const ERR_CMDLINE_CONFLICT: &str = "exec: CmdLine and Args are mutually exclusive";
/// Stable error text for a sandbox that refuses or fails to install.
pub const ERR_SANDBOX: &str = "exec: sandbox refused the process";

/// How long we keep draining a pipe after the child was reaped.
///
/// A grandchild can inherit the pipe handle and keep it open after the direct
/// child exits. Waiting forever for EOF would turn a successful command into a
/// hang; two seconds is the same grace period the Go implementation uses
/// (`cmd.WaitDelay`).
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// How often the wait loop checks the clock and the cancellation flag.
///
/// Ten milliseconds keeps cancellation responsive without busy-spinning a core
/// for the whole run.
const WAIT_POLL: Duration = Duration::from_millis(10);

/// A request to run one process.
#[derive(Clone)]
pub struct Command {
    /// Executable to start. Never a shell command line; see `cmd_line` for the
    /// one Windows escape hatch.
    pub path: String,
    /// Arguments passed verbatim (no shell expansion).
    pub args: Vec<String>,
    /// Working directory. `None` inherits the harness's directory.
    pub dir: Option<String>,
    /// `None` uses the minimal whitelist; `Some(vec![])` adds only the loader
    /// minimum; `Some(list)` uses exactly `list` (plus loader names on Windows).
    pub env: Option<Vec<String>>,
    /// Bytes written to the child's stdin. `None` still means EOF, never the
    /// harness's own stdin: a tool must not be able to read the terminal.
    pub stdin: Option<Vec<u8>>,
    /// Whole-invocation deadline. `None` means no deadline.
    pub timeout: Option<Duration>,
    /// Per-stream cap. Zero means [`DEFAULT_MAX_OUTPUT_BYTES`].
    pub max_output_bytes: usize,
    /// Windows-only raw command line. Using it with `args` is a conflict.
    pub cmd_line: Option<String>,
    /// Per-process restrictions installed before spawn.
    pub sandbox: Option<Arc<dyn Sandbox>>,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            path: String::new(),
            args: Vec::new(),
            dir: None,
            env: None,
            stdin: None,
            timeout: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            cmd_line: None,
            sandbox: None,
        }
    }
}

impl Command {
    /// Start building a command for `path`.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// Append one argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append several arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set the working directory.
    pub fn dir(mut self, dir: impl Into<String>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Set the complete environment policy.
    pub fn env(mut self, env: Vec<String>) -> Self {
        self.env = Some(env);
        self
    }

    /// Set the bytes written to stdin.
    pub fn stdin(mut self, stdin: Vec<u8>) -> Self {
        self.stdin = Some(stdin);
        self
    }

    /// Set the invocation deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set the per-stream output cap.
    pub fn max_output_bytes(mut self, max: usize) -> Self {
        self.max_output_bytes = max;
        self
    }

    /// Set a Windows raw command line.
    pub fn cmd_line(mut self, line: impl Into<String>) -> Self {
        self.cmd_line = Some(line.into());
        self
    }

    /// Install a sandbox hook.
    pub fn sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }
}

/// What one process run produced.
#[derive(Debug, Default, Clone)]
pub struct Result {
    /// `-1` when nothing ran (validation or sandbox refusal), otherwise the
    /// child's exit status.
    pub exit_code: i32,
    /// Captured standard output, possibly a prefix when `truncated`.
    pub stdout: Vec<u8>,
    /// Captured standard error, capped independently of stdout.
    pub stderr: Vec<u8>,
    /// Wall-clock time from spawn attempt to reap (including pipe drain).
    pub duration: Duration,
    /// At least one stream hit the cap and bytes were discarded.
    pub truncated: bool,
}

/// The standard runner: pipes for stdout/stderr, no shell, no terminal.
#[derive(Debug, Default, Clone, Copy)]
pub struct Pipe;

/// A runner is the seam that lets a test or a future PTY implementation replace
/// process creation without changing callers.
pub trait Runner {
    fn run(&self, command: Command) -> CoreResult<Result>;
}

impl Runner for Pipe {
    fn run(&self, command: Command) -> CoreResult<Result> {
        run_command(command, &AtomicBool::new(false))
    }
}

impl Pipe {
    /// Run a command with no external cancellation token.
    ///
    /// Kept as an inherent method as well as the [`Runner`] implementation so a
    /// caller can use the concrete runner without importing the trait; the
    /// trait object is still the seam tests and future PTY runners use.
    pub fn run(&self, command: Command) -> CoreResult<Result> {
        run_command(command, &AtomicBool::new(false))
    }

    /// Run with an explicit cancellation flag.
    ///
    /// Rust's standard library has no context type; an `AtomicBool` is the
    /// smallest signal that can be shared across threads without adding a
    /// dependency. The flag is checked before the exit code so a cancel that
    /// races the child's own exit is reported as a cancellation, not as data.
    pub fn run_with_cancel(&self, command: Command, cancelled: &AtomicBool) -> CoreResult<Result> {
        run_command(command, cancelled)
    }
}

/// A per-process restriction installed before the child exists.
///
/// Rust has no optional interfaces. The two hooks that Go expressed as separate
/// interfaces (`SandboxAborter`, `SandboxHook`) are default methods here: an
/// implementation overrides `abort_apply` and/or `after_start` only when it
/// needs them, and the runner always calls the default no-op otherwise.
///
/// `apply` receives a `&mut std::process::Command` rather than a custom builder
/// because that is the type the runner ultimately spawns. Implementations that
/// need to wrap the executable (Linux Landlock, macOS sandbox-exec) may replace
/// the command; the runner installs stdio *after* `apply` returns so a replaced
/// command does not lose its pipes.
pub trait Sandbox: Send + Sync {
    /// Stable name used in error messages and `doctor` output.
    fn name(&self) -> &str;
    /// Install restrictions. Returning `Err` means nothing may be started.
    fn apply(&self, command: &mut std::process::Command) -> CoreResult<()>;
    /// Called exactly once when `apply` succeeded but `spawn` failed, so a
    /// queued job handle (Windows) or temporary profile (macOS) is released.
    fn abort_apply(&self) {}
    /// Called once immediately after `spawn` succeeds.
    ///
    /// On Windows this assigns the suspended child to the sandbox job and
    /// resumes its main thread. An error here kills the tree: a child that
    /// started without its restrictions must not keep running.
    fn after_start(&self, _pid: u32) -> CoreResult<()> {
        Ok(())
    }
}

fn run_command(mut command: Command, cancelled: &AtomicBool) -> CoreResult<Result> {
    let started = Instant::now();
    if command.path.is_empty() {
        return Err(Error::new(ErrorKind::Invalid, ERR_EMPTY_PATH));
    }
    validate_cmd_line(&command.cmd_line, &command.args)?;

    let limit = if command.max_output_bytes == 0 {
        DEFAULT_MAX_OUTPUT_BYTES
    } else {
        command.max_output_bytes
    };

    let mut process = std::process::Command::new(&command.path);
    process.args(&command.args);
    if let Some(dir) = &command.dir {
        process.current_dir(dir);
    }
    env::apply(&mut process, command.env.as_deref());
    cmdline::apply(&mut process, command.cmd_line.as_deref())?;

    // The tree job is created before spawn so the child cannot outrun us
    // between CreateProcess and assignment. Failure to create it is not fatal
    // here; the direct-child kill below still runs.
    let mut tree = tree::ProcessTree::new();

    if let Some(sandbox) = &command.sandbox {
        if let Err(error) = sandbox.apply(&mut process) {
            return Err(Error::new(
                ErrorKind::Refused,
                format!(
                    "{ERR_SANDBOX}: {}: {}: {}",
                    command.path,
                    sandbox.name(),
                    error.message()
                ),
            ));
        }
    }

    // Configure the process group after the sandbox hook: Unix enforcers rebuild
    // the command to insert a wrapper executable, and a rebuild would drop a
    // process group configured beforehand. Without the group, a timeout would
    // kill only the wrapper and leave the real child running.
    tree::configure(&mut process);

    // stdio is installed after sandbox.apply because Unix enforcers rebuild the
    // command to insert a wrapper executable.
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::piped());
    process.stdin(std::process::Stdio::piped());

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            if let Some(sandbox) = &command.sandbox {
                sandbox.abort_apply();
            }
            return Err(Error::new(
                ErrorKind::Io,
                format!("exec: {}: {}", command.path, error),
            ));
        }
    };

    tree.attach(child.id());

    let (stdout, stdout_rx) = spawn_reader(child.stdout.take(), limit);
    let (stderr, stderr_rx) = spawn_reader(child.stderr.take(), limit);

    if let Some(mut sink) = child.stdin.take() {
        let input = command.stdin.take().unwrap_or_default();
        // Writing stdin on its own thread prevents a child that writes a lot
        // before reading from deadlocking the wait loop on a full stdin pipe.
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = sink.write_all(&input);
            // Dropping `sink` closes the pipe, which is the EOF the child needs.
        });
    }

    let mut sandbox_error: Option<Error> = None;
    if let Some(sandbox) = &command.sandbox {
        if let Err(error) = sandbox.after_start(child.id()) {
            sandbox_error = Some(Error::new(
                ErrorKind::Refused,
                format!(
                    "{ERR_SANDBOX}: {}: {}: {}",
                    command.path,
                    sandbox.name(),
                    error.message()
                ),
            ));
            tree.kill(&mut child);
        }
    }

    let deadline = command.timeout.map(|timeout| started + timeout);
    let mut timed_out = false;
    let mut was_cancelled = false;
    let status = loop {
        // Check the cancellation flag before try_wait: if the caller cancelled
        // while the child was exiting, the cancellation is the reason the run
        // stopped, not the exit code the child happened to produce.
        if cancelled.load(Ordering::SeqCst) {
            was_cancelled = true;
            tree.kill(&mut child);
            break child.wait().ok();
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                tree.kill(&mut child);
                let _ = child.wait();
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("exec: {}: wait: {}", command.path, error),
                ));
            }
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                timed_out = true;
                tree.kill(&mut child);
                break child.wait().ok();
            }
        }
        let sleep_for = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(WAIT_POLL)
            .min(WAIT_POLL);
        std::thread::sleep(sleep_for);
    };

    // The same precedence Go's context gives: a cancellation that landed before
    // the exit is reported as cancellation even if the exit was also observed.
    if cancelled.load(Ordering::SeqCst) {
        was_cancelled = true;
    }
    if let Some(deadline) = deadline {
        if !timed_out && Instant::now() >= deadline {
            timed_out = true;
        }
    }

    tree.close();

    // A surviving grandchild may still hold the pipe write end. Give the
    // readers the documented grace period, then stop waiting; the bytes
    // captured so far are still returned.
    let _ = stdout_rx.recv_timeout(PIPE_DRAIN_GRACE);
    let _ = stderr_rx.recv_timeout(PIPE_DRAIN_GRACE);

    let (stdout, stdout_truncated) = take_capture(&stdout);
    let (stderr, stderr_truncated) = take_capture(&stderr);
    let duration = started.elapsed();

    let exit_code = status.and_then(|status| status.code()).unwrap_or(-1);
    let mut result = Result {
        exit_code,
        stdout,
        stderr,
        duration,
        truncated: stdout_truncated || stderr_truncated,
    };

    if let Some(error) = sandbox_error {
        return Err(error);
    }
    if was_cancelled {
        result
            .stderr
            .extend_from_slice(b"exec: cancelled before the child exited\n");
        return Err(Error::new(
            ErrorKind::Refused,
            format!("exec: {}: cancelled", command.path),
        ));
    }
    if timed_out {
        // The caller gets a typed error *and* a note in stderr so a log that
        // only records output still says why the process stopped.
        result
            .stderr
            .extend_from_slice(b"exec: stopped after timeout\n");
        return Err(Error::new(
            ErrorKind::Refused,
            format!(
                "exec: {}: stopped after timeout {}: {}",
                command.path,
                command
                    .timeout
                    .map(format_duration)
                    .unwrap_or_else(|| "0s".to_string()),
                ERR_TIMEOUT
            ),
        ));
    }
    Ok(result)
}

fn format_duration(duration: Duration) -> String {
    if duration.subsec_nanos() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{:?}", duration)
    }
}

/// Validate the Windows raw-command-line escape hatch.
///
/// Conflict is checked before platform so a caller that supplied both fields
/// gets the actionable error on every OS, not "unsupported" on Unix.
fn validate_cmd_line(cmd_line: &Option<String>, args: &[String]) -> CoreResult<()> {
    let Some(line) = cmd_line else {
        return Ok(());
    };
    if line.is_empty() {
        return Ok(());
    }
    if !args.is_empty() {
        return Err(Error::new(ErrorKind::Invalid, ERR_CMDLINE_CONFLICT));
    }
    if !cfg!(windows) {
        return Err(Error::new(ErrorKind::Refused, ERR_CMDLINE_UNSUPPORTED));
    }
    Ok(())
}

fn spawn_reader<R>(
    reader: Option<R>,
    limit: usize,
) -> (Arc<Mutex<CappedBuffer>>, mpsc::Receiver<()>)
where
    R: Read + Send + 'static,
{
    let buffer = Arc::new(Mutex::new(CappedBuffer::new(limit)));
    let (sender, receiver) = mpsc::channel();
    let Some(mut reader) = reader else {
        let _ = sender.send(());
        return (buffer, receiver);
    };
    let shared = Arc::clone(&buffer);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => lock_buffer(&shared).push(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = sender.send(());
    });
    (buffer, receiver)
}

fn lock_buffer(buffer: &Arc<Mutex<CappedBuffer>>) -> MutexGuard<'_, CappedBuffer> {
    match buffer.lock() {
        Ok(guard) => guard,
        // A reader thread cannot run user code while holding this lock; a
        // poisoned mutex still contains the bytes read so far.
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn take_capture(buffer: &Arc<Mutex<CappedBuffer>>) -> (Vec<u8>, bool) {
    let mut guard = lock_buffer(buffer);
    let truncated = std::mem::replace(&mut guard.truncated, false);
    (std::mem::take(&mut guard.data), truncated)
}
