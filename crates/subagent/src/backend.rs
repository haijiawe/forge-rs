//! A second place a child can run.
//!
//! Until now every child ran inside this process: `Runner` opened a child log,
//! built a set of tools and drove a fresh `hearth_agent::Agent`. That is the
//! simple case and it stays the default. This module names the *seam* that lets
//! a child run somewhere else — another process on this machine, started by us,
//! speaking one JSON object per line over stdio.
//!
//! The rule that keeps this honest: a backend is only worth the abstraction once
//! there is a second real implementation. The one here spawns *our own binary*
//! in worker mode (`hearth subagent worker`), which is a genuine second process
//! with its own memory, its own crash domain and its own environment. It is not
//! a DSH or DS server; nothing here talks to one.
//!
//! The wire is deliberately boring: one request line in, one response line out,
//! both `BackendRequest`/`BackendOutcome` below. A worker that says anything
//! other than one JSON line is a bug, and we fail loudly rather than guess.

use crate::child::ChildRun;
use crate::descriptor::Mode;
use hearth_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Bumped only when the request or response shape changes incompatibly. A
/// worker that speaks a different version refuses; we never silently coerce.
pub const BACKEND_PROTOCOL_VERSION: u32 = 1;

/// How long we wait for a spawned worker to answer one request before giving up.
pub const DEFAULT_BACKEND_TIMEOUT: Duration = Duration::from_secs(600);

