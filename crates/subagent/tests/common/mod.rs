//! Shared test helpers: scripted provider, gates, tools, and log readers.
//!
//! No network, no async runtime, no extra dependencies. Each integration-test
//! binary uses only a subset, so unused items are expected.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use hearth_core::{Error, Event};
use hearth_llm::{Delta, Provider, Request, Stream, ToolCallDelta, ToolSpec};
use hearth_session::{Options as StoreOptions, Store};
use hearth_tool::{ApprovalRequest, Approver, Call, Decision, Policy};
use serde_json::{json, Value};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique directory that is removed when the guard drops.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "hearth-subagent-{}-{}-{}-{}",
            name,
            std::process::id(),
            n,
            hearth_core::new_id().unwrap_or_else(|_| "noid".into())
        ));
        if let Err(e) = std::fs::create_dir_all(&path) {
            panic!("create temp dir {}: {e}", path.display());
        }
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sessions(&self) -> PathBuf {
        self.path.join("sessions")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A manual gate: `wait` blocks until another thread calls `open`.
#[derive(Clone, Default)]
pub struct Gate {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl Gate {
    pub fn open(&self) {
        if let Ok(mut done) = self.inner.0.lock() {
            *done = true;
        }
        self.inner.1.notify_all();
    }

    fn wait(&self) {
        if let Ok(done) = self.inner.0.lock() {
            drop(self.inner.1.wait_while(done, |d| !*d));
        }
    }
}

/// One scripted provider response.
pub enum Response {
    Deltas(Vec<Delta>),
    StartErr(Error),
    /// Blocks in `recv` until the gate opens, then yields the given deltas.
    BlockThen(Gate, Vec<Delta>),
}

/// A provider driven by a queue of scripted responses.
pub struct ScriptedProvider {
    responses: Mutex<VecDeque<Response>>,
    requests: Mutex<Vec<Request>>,
    closes: Arc<AtomicUsize>,
    reached: Arc<AtomicUsize>,
}

impl ScriptedProvider {
    pub fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            closes: Arc::new(AtomicUsize::new(0)),
            reached: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests lock").clone()
    }

    pub fn close_count(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }

    pub fn reached(&self) -> usize {
        self.reached.load(Ordering::SeqCst)
    }

    fn observer(&self) -> StreamObserver {
        StreamObserver {
            closes: Arc::clone(&self.closes),
            closed: false,
        }
    }
}

impl Provider for ScriptedProvider {
    fn stream(&self, req: &Request) -> hearth_core::Result<Box<dyn Stream>> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(req.clone());
        self.reached.fetch_add(1, Ordering::SeqCst);
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .ok_or_else(|| {
                hearth_core::Error::new(
                    hearth_core::ErrorKind::Invalid,
                    "no more scripted responses",
                )
            })?;
        match response {
            Response::Deltas(deltas) => Ok(Box::new(ScriptedStream {
                deltas: VecDeque::from(deltas),
                observer: self.observer(),
            })),
            Response::StartErr(e) => Err(e),
            Response::BlockThen(gate, deltas) => Ok(Box::new(BlockingStream {
                gate,
                deltas: VecDeque::from(deltas),
                observer: self.observer(),
            })),
        }
    }
}

struct StreamObserver {
    closes: Arc<AtomicUsize>,
    closed: bool,
}

impl StreamObserver {
    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }
}

struct ScriptedStream {
    deltas: VecDeque<Delta>,
    observer: StreamObserver,
}

impl Stream for ScriptedStream {
    fn recv(&mut self) -> hearth_core::Result<Option<Delta>> {
        Ok(self.deltas.pop_front())
    }

    fn close(&mut self) {
        self.observer.close();
    }
}

struct BlockingStream {
    gate: Gate,
    deltas: VecDeque<Delta>,
    observer: StreamObserver,
}

impl Stream for BlockingStream {
    fn recv(&mut self) -> hearth_core::Result<Option<Delta>> {
        self.gate.wait();
        Ok(self.deltas.pop_front())
    }

