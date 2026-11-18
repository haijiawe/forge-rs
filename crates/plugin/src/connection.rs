//! The child process and the line-level plumbing.
//!
//! Two threads per plugin: one reads stdout into a channel, one drains stderr
//! into the log. Both are joined on `close`. The reader thread is what makes a
//! timeout possible at all — waiting on a `BufRead` directly would block the
//! caller forever, and a plugin that stops answering is the failure this design
//! has to survive.
//!
//! The child is killed, not asked, when a call times out. A plugin that ignored
//! a deadline once will ignore the next one, and a wedged child holding the
//! caller's thread is worse than losing its in-flight state. That is also why
//! "the child is gone" is a sticky fact on this struct rather than a `try_wait`
//! the next caller has to remember to make: the call *after* a timeout must say
//! `child_exited`, not hang.
//!
//! Why the reader enforces the byte cap itself instead of reading a line and
//! measuring it: a plugin that writes 5 MiB on one line has already allocated
//! 5 MiB inside the host before the check runs, which is exactly what the cap
//! exists to prevent. Bytes past the cap are counted and dropped, never
//! buffered; the line is drained so the stream stays at a frame boundary.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use forge_mcp::Logger;

use crate::error::{Error, Kind, Result};
use crate::manifest::Manifest;
use crate::protocol::{self, code, Message, Request, DEFAULT_MAX_BYTES};

const READ_BUFFER: usize = 32 << 10;
const STDERR_LINE_CAP: usize = 4 << 10;
const STDERR_TAIL: usize = 200;
/// How long `close` waits for the child to exit on its own before killing it.
pub const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// The id `close` uses for its `shutdown` request.
///
/// Reserved rather than allocated: the host's own ids start at 1, so 0 is never
/// in flight, and `close` has no access to the host's counter. A plugin that
/// echoes ids back correctly — which the protocol requires — answers 0 like any
/// other id.
const SHUTDOWN_ID: u64 = 0;

/// How long `close` waits for the `shutdown` reply before closing stdin anyway.
const SHUTDOWN_REPLY_WAIT: Duration = Duration::from_millis(500);

/// Something the reader thread saw.
enum Event {
    /// One complete line, newline stripped.
    Line(Vec<u8>),
    /// A line that ran past the cap; carries how many bytes it had.
    TooLarge(usize),
    /// The stream ended, with the reason to report.
    Eof(String),
}

/// How a plugin stopped, for [`crate::Host::unload`]'s report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    /// Whether the child exited on its own (a `shutdown` then EOF, or a crash)
    /// rather than being killed after the grace period.
    pub exited: bool,
    /// Whether the exit was a success. `false` covers both a crash and a kill.
    pub clean: bool,
    /// The exit code, when the platform reported one.
    pub code: Option<i32>,
    /// One line for a log: "exited with status 0", "killed after 2000 ms", ...
    pub detail: String,
}

/// One running plugin.
pub struct Connection {
    child: Child,
    pid: u32,
    /// `None` once the pipe has been closed on purpose; a closed pipe is how a
    /// plugin is told to exit, so the state has to be representable.
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
    stderr_log: Arc<Mutex<Vec<String>>>,
    logger: Arc<dyn Logger>,
    reader: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    /// Sticky: set the first time the child is known to be gone, never cleared.
    gone: Option<String>,
    name: String,
    max_bytes: usize,
    ignored: usize,
}

impl Connection {
    /// Spawn the plugin and start reading its output.
    ///
    /// The child inherits the host's environment on purpose: it is an installed
    /// program that has to find its own interpreter (`node`, `python`) and its
    /// own files, and an MCP-style curated environment would make `runtime:
    /// "node"` work only on machines where the host happened to add PATH.
    pub fn spawn(manifest: &Manifest, logger: Arc<dyn Logger>) -> Result<Connection> {
        Self::spawn_with_limit(manifest, logger, DEFAULT_MAX_BYTES)
    }