/// One request to a backend. `op` is `"run"` (a fresh child) or `"resume"`
/// (append a turn to an existing child; `child` names it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendRequest {
    pub v: u32,
    pub op: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl BackendRequest {
    /// A request to start a fresh child.
    pub fn run(prompt: impl Into<String>, mode: Mode, workspace: impl Into<String>) -> Self {
        BackendRequest {
            v: BACKEND_PROTOCOL_VERSION,
            op: "run".to_string(),
            prompt: prompt.into(),
            system: None,
            model: None,
            workspace: Some(workspace.into()),
            schema: None,
            child: None,
            mode: Some(mode.as_str().to_string()),
        }
    }

    /// A request to append a turn to an existing child.
    pub fn resume(child: impl Into<String>, prompt: impl Into<String>) -> Self {
        BackendRequest {
            v: BACKEND_PROTOCOL_VERSION,
            op: "resume".to_string(),
            prompt: prompt.into(),
            system: None,
            model: None,
            workspace: None,
            schema: None,
            child: Some(child.into()),
            mode: None,
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_schema(mut self, schema: Value) -> Self {
        self.schema = Some(schema);
        self
    }

    /// The one JSON line we put on the worker's stdin.
    pub fn to_line(&self) -> Result<String> {
        let mut text = serde_json::to_string(self).map_err(|e| {
            Error::new(
                ErrorKind::Json,
                format!("subagent backend: encode request: {e}"),
            )
        })?;
        text.push('\n');
        Ok(text)
    }
}

/// What a worker answers. `ok:false` carries `error`/`code` and no result; a
/// worker that reports a failed child is *data*, not a transport error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendOutcome {
    pub v: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    #[serde(default)]
    pub turns: usize,
    #[serde(default)]
    pub steps: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl BackendOutcome {
    pub fn success(
        session_id: impl Into<String>,
        text: impl Into<String>,
        turns: usize,
        steps: usize,
    ) -> Self {
        BackendOutcome {
            v: BACKEND_PROTOCOL_VERSION,
            ok: true,
            session_id: Some(session_id.into()),
            text: Some(text.into()),
            structured: None,
            turns,
            steps,
            error: None,
            code: None,
        }
    }

    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        BackendOutcome {
            v: BACKEND_PROTOCOL_VERSION,
            ok: false,
            session_id: None,
            text: None,
            structured: None,
            turns: 0,
            steps: 0,
            error: Some(message.into()),
            code: Some(code.into()),
        }
    }

    pub fn with_structured(mut self, value: Value) -> Self {
        self.structured = Some(value);
        self
    }

    /// Parse one response line. A version we do not speak is refused, not coerced.
    pub fn parse_line(line: &str) -> Result<BackendOutcome> {
        let outcome: BackendOutcome = serde_json::from_str(line).map_err(|e| {
            Error::new(
                ErrorKind::Json,
                format!("subagent backend: decode response: {e}"),
            )
        })?;
        if outcome.v != BACKEND_PROTOCOL_VERSION {
            return Err(Error::new(
                ErrorKind::UnsupportedVersion,
                format!(
                    "subagent backend: worker speaks protocol {} but this build speaks {}",
                    outcome.v, BACKEND_PROTOCOL_VERSION
                ),
            ));
        }
        Ok(outcome)
    }

    /// Turn a failed outcome into the error the caller should surface.
    pub fn into_result(self) -> Result<ChildRun> {
        if self.ok {
            Ok(ChildRun {
                id: self.session_id.unwrap_or_default(),
                mode: Mode::OneShot,
                text: self.text.unwrap_or_default(),
                structured: self.structured,
                turns: self.turns,
                steps: self.steps,
                ..ChildRun::default()
            })
        } else {
            Err(Error::new(
                ErrorKind::Refused,
                self.error.unwrap_or_else(|| {
                    "subagent backend: the worker reported a failure".to_string()
                }),
            ))
        }
    }
}

/// One place a child can run.
pub trait Backend: Send + Sync {
    /// A short name for logs and tests.
    fn name(&self) -> &str;
    /// Send one request and wait for one outcome.
    fn call(&self, request: &BackendRequest) -> Result<BackendOutcome>;
}

/// A backend that spawns `program args...`, writes one request line to its
/// stdin, and reads one response line from its stdout. stderr is inherited so a
/// worker's diagnostics reach the operator instead of vanishing.
pub struct ProcessBackend {
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
}

impl ProcessBackend {
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        ProcessBackend {
            program: program.into(),
            args,
            timeout: DEFAULT_BACKEND_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl Backend for ProcessBackend {
    fn name(&self) -> &str {
        "process"
    }

    fn call(&self, request: &BackendRequest) -> Result<BackendOutcome> {
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                Error::new(
                    ErrorKind::Io,
                    format!(
                        "subagent backend: cannot spawn {}: {e}",
                        self.program.display()
                    ),
                )
            })?;

        let line = request.to_line()?;
        {
            let stdin = child.stdin.as_mut().ok_or_else(|| {
                Error::new(ErrorKind::Io, "subagent backend: worker has no stdin")
            })?;
            stdin.write_all(line.as_bytes()).map_err(|e| {
                Error::new(
                    ErrorKind::Io,
                    format!("subagent backend: write request: {e}"),
                )
            })?;
        }
        // Close stdin so a worker that reads to EOF is not left waiting.
        drop(child.stdin.take());

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Io, "subagent backend: worker has no stdout"))?;

        // Read on a thread so the timeout does not depend on the worker exiting.
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = String::new();
            let read = reader.read_line(&mut buf);
            let _ = tx.send((read, buf));
        });

        let outcome = match rx.recv_timeout(self.timeout) {
            Ok((read, buf)) => {
                let n = read.map_err(|e| {
                    Error::new(
                        ErrorKind::Io,
                        format!("subagent backend: read response: {e}"),
                    )
                })?;
                if n == 0 {
                    return Err(Error::new(
                        ErrorKind::Io,
                        "subagent backend: the worker closed its output without a response line",
                    ));
                }
                BackendOutcome::parse_line(buf.trim_end())
            }
            Err(_) => {
                let _ = child.kill();
                Err(Error::new(
                    ErrorKind::Refused,
                    format!(
                        "subagent backend: the worker did not answer within {}s",
                        self.timeout.as_secs()
                    ),
                ))
            }
        };

        // Reap the worker regardless; its exit status is not the verdict (the
        // response line is), but a crash mid-line already surfaced above.
        let _ = child.wait();
        let _ = reader.join();
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_through_one_line() {
        let req = BackendRequest::run("do it", Mode::Continuable, "/w")
            .with_model("m")
            .with_system("sys")
            .with_schema(serde_json::json!({"type": "object"}));
        let line = req.to_line().unwrap();
        assert!(line.ends_with('\n'));
        let back: BackendRequest = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(back, req);
        assert_eq!(back.op, "run");
        assert_eq!(back.mode.as_deref(), Some("continuable"));
    }

    #[test]
    fn a_resume_request_names_the_child() {
        let req = BackendRequest::resume("abc-123", "more");
        assert_eq!(req.op, "resume");
        assert_eq!(req.child.as_deref(), Some("abc-123"));
        assert!(req.workspace.is_none());
    }

    #[test]
    fn a_success_outcome_parses_into_a_child_run() {
        let line = r#"{"v":1,"ok":true,"sessionId":"s1","text":"hi","structured":{"a":1},"turns":2,"steps":3}"#;
        let out = BackendOutcome::parse_line(line).unwrap();
        assert!(out.ok);
        let run = out.into_result().unwrap();
        assert_eq!(run.id, "s1");
        assert_eq!(run.text, "hi");
        assert_eq!(run.turns, 2);
        assert_eq!(run.steps, 3);
        assert!(run.structured.is_some());
    }

    #[test]
    fn a_failure_outcome_becomes_an_error_not_a_panic() {
        let line = r#"{"v":1,"ok":false,"error":"boom","code":"bad"}"#;
        let out = BackendOutcome::parse_line(line).unwrap();
        let err = out.into_result().unwrap_err();
        assert!(err.message().contains("boom"));
    }

    #[test]
    fn a_foreign_protocol_version_is_refused() {
        let line = r#"{"v":2,"ok":true,"text":"hi"}"#;
        let err = BackendOutcome::parse_line(line).unwrap_err();
        assert!(err.message().contains("protocol 2"));
    }

    #[cfg(windows)]
    fn canned(program: &mut Command, body: &str) {
        // Write the body to a temp file and `type` it: ignores stdin, prints one
        // line, exits — a minimal stand-in for a worker's response.
        let mut path = std::env::temp_dir();
        path.push(format!("hearth-backend-test-{}.json", std::process::id()));
        std::fs::write(&path, body).unwrap();
        program.args(["/c", "type"]).arg(&path);
    }

    #[cfg(unix)]
    fn canned(program: &mut Command, body: &str) {
        use std::io::Write as _;
        let mut path = std::env::temp_dir();
        path.push(format!("hearth-backend-test-{}.json", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        drop(f);
        program.args(["-c", &format!("cat {}", path.display())]);
    }

    #[cfg(windows)]
    fn shell() -> &'static str {
        "cmd"
    }
    #[cfg(unix)]
    fn shell() -> &'static str {
        "sh"
    }

    #[test]
    fn process_backend_spawns_reads_and_parses_a_response() {
        let body = r#"{"v":1,"ok":true,"sessionId":"spawned","text":"from a process","turns":1,"steps":1}"#;
        // Build a ProcessBackend whose program is the shell and whose args `type`
        // a canned file. We cannot pass a closure, so we inline the shell args.
        let mut cmd = Command::new(shell());
        canned(&mut cmd, body);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let backend = ProcessBackend::new(shell(), args);
        let out = backend
            .call(&BackendRequest::run("hi", Mode::OneShot, "/w"))
            .unwrap();
        assert!(out.ok);
        assert_eq!(out.session_id.as_deref(), Some("spawned"));
    }
}
