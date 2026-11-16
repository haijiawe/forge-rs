//! stdio transport: one JSON-RPC document per line on the child's pipes.
//!
//! Three threads own the streams: a line-framed reader dispatches by request
//! id, a stderr pump logs (never parses) the error stream, and a reaper records
//! why the child exited. stderr can never corrupt the protocol stream.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::jsonrpc::{self, WireMessage};
use crate::process::{self, SpawnSpec};
use crate::{Conn, Error, Logger, Server};

pub(crate) const MAX_MESSAGE_BYTES: usize = 8 << 20;
const MAX_STDERR_LINE: usize = 4 << 10;
const READ_BUFFER: usize = 32 << 10;
const CLOSE_GRACE: Duration = Duration::from_millis(250);
const CLOSE_WAIT: Duration = Duration::from_secs(2);

type Pending = HashMap<u64, Sender<Result<WireMessage, Error>>>;

fn describe_exit(status: &Result<ExitStatus, io::Error>) -> String {
    match status {
        Ok(status) => match status.code() {
            Some(code) => format!("child exited with status {code}"),
            None => "child terminated by signal".to_string(),
        },
        Err(e) => format!("wait for child: {e}"),
    }
}

pub(crate) fn dial_stdio(
    server: &Server,
    logger: &Arc<dyn Logger>,
) -> Result<Arc<dyn Conn>, Error> {
    if server.command.is_empty() {
        return Err(Error::Config(format!(
            "server {:?}: stdio transport needs a command",
            server.label()
        )));
    }
    let spec = SpawnSpec {
        path: OsString::from(&server.command[0]),
        args: server.command[1..].iter().map(OsString::from).collect(),
        env: server.env.clone(),
    };
    let spawned = process::spawn(&spec, server)?;
    Ok(StdioConn::new(spawned, server.label(), logger.clone()))
}

struct StdioConn {
    name: String,
    logger: Arc<dyn Logger>,
    pid: u32,
    tree: Mutex<Option<process::TreeHandle>>,
    stdin: Mutex<Option<ChildStdin>>,
    pending: Arc<Mutex<Pending>>,
    gone: Arc<Mutex<Option<String>>>,
    exit_reason: Arc<Mutex<String>>,
    exit_rx: Mutex<Receiver<()>>,
    thread_done: Mutex<Vec<Receiver<()>>>,
    closed: AtomicBool,
    write_lock: Mutex<()>,
}

impl StdioConn {
    fn new(spawned: process::Spawned, name: String, logger: Arc<dyn Logger>) -> Arc<Self> {
        let process::Spawned { mut child, tree } = spawned;
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(HashMap::new()));
        let gone: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let exit_reason: Arc<Mutex<String>> =
            Arc::new(Mutex::new("child stdout closed".to_string()));
        let (exit_signal, exit_rx) = mpsc::sync_channel::<()>(1);

        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
        std::thread::Builder::new()
            .name("mcp-reaper".to_string())
            .spawn({
                let reason = exit_reason.clone();
                let signal = exit_signal;
                move || {
                    let status = child.wait();
                    *reason.lock().unwrap_or_else(|e| e.into_inner()) = describe_exit(&status);
                    let _ = signal.try_send(());
                    let _ = done_tx.try_send(());
                }
            })
            .expect("spawn mcp reaper thread");

        let conn = StdioConn {
            name,
            logger,
            pid,
            tree: Mutex::new(Some(tree)),
            stdin: Mutex::new(stdin),
            pending,
            gone,
            exit_reason,
            exit_rx: Mutex::new(exit_rx),
            thread_done: Mutex::new(vec![done_rx]),
            closed: AtomicBool::new(false),
            write_lock: Mutex::new(()),
        };
        let arc = Arc::new(conn);

        if let Some(stdout) = stdout {
            let conn = arc.clone();
            let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
            arc.thread_done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(done_rx);
            std::thread::Builder::new()
                .name("mcp-stdout".to_string())
                .spawn(move || {
                    read_loop(stdout, &conn);
                    let _ = done_tx.try_send(());
                })
                .expect("spawn mcp stdout thread");
        }
        if let Some(stderr) = stderr {
            let logger = arc.logger.clone();
            let name = arc.name.clone();
            let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
            arc.thread_done
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(done_rx);
            std::thread::Builder::new()
                .name("mcp-stderr".to_string())
                .spawn(move || {
                    stderr_loop(stderr, &name, &*logger);
                    let _ = done_tx.try_send(());
                })
                .expect("spawn mcp stderr thread");
        }
        arc
    }

    fn fail(&self, reason: &str) {
        if let Ok(mut gone) = self.gone.lock() {
            if gone.is_none() {
                *gone = Some(reason.to_string());
            }
        }
        let waiters: Vec<_> = self
            .pending
            .lock()
            .map(|mut p| p.drain().map(|(_, tx)| tx).collect())
            .unwrap_or_default();
        for tx in waiters {
            let _ = tx.send(Err(Error::gone(reason.to_string())));
        }
    }

    fn gone_reason(&self) -> Option<String> {
        self.gone.lock().ok().and_then(|g| g.clone()).or_else(|| {
            if self.closed.load(Ordering::Acquire) {
                Some("client closed".to_string())
            } else {
                None
            }
        })
    }

    fn write_frame_inner(&self, payload: &[u8]) -> Result<(), Error> {
        if let Some(reason) = self.gone_reason() {
            return Err(Error::gone(reason));
        }
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| Error::gone("write lock poisoned"))?;
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| Error::gone("stdin lock poisoned"))?;
        let Some(stream) = stdin.as_mut() else {
            let reason = "stdin is closed".to_string();
            self.fail(&reason);
            return Err(Error::gone(reason));
        };
        let result = stream
            .write_all(payload)
            .and_then(|()| stream.write_all(b"\n"))
            .and_then(|()| stream.flush());
        if let Err(e) = result {
            let reason = format!("write to stdin: {e}");
            self.fail(&reason);
            return Err(Error::gone(reason));
        }
        Ok(())
    }
}

