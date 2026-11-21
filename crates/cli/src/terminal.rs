//! The browser terminal's process runner.
//!
//! Layer L3. `hearth-server` owns the socket, the frames and the two log lines;
//! this module owns the process, because starting one needs `hearth-exec` and a
//! sandbox, and the HTTP layer must not be able to do that on its own.
//!
//! Two decisions live here rather than in the server:
//!
//! - **No approval per command.** A person at the keyboard is who the approval
//!   prompt protects; asking them to approve their own keystroke is theatre. The
//!   line is still recorded (`terminal/run` / `terminal/end`, written by the
//!   server), and the sandbox is installed on the child, so the terminal is
//!   strictly less capable than the `shell` tool it sits beside.
//! - **No deadline.** A build takes as long as it takes and the person watching
//!   decides when to stop it; the cancel frame is the only way a run ends early,
//!   and a socket that closes cancels too.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use hearth_core::Result;
use hearth_exec::{Command, Pipe, Stream as ExecStream};
use hearth_server::api::{TerminalOutcome, TerminalRequest, TerminalRunner, TerminalSink};

/// How much of each stream `terminal/end` keeps.
///
/// The live socket carries the whole stream; this is what survives for a reader
/// who arrives after the fact, and 4 KiB is about what a scrollback line-up of
/// a build failure needs. The ADR's original 8 KiB was cut because two tails ride
/// in one event and the log is append-only.
const TAIL_BYTES: usize = 4 << 10;

/// How often the cancel token is checked. Ten milliseconds matches exec's own
/// wait loop: any slower and a `Ctrl+C` on a fast command loses the race.
const CANCEL_POLL: Duration = Duration::from_millis(10);

/// Runs terminal commands for one session.
pub struct Terminal {
    enforcer: Arc<dyn hearth_sandbox::Enforcer>,
}

impl Terminal {
    pub fn new(enforcer: Arc<dyn hearth_sandbox::Enforcer>) -> Self {
        Self { enforcer }
    }
}

/// Bridges the sandbox crate's enforcer onto `hearth-exec`'s trait.
///
/// The two traits have the same shape but live in crates that must not know
/// about each other (exec is L1 and has no sandbox dependency). Without this
/// bridge the terminal would run unconfined while `--sandbox` claimed otherwise.
struct Enforced(Arc<dyn hearth_sandbox::Enforcer>);

impl hearth_exec::Sandbox for Enforced {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn apply(&self, command: &mut std::process::Command) -> Result<()> {
        self.0.apply(command)
    }
    fn abort_apply(&self) {
        self.0.abort_apply()
    }
    fn after_start(&self, pid: u32) -> Result<()> {
        self.0.after_start(pid)
    }
}

impl TerminalRunner for Terminal {
    fn run(
        &self,
        request: &TerminalRequest,
        sink: Arc<dyn TerminalSink>,
    ) -> Result<TerminalOutcome> {
        let started = Instant::now();
        let line = request.command.trim().to_string();
        if line.is_empty() {
            return Ok(refused("the command is empty"));
        }
        // The same list the `shell` tool refuses. A person can still run these
        // by hand in their own terminal; what this surface must not do is run
        // them with the workspace's blessing and no second thought.
        if let Some(reason) = hearth_tool::builtin::dangerous(&line) {
            return Ok(refused(&reason));
        }
        let (path, argv, cmd_line) = hearth_tool::builtin::shell_invocation(&line);
        let observer = {
            let sink = Arc::clone(&sink);
            Arc::new(move |stream: ExecStream, bytes: &[u8]| {
                sink.output(stream.as_str(), bytes);
            })
        };
        let mut command = Command::new(path)
            .args(argv)
            .dir(request.cwd.to_string_lossy().to_string())
            .sandbox(Arc::new(Enforced(Arc::clone(&self.enforcer))))
            .observe(observer);
        if let Some(line) = cmd_line {
            command = command.cmd_line(line);
        }
        // A watcher, because `run_with_cancel` wants a flag and the server
        // hands over a token: the bridge is this thread, not a second mechanism.
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let watcher = {
            let stop = Arc::clone(&stop);
            let cancelled = Arc::clone(&cancelled);
            let token = request.cancel.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if token.is_cancelled() {
                        cancelled.store(true, Ordering::Release);
                        return;
                    }
                    thread::sleep(CANCEL_POLL);
                }
            })
        };
        let outcome = Pipe.run_with_cancel_capture(command, &cancelled);
        stop.store(true, Ordering::Release);
        let _ = watcher.join();
        let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let was_cancelled = cancelled.load(Ordering::Acquire);
        match outcome {
            Ok(result) => Ok(TerminalOutcome {
                // A cancelled run has no exit status to report; saying -1 and
                // why is more honest than passing through whatever the killed
                // child happened to return.
                exit_code: if result.cancelled {
                    -1
                } else {
                    result.exit_code
                },
                duration_ms,
                cancelled: result.cancelled || was_cancelled,
                stdout_tail: tail(&result.stdout, result.truncated),
                stderr_tail: tail(&result.stderr, result.truncated),
                message: if result.cancelled {
                    "exec: cancelled".to_string()
                } else {
                    String::new()
                },
            }),
            Err(error) => Ok(TerminalOutcome {
                exit_code: -1,
                duration_ms,
                cancelled: was_cancelled,
                message: error.message().to_string(),
                ..TerminalOutcome::default()
            }),
        }
    }
}

/// A command that never started, in the same shape as one that did.
fn refused(reason: &str) -> TerminalOutcome {
    TerminalOutcome {
        exit_code: -1,
        message: reason.to_string(),
        ..TerminalOutcome::default()
    }
}

/// The end of one stream, as text, for the `terminal/end` line.
///
/// Cutting on a byte boundary can split a character, so the cut is moved
/// forward to the next boundary rather than emitting a replacement character at
/// the start of the kept tail: the text is a record, and a record that begins
/// with damage looks like the command did that.
fn tail(bytes: &[u8], truncated: bool) -> String {
    let mut start = bytes.len().saturating_sub(TAIL_BYTES);
    while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
        start += 1;
    }
    let mut text = String::from_utf8_lossy(&bytes[start..]).to_string();
    if truncated || start > 0 {
        text.insert_str(0, "(truncated)\n");
    }
    text
}