    fn close(&mut self) {
        self.observer.close();
    }
}

/// A parent tool runner with configurable specs and recorded calls.
pub struct RecordingTools {
    specs: Vec<ToolSpec>,
    calls: Mutex<Vec<hearth_llm::ToolCall>>,
    output: String,
}

impl RecordingTools {
    pub fn new(specs: Vec<ToolSpec>) -> Self {
        Self {
            specs,
            calls: Mutex::new(Vec::new()),
            output: "echo ok".into(),
        }
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = output.into();
        self
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("calls lock").len()
    }

    pub fn calls(&self) -> Vec<hearth_llm::ToolCall> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl hearth_agent::ToolRunner for RecordingTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn run(&self, call: hearth_llm::ToolCall) -> hearth_core::Result<hearth_agent::ToolResult> {
        self.calls.lock().expect("calls lock").push(call);
        Ok(hearth_agent::ToolResult {
            output: self.output.clone(),
            ..Default::default()
        })
    }
}

pub fn tool_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: format!("{name} tool"),
        schema: json!({"type": "object", "properties": {}}),
    }
}

/// A policy that asks for approval on `danger` and allows everything else.
#[derive(Clone, Default)]
pub struct AskDanger;

impl Policy for AskDanger {
    fn decide(&self, call: &Call) -> (Decision, String) {
        if call.name == "danger" {
            (Decision::Ask, "danger needs approval".into())
        } else {
            (Decision::Allow, "allowed".into())
        }
    }
}

#[derive(Default)]
pub struct CountingApprover {
    count: AtomicUsize,
}

impl CountingApprover {
    pub fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Approver for CountingApprover {
    fn approve(&self, _request: ApprovalRequest) -> Result<bool, String> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }
}

pub fn open_store(path: &Path) -> Store {
    Store::open(
        path,
        StoreOptions {
            no_fsync: true,
            ..Default::default()
        },
    )
    .expect("open store")
}

pub fn read_events(store: &Store) -> Vec<Event> {
    let mut events = Vec::new();
    store
        .read_from(0, |e| {
            events.push(e.clone());
            Ok(())
        })
        .expect("read log");
    events
}

pub fn read_events_at(path: &Path) -> Vec<Event> {
    let store = open_store(path);
    read_events(&store)
}

pub fn payload(ev: &Event) -> Value {
    ev.payload.clone().unwrap_or_else(|| json!({}))
}

pub fn first<'a>(events: &'a [Event], typ: &str) -> Option<&'a Event> {
    events.iter().find(|e| e.typ.as_str() == typ)
}

pub fn last<'a>(events: &'a [Event], typ: &str) -> Option<&'a Event> {
    events.iter().rev().find(|e| e.typ.as_str() == typ)
}

pub fn count(events: &[Event], typ: &str) -> usize {
    events.iter().filter(|e| e.typ.as_str() == typ).count()
}

pub fn child_ids(dir: &Path) -> Vec<String> {
    let sessions = dir.join("sessions");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&sessions) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".jsonl") {
                out.push(id.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Assert the log is dense in `seq` and every event belongs to `session`.
pub fn assert_dense(events: &[Event], session: &str) {
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.seq, (i + 1) as u64, "dense seq at index {i}");
        assert_eq!(ev.session, session, "session id at index {i}");
    }
}

// ---- delta builders -------------------------------------------------------

pub fn text(text: &str) -> Delta {
    Delta {
        text: text.into(),
        ..Default::default()
    }
}

pub fn tool_delta(index: usize, id: &str, name: &str, args: &str) -> Delta {
    Delta {
        tool_call: Some(ToolCallDelta {
            index,
            id: id.into(),
            name: name.into(),
            args_fragment: args.into(),
        }),
        ..Default::default()
    }
}

pub fn stop(reason: &str) -> Delta {
    Delta {
        stop_reason: Some(reason.into()),
        ..Default::default()
    }
}

pub fn text_response(content: &str) -> Response {
    Response::Deltas(vec![text(content), stop("stop")])
}