fn read_loop(stdout: ChildStdout, conn: &StdioConn) {
    let mut reader = BufReader::with_capacity(READ_BUFFER, stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = match reader.read_until(b'\n', &mut line) {
            Ok(0) => {
                let reason = conn
                    .exit_reason
                    .lock()
                    .map(|r| r.clone())
                    .unwrap_or_else(|e| e.into_inner().clone());
                conn.fail(&reason);
                return;
            }
            Ok(n) => n,
            Err(e) => {
                conn.fail(&format!("read stdout: {e}"));
                return;
            }
        };
        if read > MAX_MESSAGE_BYTES + 2 || line.len() > MAX_MESSAGE_BYTES + 2 {
            conn.fail(&format!("stdout line exceeds {MAX_MESSAGE_BYTES} bytes"));
            return;
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let message = match jsonrpc::parse_message(&line) {
            Ok(message) => message,
            Err(e) => {
                conn.fail(&format!("malformed server message: {e}"));
                return;
            }
        };
        if let Some(method) = message.method.clone() {
            match message.id.clone() {
                Some(id) if !id.is_null() => {
                    let reply = match jsonrpc::encode_method_not_found(&id) {
                        Ok(reply) => reply,
                        Err(e) => {
                            conn.fail(&format!("encode server-request reply: {e}"));
                            return;
                        }
                    };
                    if let Err(e) = conn.write_frame_inner(&reply) {
                        conn.logger.warn(&format!(
                            "mcp: server {:?}: server request reply failed: {}",
                            conn.name, e
                        ));
                        return;
                    }
                }
                _ => conn.logger.debug(&format!(
                    "mcp: server {:?}: notification {method}",
                    conn.name
                )),
            }
            continue;
        }
        let Some(id) = jsonrpc::parse_id(message.id.as_ref()) else {
            conn.logger.warn(&format!(
                "mcp: server {:?}: ignoring response without a usable id",
                conn.name
            ));
            continue;
        };
        let waiter = conn
            .pending
            .lock()
            .map(|mut p| p.remove(&id))
            .unwrap_or(None);
        if let Some(tx) = waiter {
            let _ = tx.send(Ok(message));
        }
    }
}

fn stderr_loop(stderr: ChildStderr, name: &str, logger: &dyn Logger) {
    let mut stderr = stderr;
    let mut buffer = vec![0u8; READ_BUFFER];
    let mut partial: Vec<u8> = Vec::new();
    loop {
        let n = match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        for byte in &buffer[..n] {
            if *byte == b'\n' {
                log_stderr_line(name, &partial, logger);
                partial.clear();
            } else if partial.len() < MAX_STDERR_LINE {
                partial.push(*byte);
            }
        }
        if partial.len() >= MAX_STDERR_LINE {
            log_stderr_line(name, &partial, logger);
            partial.clear();
        }
    }
    if !partial.is_empty() {
        log_stderr_line(name, &partial, logger);
    }
}

fn log_stderr_line(name: &str, line: &[u8], logger: &dyn Logger) {
    if line.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(line);
    logger.debug(&format!("mcp: server {name:?} stderr: {text}"));
}

impl Conn for StdioConn {
    fn round_trip(
        &self,
        deadline: Instant,
        _method: &str,
        payload: &[u8],
        id: u64,
    ) -> Result<WireMessage, Error> {
        if let Some(reason) = self.gone_reason() {
            return Err(Error::gone(reason));
        }
        let (tx, rx) = mpsc::channel::<Result<WireMessage, Error>>();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| Error::gone("pending lock poisoned"))?;
            pending.insert(id, tx);
        }
        if let Err(e) = self.write_frame_inner(payload) {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&id);
            }
            return Err(e);
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        let received = match rx.recv_timeout(remaining) {
            Ok(received) => received,
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                return Err(Error::timeout(format!(
                    "server {:?}: request {id} timed out after {}ms",
                    self.name,
                    remaining.as_millis()
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::gone(format!(
                    "server {:?}: response channel closed",
                    self.name
                )))
            }
        };
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
        received
    }

    fn notify(&self, _deadline: Instant, _method: &str, payload: &[u8]) -> Result<(), Error> {
        self.write_frame_inner(payload)
    }

    fn set_protocol(&self, _version: &str) {}

    fn close(&self) -> Result<(), Error> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.fail("client closed");
        if let Ok(mut stdin) = self.stdin.lock() {
            *stdin = None;
        }
        let exited = self
            .exit_rx
            .lock()
            .map(|rx| rx.recv_timeout(CLOSE_GRACE).is_ok())
            .unwrap_or(false);
        if !exited {
            let reason = format!("tree killed after {CLOSE_GRACE:?} grace period");
            self.fail(&reason);
            let terminated = self
                .tree
                .lock()
                .ok()
                .and_then(|mut t| t.take())
                .map(|mut tree| {
                    let result = process::terminate_tree(self.pid, &tree);
                    process::drop_tree(&mut tree);
                    result
                })
                .unwrap_or(Ok(()));
            if let Err(e) = terminated {
                self.logger
                    .warn(&format!("mcp: server {:?}: tree kill: {e}", self.name));
            }
        }
        let deadline = Instant::now() + CLOSE_WAIT;
        let done = self.thread_done.lock().unwrap_or_else(|e| e.into_inner());
        for rx in done.iter() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            let _ = rx.recv_timeout(remaining);
        }
        Ok(())
    }
}