    pub fn spawn_with_limit(
        manifest: &Manifest,
        logger: Arc<dyn Logger>,
        max_bytes: usize,
    ) -> Result<Connection> {
        let argv = manifest.command_line()?;
        let program = &argv[0];
        let mut command = Command::new(program);
        command
            .args(&argv[1..])
            .current_dir(manifest.working_directory())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            Error::new(
                Kind::SpawnFailed,
                format!(
                    "{} ({}) could not run {program}: {error}",
                    manifest.name,
                    manifest.working_directory().display()
                ),
            )
        })?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or_else(|| {
            Error::new(
                Kind::SpawnFailed,
                format!("{}: the child has no stdin to write to", manifest.name),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            Error::new(
                Kind::SpawnFailed,
                format!("{}: the child has no stdout to read", manifest.name),
            )
        })?;
        let stderr = child.stderr.take();

        let max_bytes = max_bytes.max(1);
        let (sender, events) = mpsc::channel::<Event>();
        let reader = std::thread::Builder::new()
            .name(format!("plugin-{}-out", manifest.name))
            .spawn(move || read_events(stdout, max_bytes, sender))
            .map_err(|error| {
                Error::new(
                    Kind::SpawnFailed,
                    format!("{}: cannot start the reader thread: {error}", manifest.name),
                )
            })?;

        let stderr_log = Arc::new(Mutex::new(Vec::new()));
        let stderr_thread = stderr.and_then(|stderr| {
            let log = Arc::clone(&stderr_log);
            let logger = Arc::clone(&logger);
            let name = manifest.name.clone();
            std::thread::Builder::new()
                .name(format!("plugin-{name}-err"))
                .spawn(move || read_stderr(stderr, log, logger, name))
                .ok()
        });

        Ok(Connection {
            child,
            pid,
            stdin: Some(stdin),
            events,
            stderr_log,
            logger,
            reader: Some(reader),
            stderr_thread,
            gone: None,
            name: manifest.name.clone(),
            max_bytes,
            ignored: 0,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// How many plugin-initiated messages were skipped while waiting for a
    /// reply. Logged, and counted so a test can prove they were skipped rather
    /// than mistaken for answers.
    pub fn ignored_messages(&self) -> usize {
        self.ignored
    }

    /// Write one line, then wait for the reply with this id.
    ///
    /// The reply must carry the same id: a mismatched one is a protocol error
    /// and is *not* accepted, because a plugin that multiplexes replies would
    /// otherwise hand one call's result to another.
    pub fn round_trip(&mut self, id: u64, line: &[u8], timeout: Duration) -> Result<Message> {
        if let Some(reason) = self.gone.clone() {
            return Err(self.exited_error(&reason));
        }
        self.refresh_alive()?;
        if line.len() > self.max_bytes {
            return Err(Error::new(
                Kind::TooLarge,
                format!(
                    "{}: the request is {} bytes, over the {}-byte cap",
                    self.name,
                    line.len(),
                    self.max_bytes
                ),
            ));
        }
        self.write_frame(line)?;

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.timed_out(timeout));
            }
            match self.events.recv_timeout(remaining) {
                Ok(Event::Line(bytes)) => match self.accept(id, &bytes) {
                    Accepted::Reply(message) => return Ok(message),
                    Accepted::Skip => continue,
                    Accepted::Reject(error) => return Err(error),
                },
                Ok(Event::TooLarge(bytes)) => {
                    return Err(Error::new(
                        Kind::TooLarge,
                        format!(
                            "{}: a reply was {bytes} bytes, over the {}-byte cap; it was dropped, not buffered",
                            self.name, self.max_bytes
                        ),
                    ))
                }
                Ok(Event::Eof(reason)) => {
                    self.gone = Some(reason.clone());
                    return Err(self.exited_error(&reason));
                }
                Err(RecvTimeoutError::Timeout) => return Err(self.timed_out(timeout)),
                Err(RecvTimeoutError::Disconnected) => {
                    let reason = "the reader thread is gone".to_string();
                    self.gone = Some(reason.clone());
                    return Err(self.exited_error(&reason));
                }
            }
        }
    }

    /// Send a method call and return its `result`, with a plugin-reported
    /// JSON-RPC error turned into `bad_response`.
    ///
    /// A JSON-RPC error means the plugin could not attempt the call at all; a
    /// tool that ran and failed is `isError` in a normal result. Conflating the
    /// two would let a broken plugin look like a failing tool, which the model
    /// would then try to "fix" by calling it again.
    pub fn call(
        &mut self,
        id: u64,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let request = Request::new(id, method, params);
        let line = request.to_line()?;
        let reply = self.round_trip(id, &line, timeout)?;
        if let Some(wire) = reply.error {
            return Err(Error::new(
                Kind::BadResponse,
                format!("{}: {method} answered a JSON-RPC error: {wire}", self.name),
            ));
        }
        match reply.result {
            Some(result) => Ok(result),
            None => Err(Error::new(
                Kind::BadResponse,
                format!(
                    "{}: {method} answered with neither a result nor an error",
                    self.name
                ),
            )),
        }
    }

    /// Send a notification: no id, no reply, no way to fail quietly.
    pub fn notify(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<()> {
        let mut map = serde_json::Map::new();
        map.insert("jsonrpc".to_string(), serde_json::Value::from("2.0"));
        map.insert("method".to_string(), serde_json::Value::from(method));
        if let Some(params) = params {
            map.insert("params".to_string(), params);
        }
        let mut line = serde_json::to_vec(&serde_json::Value::Object(map)).map_err(|error| {
            Error::new(
                Kind::ProtocolError,
                format!("{}: cannot encode {method}: {error}", self.name),
            )
        })?;
        line.push(b'\n');
        self.write_frame(&line)
    }

    fn accept(&mut self, id: u64, bytes: &[u8]) -> Accepted {
        let message = match protocol::parse_message(bytes) {
            Ok(message) => message,
            Err(reason) => {
                return Accepted::Reject(Error::new(
                    Kind::BadResponse,
                    format!(
                        "{}: the reply was not a JSON-RPC message ({reason}); it began {}",
                        self.name,
                        protocol::quote_head(bytes)
                    ),
                ))
            }
        };
        // A plugin-initiated request is answered with method-not-found rather
        // than dropped: a plugin awaiting that reply would hang until its own
        // timeout, and "the host does not take callbacks" is a fact it can act
        // on. A notification needs no answer.
        if message.method.is_some() && !message.is_reply() {
            match message.numeric_id() {
                Some(their_id) => {
                    self.logger.warn(&format!(
                        "plugin {} asked for {}; this host takes no callbacks",
                        self.name,
                        message.method.as_deref().unwrap_or("?")
                    ));
                    let _ = self.answer_method_not_found(their_id);
                }
                None => {
                    self.ignored += 1;
                    self.logger.debug(&format!(
                        "plugin {} sent a notification {:?}",
                        self.name,
                        message.method.as_deref().unwrap_or("?")
                    ));
                }
            }
            return Accepted::Skip;
        }
        if !message.is_reply() {
            self.ignored += 1;
            self.logger.warn(&format!(
                "plugin {} sent a message that is neither a reply nor a notification",
                self.name
            ));
            return Accepted::Skip;
        }
        match message.numeric_id() {
            Some(other) if other == id => Accepted::Reply(message),
            _ => Accepted::Reject(Error::new(
                Kind::ProtocolError,
                format!(
                    "{}: the reply carries id {:?}, but the call was id {id}; a host that accepted \
                     that would hand one call's answer to another",
                    self.name, message.id
                ),
            )),
        }
    }

    fn answer_method_not_found(&mut self, their_id: u64) -> Result<()> {
        let mut map = serde_json::Map::new();
        map.insert("jsonrpc".to_string(), serde_json::Value::from("2.0"));
        map.insert("id".to_string(), serde_json::Value::from(their_id));
        map.insert(
            "error".to_string(),
            serde_json::json!({
                "code": code::METHOD_NOT_FOUND,
                "message": "this host answers no plugin-initiated requests",
            }),
        );
        let mut line = serde_json::to_vec(&serde_json::Value::Object(map)).map_err(|error| {
            Error::new(
                Kind::ProtocolError,
                format!(
                    "{}: cannot encode a method-not-found reply: {error}",
                    self.name
                ),
            )
        })?;
        line.push(b'\n');
        self.write_frame(&line)
    }

    fn write_frame(&mut self, line: &[u8]) -> Result<()> {
        let stdin = match self.stdin.as_mut() {
            Some(stdin) => stdin,
            None => {
                let reason = "stdin was closed".to_string();
                self.gone = Some(reason.clone());
                return Err(self.exited_error(&reason));
            }
        };
        let result = stdin.write_all(line).and_then(|()| stdin.flush());
        if let Err(error) = result {
            let reason = format!("writing to the child failed: {error}");
            self.gone = Some(reason.clone());
            return Err(self.exited_error(&reason));
        }
        Ok(())
    }

    /// Set the sticky "gone" flag if the child has already exited.
    fn refresh_alive(&mut self) -> Result<()> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let reason = describe_status(&status);
                self.gone = Some(reason.clone());
                Err(self.exited_error(&reason))
            }
            Ok(None) => Ok(()),
            // A `try_wait` that fails is not evidence the child is alive, but it
            // is also not evidence it is dead; the read/write path will find out.
            Err(_) => Ok(()),
        }
    }

    fn timed_out(&mut self, timeout: Duration) -> Error {
        let detail = format!(
            "no reply within {} ms; the child was killed",
            timeout.as_millis()
        );
        self.kill(&detail);
        Error::new(Kind::Timeout, format!("{}: {detail}", self.name))
    }

    /// Kill the child and wait for it, so no zombie is left and no thread is
    /// reading from a pipe nobody will close.
    pub fn kill(&mut self, reason: &str) {
        if self.gone.is_none() {
            self.gone = Some(reason.to_string());
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Whether the child has already exited, without blocking for a timeout.
    pub fn has_exited(&mut self) -> bool {
        if self.gone.is_some() {
            return true;
        }
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// The last lines the child wrote to stderr, newest last.
    pub fn stderr_tail(&self, count: usize) -> Vec<String> {
        let guard = self
            .stderr_log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let start = guard.len().saturating_sub(count);
        guard[start..].to_vec()
    }

    /// Ask the plugin to stop, wait for it, and kill it if it overstays.
    ///
    /// The order matters: `shutdown` lets a plugin flush state, closing stdin
    /// tells a plugin that reads until EOF to exit, and only then does the host
    /// stop being polite. A host that killed first would lose whatever the
    /// plugin was in the middle of writing.
    pub fn close(&mut self) -> Stopped {
        if self.gone.is_none() {
            // A `shutdown` *request* with a reply, not a notification: a plugin
            // that has to flush a file needs a moment and wants to say whether
            // it managed. Both failures here are tolerated — a plugin that is
            // already gone cannot hear either way, and that is not an error to
            // report from `close`.
            let request = Request::new(SHUTDOWN_ID, "shutdown", None);
            if let Ok(line) = request.to_line() {
                if self.write_frame(&line).is_ok() {
                    // One wait, not a loop: the first reply or the first end of
                    // stream settles it either way, and a bounded single wait is
                    // what keeps `close` from blocking on a plugin that answers
                    // nothing at all.
                    let deadline = Instant::now() + SHUTDOWN_REPLY_WAIT;
                    if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                        let _ = self.events.recv_timeout(remaining);
                    }
                }
            }
        }
        drop(self.stdin.take());

        let deadline = Instant::now() + CLOSE_GRACE;
        let mut status: Option<ExitStatus> = None;
        loop {
            match self.child.try_wait() {
                Ok(Some(finished)) => {
                    status = Some(finished);
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        let killed = status.is_none();
        if killed {
            let _ = self.child.kill();
            status = self.child.wait().ok();
            if self.gone.is_none() {
                self.gone = Some(format!("killed after {CLOSE_GRACE:?}"));
            }
        } else if self.gone.is_none() {
            self.gone = status.as_ref().map(describe_status);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(stderr) = self.stderr_thread.take() {
            let _ = stderr.join();
        }
        let clean = !killed && status.as_ref().is_some_and(ExitStatus::success);
        let detail = if killed {
            format!("killed after {CLOSE_GRACE:?} grace period")
        } else {
            status
                .as_ref()
                .map(describe_status)
                .unwrap_or_else(|| "exited, status unknown".to_string())
        };
        Stopped {
            exited: !killed,
            clean,
            code: status.as_ref().and_then(ExitStatus::code),
            detail,
        }
    }

    fn exited_error(&self, reason: &str) -> Error {
        let tail = self.stderr_tail(1);
        let detail = match tail.last() {
            Some(line) => format!("{reason}; its last stderr line was {line:?}"),
            None => format!("{reason}; it wrote nothing to stderr"),
        };
        Error::new(Kind::ChildExited, format!("{}: {detail}", self.name))
    }
}

enum Accepted {
    Reply(Message),
    Skip,
    Reject(Error),
}

impl Drop for Connection {
    fn drop(&mut self) {
        // `close` is idempotent enough for this: joining a thread that already
        // finished returns at once, and killing a dead child is a no-op that
        // still reaps it.
        let _ = self.close();
    }
}

fn describe_status(status: &ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exited with status {code}"),
        None => "terminated by a signal".to_string(),
    }
}

/// Read one JSON object per line, enforcing `max_bytes` without buffering more.
fn read_events<R: Read>(stream: R, max_bytes: usize, sender: Sender<Event>) {
    let mut reader = BufReader::with_capacity(READ_BUFFER, stream);
    let mut line: Vec<u8> = Vec::new();
    // `Some(n)` means the line currently being drained has already exceeded the
    // cap and `n` counts every byte of it seen so far. The count continues
    // across buffers: a 5 MiB line arrives in 32 KiB pieces, and the number the
    // error reports has to be the whole line, not the piece that tripped it.
    let mut overflow: Option<usize> = None;
    loop {
        let (newline_at, consumed) = {
            let buffer = match reader.fill_buf() {
                Ok(buffer) => buffer,
                Err(error) => {
                    let _ = sender.send(Event::Eof(format!("reading stdout failed: {error}")));
                    return;
                }
            };
            if buffer.is_empty() {
                // EOF. A final line without a newline is still a line, but the
                // protocol requires the newline, so it is not delivered: the
                // reader thread is about to report EOF, and the caller needs
                // `child_exited` more than it needs half a message.
                if let Some(bytes) = overflow.take() {
                    let _ = sender.send(Event::TooLarge(bytes));
                }
                let _ = sender.send(Event::Eof("stdout reached end of file".to_string()));
                return;
            }
            match buffer.iter().position(|byte| *byte == b'\n') {
                Some(index) => {
                    let piece = &buffer[..index];
                    if let Some(count) = overflow.as_mut() {
                        *count += piece.len() + 1;
                    } else if line.len() + piece.len() > max_bytes {
                        overflow = Some(line.len() + piece.len() + 1);
                        line.clear();
                    } else {
                        line.extend_from_slice(piece);
                    }
                    (true, index + 1)
                }
                None => {
                    if let Some(count) = overflow.as_mut() {
                        *count += buffer.len();
                    } else if line.len() + buffer.len() > max_bytes {
                        overflow = Some(line.len() + buffer.len());
                        line.clear();
                    } else {
                        line.extend_from_slice(buffer);
                    }
                    (false, buffer.len())
                }
            }
        };
        reader.consume(consumed);
        if !newline_at {
            continue;
        }
        if let Some(bytes) = overflow.take() {
            if sender.send(Event::TooLarge(bytes)).is_err() {
                return;
            }
            line.clear();
            continue;
        }
        let mut payload = std::mem::take(&mut line);
        while matches!(payload.last(), Some(b'\r')) {
            payload.pop();
        }
        if payload.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if sender.send(Event::Line(payload)).is_err() {
            return;
        }
    }
}

fn read_stderr<R: Read>(
    stream: R,
    log: Arc<Mutex<Vec<String>>>,
    logger: Arc<dyn Logger>,
    name: String,
) {
    let mut reader = BufReader::with_capacity(READ_BUFFER, stream);
    let mut line: Vec<u8> = Vec::new();
    let mut truncated = false;
    loop {
        let (newline_at, consumed) = {
            let buffer = match reader.fill_buf() {
                Ok(buffer) => buffer,
                Err(_) => return,
            };
            if buffer.is_empty() {
                if !line.is_empty() {
                    record_stderr(&name, &line, truncated, &log, &*logger);
                }
                return;
            }
            match buffer.iter().position(|byte| *byte == b'\n') {
                Some(index) => {
                    if line.len() < STDERR_LINE_CAP {
                        let room = STDERR_LINE_CAP - line.len();
                        if index > room {
                            line.extend_from_slice(&buffer[..room]);
                            truncated = true;
                        } else {
                            line.extend_from_slice(&buffer[..index]);
                        }
                    } else {
                        truncated = true;
                    }
                    (true, index + 1)
                }
                None => {
                    if line.len() < STDERR_LINE_CAP {
                        let room = STDERR_LINE_CAP - line.len();
                        let take = buffer.len().min(room);
                        line.extend_from_slice(&buffer[..take]);
                        if take < buffer.len() {
                            truncated = true;
                        }
                    }
                    (false, buffer.len())
                }
            }
        };
        reader.consume(consumed);
        if newline_at {
            if !line.is_empty() {
                record_stderr(&name, &line, truncated, &log, &*logger);
            }
            line.clear();
            truncated = false;
        }
    }
}

fn record_stderr(
    name: &str,
    line: &[u8],
    truncated: bool,
    log: &Arc<Mutex<Vec<String>>>,
    logger: &dyn Logger,
) {
    let mut text = String::from_utf8_lossy(line).trim_end().to_string();
    if truncated {
        text.push_str(" [truncated]");
    }
    if text.is_empty() {
        return;
    }
    {
        let mut guard = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.push(text.clone());
        // Bounded: a plugin that logs in a loop must not grow the host's
        // memory. The tail is what a diagnostic needs; the head is not.
        if guard.len() > STDERR_TAIL {
            guard.remove(0);
        }
    }
    logger.debug(&format!("plugin {name} stderr: {text}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain_events<R: Read>(stream: R, max_bytes: usize) -> Vec<Event> {
        let (sender, receiver) = mpsc::channel();
        read_events(stream, max_bytes, sender);
        receiver.try_iter().collect()
    }

    #[test]
    fn lines_are_split_on_newlines_and_carriage_returns_are_dropped() {
        let events = drain_events(&b"{\"a\":1}\r\n\n  \n{\"b\":2}\n"[..], 1024);
        let lines: Vec<&Vec<u8>> = events
            .iter()
            .filter_map(|event| match event {
                Event::Line(line) => Some(line),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"{\"a\":1}");
        assert_eq!(lines[1], b"{\"b\":2}");
        assert!(matches!(events.last(), Some(Event::Eof(_))));
    }

    #[test]
    fn a_line_over_the_cap_is_reported_and_the_stream_stays_in_frame() {
        // 8 bytes of junk, then a newline, then a short line: the cap is 16, so
        // the first line fits and the second (24 bytes) does not. The third
        // line must arrive whole — that is what "drained, not buffered" means.
        let input = b"short\nxxxxxxxxxxxxxxxxxxxxxxxx\nok\n";
        let events = drain_events(&input[..], 16);
        let mut saw_too_large = false;
        let mut lines: Vec<Vec<u8>> = Vec::new();
        for event in &events {
            match event {
                Event::TooLarge(bytes) => {
                    saw_too_large = true;
                    assert_eq!(*bytes, 25, "25 bytes including the newline");
                }
                Event::Line(line) => lines.push(line.clone()),
                Event::Eof(_) => {}
            }
        }
        assert!(saw_too_large, "the over-cap line was not reported");
        assert_eq!(lines, vec![b"short".to_vec(), b"ok".to_vec()]);
    }

    #[test]
    fn a_cap_is_counted_in_bytes_across_buffer_boundaries() {
        // 40 KB with no newline exercises the "no newline in this buffer" path
        // several times; the cap is 1024, so it must be reported, not stored.
        let big = vec![b'a'; 40 << 10];
        let events = drain_events(&big[..], 1024);
        let too_large = events.iter().find_map(|event| match event {
            Event::TooLarge(bytes) => Some(*bytes),
            _ => None,
        });
        assert_eq!(too_large, Some(40 << 10));
    }
}
