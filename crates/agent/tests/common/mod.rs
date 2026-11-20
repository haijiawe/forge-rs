//! Shared test helpers: a scripted Provider fake, a fake ToolRunner, and a
//! deterministic event-log harness. No network, no real model, no async.
//!
//! Each integration-test binary uses only a subset of these helpers, so unused
//! items are expected.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hearth_core::{Error, ErrorKind, Event, Result};
use hearth_llm::{Delta, Provider, Request, Stream, ToolCall, ToolSpec};
use hearth_session::{Options as StoreOptions, Store};
use serde_json::json;

use hearth_agent::{cancelled_error, provider_error, Cancel, ToolResult, ToolRunner};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique on-disk log path for a test (no tempfile dependency needed).
pub fn temp_log(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "hearth-agent-{}-{}-{}.jsonl",
        name,
        std::process::id(),
        n
    ));
    p
}

pub fn open_store(path: &PathBuf) -> Store {
    Store::open(
        path,
        StoreOptions {
            no_fsync: true,
            ..Default::default()
        },
    )
    .expect("open store")
}

/// One scripted provider response: either a successful stream of deltas or an
/// immediate failure to open a stream (carrying retry metadata).
pub enum Response {
    Deltas(Vec<Delta>),
    StartErr(Error),
    /// A stream that triggers cancellation from inside its first `recv`, then
    /// keeps returning deltas (which the loop must ignore once cancelled).
    CancelOnFirstRecv(Cancel),
}

/// A Provider driven by a queue of scripted responses. Every stream is closed
/// exactly once by the agent; the fake counts closes to prove it.
pub struct ScriptedProvider {
    responses: Mutex<std::collections::VecDeque<Response>>,
    requests: Mutex<Vec<Request>>,
    closes: Arc<Mutex<usize>>,
}

impl ScriptedProvider {
    pub fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            closes: Arc::new(Mutex::new(0)),
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests lock").clone()
    }

    pub fn close_count(&self) -> usize {
        *self.closes.lock().expect("closes lock")
    }
}

impl Provider for ScriptedProvider {
    fn stream(&self, req: &Request) -> Result<Box<dyn Stream>> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(req.clone());
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .ok_or_else(|| Error::new(ErrorKind::Invalid, "no more scripted responses"))?;
        match response {
            Response::Deltas(deltas) => Ok(Box::new(ScriptedStream {
                deltas: std::collections::VecDeque::from(deltas),
                closes: Arc::clone(&self.closes),
                closed: false,
            })),
            Response::StartErr(e) => Err(e),
            Response::CancelOnFirstRecv(cancel) => Ok(Box::new(CancellingStream {
                cancel,
                first: true,
                closes: Arc::clone(&self.closes),
                closed: false,
            })),
        }
    }
}

struct ScriptedStream {
    deltas: std::collections::VecDeque<Delta>,
    closes: Arc<Mutex<usize>>,
    closed: bool,
}

/// A stream that cancels the agent from inside `recv` on its first read,
/// simulating a user pressing Ctrl+C mid-stream. After cancellation the loop
/// stops reading, so later "deltas" are never delivered.
struct CancellingStream {
    cancel: Cancel,
    first: bool,
    closes: Arc<Mutex<usize>>,
    closed: bool,
}

impl Stream for CancellingStream {
    fn recv(&mut self) -> Result<Option<Delta>> {
        if self.first {
            self.first = false;
            self.cancel.cancel();
            // A cancelled stream surfaces the cancellation as an error; the
            // loop recognises it via `is_cancelled` and delivers no text.
            Err(cancelled_error())
        } else {
            Ok(Some(Delta {
                text: "also never".into(),
                ..Default::default()
            }))
        }
    }
    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            *self.closes.lock().expect("closes lock") += 1;
        }
    }
}

impl Stream for ScriptedStream {
    fn recv(&mut self) -> Result<Option<Delta>> {
        Ok(self.deltas.pop_front())
    }
    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            *self.closes.lock().expect("closes lock") += 1;
        }
    }
}

/// A fake tool runner that records calls and returns a fixed output.
pub struct EchoTool {
    pub calls: Mutex<Vec<ToolCall>>,
}

impl EchoTool {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("calls lock").len()
    }
}

impl ToolRunner for EchoTool {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "echo".into(),
            description: "Echo the input".into(),
            schema: json!({"type": "object"}),
        }]
    }
    fn run(&self, call: ToolCall) -> Result<ToolResult> {
        self.calls.lock().expect("calls lock").push(call);
        Ok(ToolResult {
            output: "echoed".into(),
            ..Default::default()
        })
    }
}

/// A retryable provider failure with no delay (keeps tests fast).
pub fn retryable_error(message: &str, code: &str) -> Error {
    provider_error(message, code, true, Duration::ZERO)
}

/// Strip `ts`/`id` (nondeterministic) and render an event canonically.
pub fn canonical_stable(ev: &Event) -> String {
    let mut v = serde_json::to_value(ev).expect("serialize event");
    if let Some(obj) = v.as_object_mut() {
        obj.remove("ts");
        obj.remove("id");
    }
    hearth_core::canonical(&v).expect("canonical event")
}

/// Read all events from a store.
pub fn read_all(store: &Store) -> Vec<Event> {
    let mut events = Vec::new();
    store
        .read_from(0, |e| {
            events.push(e.clone());
            Ok(())
        })
        .expect("read log");
    events
}
