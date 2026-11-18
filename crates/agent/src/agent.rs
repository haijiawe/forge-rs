//! The one-turn agent loop (port of forge-go/internal/agent/agent.go).
//!
//! One public call [`Agent::run`] drives a single user turn. History is never
//! cached: every run re-reads the append-only log and re-projects it through
//! `forge_session` (invariant I1). Events for a turn are emitted in the frozen
//! events-v1 order; the envelope step is 1-based while payload indices stay
//! 0-based.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use forge_core::{Error, ErrorKind, Event, EventType, Result};
use forge_llm::{Aggregator, ContentPart, Message, Provider, Request, Stream, ToolCall, Usage};
use forge_session::{Projection, Snapshot};
use serde_json::{json, Map, Value};

use crate::cancel::Cancel;
use crate::err::{
    cancelled_error, code_of, is_cancelled, refusal_error, retry_after_of, retryable_of,
};
use crate::history::{next_turn, open_turn, to_llm_messages, to_llm_messages_with_parts};
use crate::payload::{
    clip_system, error_event, llm_call, normalize_stop, step_end, step_start, stop_with_calls,
    turn_end, turn_reason, turn_start, usage_payload, user_message, CODE_CANCELLED,
    MAX_SYSTEM_BYTES, STOP_CANCELLED, STOP_END_TURN, STOP_ERROR, STOP_MAX_TOKENS, STOP_TOOL_USE,
};
use crate::tool::{EventLog, ToolRunner};

/// Callback receiving each streamed text fragment.
pub type TextHook = Arc<dyn Fn(&str) + Send + Sync>;
/// Callback receiving `(turn, step, fragment)` before aggregation.
pub type DeltaHook = Arc<dyn Fn(u32, u32, &str) + Send + Sync>;
/// Steering closure, polled before every step after the first.
pub type SteeringHook = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Supplies image parts for one projected message, or nothing when the caller has
/// no bytes for it. Named rather than inlined: a closure type written twice is a
/// type written twice, and the option around it reads as noise either way.
pub type AttachmentHook =
    Arc<dyn Fn(&forge_session::projection::Message) -> Vec<ContentPart> + Send + Sync>;
/// Deterministic clock returning epoch milliseconds (injectable for goldens).
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Everything [`Agent::run`] needs. No hidden global state; callers own every
/// collaborator and can swap a fake provider or a deterministic clock.
pub struct Options {
    pub provider: Arc<dyn Provider>,
    pub log: Arc<dyn EventLog>,
    pub model: String,
    pub session_id: String,
    pub workspace: String,
    pub system: String,
    /// Set later by the W3 tool crate; `None` means the loop refuses tool calls.
    pub tools: Option<Arc<dyn ToolRunner>>,
    pub max_steps: usize,
    pub max_retries: usize,
    /// The HTTP layer already wrote `turn/start` and `user/message`.
    pub prompt_recorded: bool,
    pub steering: Option<SteeringHook>,
    pub on_text: Option<TextHook>,
    pub on_delta: Option<DeltaHook>,
    /// Supplies image parts for one projected message, or nothing when the
    /// caller has no bytes for it. `None` is the text-only loop this crate
    /// shipped before attachments could reach the model.
    pub attachments: Option<AttachmentHook>,
    pub clock: Clock,
    pub cancel: Cancel,
}

impl Options {
    /// Build options with the Go defaults (24 steps, 2 retries, wall clock).
    pub fn new(
        provider: Arc<dyn Provider>,
        log: Arc<dyn EventLog>,
        model: impl Into<String>,
        session_id: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            log,
            model: model.into(),
            session_id: session_id.into(),
            workspace: workspace.into(),
            system: String::new(),
            tools: None,
            max_steps: 24,
            max_retries: 2,
            prompt_recorded: false,
            steering: None,
            on_text: None,
            on_delta: None,
            attachments: None,
            clock: Arc::new(wall_clock_ms),
            cancel: Cancel::default(),
        }
    }
}

fn wall_clock_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}

/// Outcome of one turn. Kept even when `error` is set so callers can render the
/// partial reply, exactly like the Go `(Result, error)` pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunResult {
    pub turn: u32,
    pub steps: usize,
    pub text: String,
    /// One of `TURN_COMPLETED`, `TURN_CANCELLED`, `TURN_ERROR`.
    pub reason: &'static str,
}

/// The one-turn loop. Cheap to clone handles; all state is in `Options`.
pub struct Agent {
    opt: Options,
    cur_turn: AtomicU32,
    cur_step: AtomicU32,
}

impl Agent {
    /// Validate the required knobs. Provider and log are `Arc`s, so presence is
    /// guaranteed by the type; model/session are checked to match the Go errors.
    pub fn new(opt: Options) -> Result<Self> {
        if opt.model.is_empty() {
            return Err(Error::new(ErrorKind::Invalid, "agent: Model is required"));
        }
        if opt.session_id.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "agent: SessionID is required",
            ));
        }
        if opt.max_steps == 0 {
            return Err(Error::new(
                ErrorKind::Invalid,
                "agent: MaxSteps must be positive",
            ));
        }
        Ok(Self {
            opt,
            cur_turn: AtomicU32::new(0),
            cur_step: AtomicU32::new(0),
        })
    }

    /// A handle callers use to cancel an in-flight turn.
    pub fn canceller(&self) -> Cancel {
        self.opt.cancel.clone()
    }

    /// Run one user turn. The optional error accompanies (never replaces) the
    /// result: a cancelled or failed turn still carries its partial text and
    /// reason, and the `turn/end` event is always written.
    pub fn run(&self, text: &str) -> (RunResult, Option<Error>) {
        if let Err(e) = self.ensure_session() {
            return (RunResult::default(), Some(e));
        }
        let snap = match self.snapshot() {
            Ok(s) => s,
            Err(e) => return (RunResult::default(), Some(e)),
        };
        let mut turn = next_turn(&snap);
        // Attachments arrive as bodies in the caller's blob store, so the loop
        // only asks for the parts it was told it can supply. With no hook this
        // is byte for byte the text-only history it has always built.
        let mut history = match &self.opt.attachments {
            Some(hook) => to_llm_messages_with_parts(&snap, &self.opt.system, hook.as_ref()),
            None => to_llm_messages(&snap, &self.opt.system),
        };

        // PromptRecorded: adopt the turn the HTTP layer already opened instead
        // of writing turn/start + user/message a second time.
        let mut adopted = false;
        if self.opt.prompt_recorded {
            let open = open_turn(&snap);
            if open != 0 {
                turn = open;
                adopted = true;
            }
        }
        if !adopted {
            if let Err(e) = self.emit(turn, 0, turn_start(turn)) {
                return (RunResult::default(), Some(e));
            }
            if let Err(e) = self.emit(turn, 0, user_message(text)) {
                return (RunResult::default(), Some(e));
            }
            history.push(Message::user(text));
        }

        let steps = self.steps(turn, &mut history);
        let reason = turn_reason(steps.stop);
        // endErr only overrides err when err is nil (Go parity).
        let end_res = self.emit(turn, 0, turn_end(reason, steps.count));
        let error = match steps.error {
            Some(e) => Some(e),
            None => end_res.err(),
        };
        (
            RunResult {
                turn,
                steps: steps.count,
                text: steps.text,
                reason,
            },
            error,
        )
    }

    // ---- setup ------------------------------------------------------------

    fn ensure_session(&self) -> Result<()> {
        if self.opt.log.last_seq() > 0 {
            return Ok(());
        }
        let mut m = Map::new();
        m.insert("workspace".into(), json!(self.opt.workspace));
        m.insert("model".into(), json!(self.opt.model));
        m.insert("startedBy".into(), json!("cli"));
        if !self.opt.system.is_empty() {
            let (kept, truncated) = clip_system(&self.opt.system, MAX_SYSTEM_BYTES);
            m.insert("system".into(), json!(kept));
            if truncated {
                m.insert("systemTruncated".into(), json!(true));
                m.insert("systemBytes".into(), json!(self.opt.system.len()));
            }
        }
        self.emit(0, 0, (EventType::SESSION_START, Value::Object(m)))?;
        Ok(())
    }

    /// I1: re-read every event and re-project through forge_session. There is
    /// no second copy of conversation truth anywhere in this crate.
    fn snapshot(&self) -> Result<Snapshot> {
        let events = self.opt.log.read_all()?;
        Projection::project(&events)
    }

    // ---- event emission ---------------------------------------------------

    fn emit(&self, turn: u32, step: u32, spec: (EventType, Value)) -> Result<u64> {
        let (typ, payload) = spec;
        let mut ev = Event::new(self.opt.session_id.clone(), typ, Some(payload));
        ev.turn = turn;
        ev.step = step;
        self.opt.log.append(&ev)
    }

    fn record_error(&self, message: &str, code: &str, retryable: bool) {
        // Append failures on the diagnostic channel are deliberately ignored;
        // the primary outcome is carried by the turn's own events.
        let _ = self.emit(0, 0, error_event(message, code, retryable));
    }

    fn record_error_from(&self, err: &Error) {
        self.record_error(err.message(), code_of(err), retryable_of(err));
    }

    fn record_tool_calls(&self, turn: u32, step: u32, calls: &[ToolCall]) {
        for c in calls {
            let args = if c.args.is_null() {
                json!({})
            } else {
                c.args.clone()
            };
            let _ = self.emit(
                turn,
                step,
                (
                    EventType::TOOL_CALL,
                    json!({"callId": c.id, "tool": c.name, "args": args}),
                ),
            );
        }
    }

    // ---- step loop --------------------------------------------------------

    fn steps(&self, turn: u32, history: &mut Vec<Message>) -> StepsOut {
        let mut text = String::new();
        for index in 0..self.opt.max_steps {
            if self.opt.cancel.is_cancelled() {
                return StepsOut {
                    count: index,
                    stop: STOP_CANCELLED,
                    text,
                    error: Some(cancelled_error()),
                };
            }
            // Steering is injected into the in-memory history only — never as
            // an event — and only before steps after the first.
            if index > 0 {
                if let Some(steer) = &self.opt.steering {
                    for s in steer() {
                        if !s.trim().is_empty() {
                            history.push(Message::user(s));
                        }
                    }
                }
            }

            let out = self.step(turn, index as u32, history);
            text.push_str(&out.text);
            if !out.text.is_empty() || !out.calls.is_empty() {
                let mut m = Message::assistant(&out.text);
                m.tool_calls = out.calls.clone();
                history.push(m);
            }
            let count = index + 1;

            if out.stop == STOP_CANCELLED {
                return StepsOut {
                    count,
                    stop: STOP_CANCELLED,
                    text,
                    error: Some(cancelled_error()),
                };
            }
            if let Some(e) = out.error {
                return StepsOut {
                    count,
                    stop: STOP_ERROR,
                    text,
                    error: Some(e),
                };
            }
            if out.stop == STOP_TOOL_USE {
                if out.calls.is_empty() {
                    const MSG: &str = "agent: model stopped for tool calls but produced none";
                    self.record_error(MSG, "no_tool_calls", false);
                    return StepsOut {
                        count,
                        stop: STOP_ERROR,
                        text,
                        error: Some(refusal_error(MSG, "no_tool_calls")),
                    };
                }
                let tools = match &self.opt.tools {
                    Some(t) => Arc::clone(t),
                    None => {
                        const MSG: &str = "agent: no tools are configured for this session";
                        self.record_error(MSG, "tool_unavailable", false);
                        return StepsOut {
                            count,
                            stop: STOP_ERROR,
                            text,
                            error: Some(refusal_error(MSG, "tool_unavailable")),
                        };
                    }
                };
                if let Err(e) =
                    self.run_tools(turn, count as u32, &out.calls, tools.as_ref(), history)
                {
                    let stop = if is_cancelled(&e) {
                        STOP_CANCELLED
                    } else {
                        STOP_ERROR
                    };
                    return StepsOut {
                        count,
                        stop,
                        text,
                        error: Some(e),
                    };
                }
                continue;
            }
            if out.stop == STOP_MAX_TOKENS {
                return StepsOut {
                    count,
                    stop: STOP_MAX_TOKENS,
                    text,
                    error: None,
                };
            }
            return StepsOut {
                count,
                stop: STOP_END_TURN,
                text,
                error: None,
            };
        }
        let msg = format!("agent: turn exceeded {} steps", self.opt.max_steps);
        self.record_error(&msg, "max_steps", false);
        StepsOut {
            count: self.opt.max_steps,
            stop: STOP_ERROR,
            text,
            error: Some(refusal_error(msg, "max_steps")),
        }
    }

    fn step(&self, turn: u32, index: u32, history: &[Message]) -> StepOut {
        let step_no = index + 1; // envelope step is 1-based; payload index stays 0-based
        self.cur_turn.store(turn, Ordering::SeqCst);
        self.cur_step.store(step_no, Ordering::SeqCst);
        let _ = self.emit(turn, step_no, step_start(index, &self.opt.model));

        let specs = self
            .opt
            .tools
            .as_ref()
            .map(|t| t.specs())
            .unwrap_or_default();
        let req = Request {
            model: self.opt.model.clone(),
            messages: history.to_vec(),
            tools: specs,
            ..Request::default()
        };
        if let Err(e) = req.validate() {
            self.record_error(e.message(), "invalid_request", false);
            let _ = self.emit(turn, step_no, step_end(index, STOP_ERROR));
            return StepOut {
                text: String::new(),
                stop: STOP_ERROR,
                calls: Vec::new(),
                error: Some(e),
            };
        }
        let names: Vec<String> = req.tools.iter().map(|t| t.name.clone()).collect();
        let _ = self.emit(
            turn,
            step_no,
            llm_call(&self.opt.model, req.messages.len(), &names),
        );

        let started = (self.opt.clock)();
        let streamed = self.stream(&req);
        let duration_ms = (self.opt.clock)() - started;

        let mut payload = Map::new();
        payload.insert("text".into(), json!(streamed.text));
        payload.insert("stopReason".into(), json!(streamed.stop));
        payload.insert("durationMs".into(), json!(duration_ms));
        if streamed.usage.total() > 0 {
            payload.insert("usage".into(), usage_payload(&streamed.usage));
        }
        if streamed.stop == STOP_CANCELLED {
            payload.insert("partial".into(), json!(true));
        }
        let _ = self.emit(
            turn,
            step_no,
            (EventType::ASSISTANT_MESSAGE, Value::Object(payload)),
        );
        if streamed.stop == STOP_TOOL_USE {
            self.record_tool_calls(turn, step_no, &streamed.calls);
        }
        let _ = self.emit(turn, step_no, step_end(index, streamed.stop));

        StepOut {
            text: streamed.text,
            stop: streamed.stop,
            calls: streamed.calls,
            error: streamed.error,
        }
    }

    // ---- provider streaming ----------------------------------------------

    fn stream(&self, req: &Request) -> StreamOut {
        let mut text = String::new();
        let mut usage = Usage::default();
        for attempt in 0..=self.opt.max_retries {
            let mut agg = Aggregator::new();
            // A stream that fails to open is a provider failure like any other.
            let stream = match self.opt.provider.stream(req) {
                Ok(s) => s,
                Err(e) => {
                    if is_cancelled(&e) {
                        return StreamOut {
                            text,
                            usage,
                            calls: Vec::new(),
                            stop: STOP_CANCELLED,
                            error: Some(e),
                        };
                    }
                    if !retryable_of(&e) || !text.is_empty() || attempt >= self.opt.max_retries {
                        self.record_error_from(&e);
                        return StreamOut {
                            text,
                            usage,
                            calls: Vec::new(),
                            stop: STOP_ERROR,
                            error: Some(e),
                        };
                    }
                    self.wait_retry(&e);
                    continue;
                }
            };

            let consumed = self.consume(stream, &mut agg, &mut usage);
            text.push_str(&consumed.text);
            if consumed.cancelled {
                return StreamOut {
                    text,
                    usage,
                    calls: Vec::new(),
                    stop: STOP_CANCELLED,
                    error: Some(cancelled_error()),
                };
            }
            if let Some(e) = consumed.error {
                // Only retry before the first text fragment; a retry after text
                // would duplicate that fragment in the log.
                if !retryable_of(&e) || !text.is_empty() || attempt >= self.opt.max_retries {
                    self.record_error_from(&e);
                    return StreamOut {
                        text,
                        usage,
                        calls: Vec::new(),
                        stop: STOP_ERROR,
                        error: Some(e),
                    };
                }
                self.wait_retry(&e);
                continue;
            }
            let calls = match agg.finish() {
                Ok(c) => c,
                Err(e) => {
                    self.record_error_from(&e);
                    return StreamOut {
                        text,
                        usage,
                        calls: Vec::new(),
                        stop: STOP_ERROR,
                        error: Some(e),
                    };
                }
            };
            let stop =
                stop_with_calls(normalize_stop(consumed.stop_reason.as_deref()), calls.len());
            return StreamOut {
                text,
                usage,
                calls,
                stop,
                error: None,
            };
        }
        // Unreachable: every branch above returns at the last attempt.
        StreamOut {
            text,
            usage,
            calls: Vec::new(),
            stop: STOP_ERROR,
            error: Some(refusal_error(
                "agent: provider retries exhausted",
                "retry_exhausted",
            )),
        }
    }

    /// Read one stream to EOF (or cancellation/error), closing exactly once.
    fn consume(
        &self,
        stream: Box<dyn Stream>,
        agg: &mut Aggregator,
        usage: &mut Usage,
    ) -> Consumed {
        let mut closer = StreamClose {
            stream,
            closed: false,
        };
        let mut text = String::new();
        let mut stop_reason: Option<String> = None;
        loop {
            if self.opt.cancel.is_cancelled() {
                return Consumed {
                    text,
                    stop_reason,
                    cancelled: true,
                    error: None,
                };
            }
            match closer.stream.recv() {
                Ok(Some(delta)) => {
                    if !delta.text.is_empty() {
                        text.push_str(&delta.text);
                        if let Some(cb) = &self.opt.on_text {
                            cb(&delta.text);
                        }
                        if let Some(cb) = &self.opt.on_delta {
                            cb(
                                self.cur_turn.load(Ordering::SeqCst),
                                self.cur_step.load(Ordering::SeqCst),
                                &delta.text,
                            );
                        }
                    }
                    if let Some(tc) = delta.tool_call {
                        agg.add(tc);
                    }
                    if let Some(u) = delta.usage {
                        usage.add(u);
                    }
                    if delta.stop_reason.is_some() {
                        stop_reason = delta.stop_reason;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    return Consumed {
                        text,
                        stop_reason,
                        cancelled: is_cancelled(&e),
                        error: Some(e),
                    };
                }
            }
        }
        Consumed {
            text,
            stop_reason,
            cancelled: false,
            error: None,
        }
    }

    fn wait_retry(&self, err: &Error) {
        let mut remaining = retry_after_of(err);
        let slice = Duration::from_millis(10);
        while !remaining.is_zero() && !self.opt.cancel.is_cancelled() {
            let d = remaining.min(slice);
            std::thread::sleep(d);
            remaining = remaining.saturating_sub(d);
        }
    }

    // ---- tool execution ---------------------------------------------------

    fn run_tools(
        &self,
        turn: u32,
        step: u32,
        calls: &[ToolCall],
        tools: &dyn ToolRunner,
        history: &mut Vec<Message>,
    ) -> Result<()> {
        let mut msgs = Vec::with_capacity(calls.len());
        let mut parts: Vec<ContentPart> = Vec::new();
        for call in calls {
            if self.opt.cancel.is_cancelled() {
                return Err(cancelled_error());
            }
            let res = tools.run(call.clone())?;
            let mut m = Map::new();
            m.insert("callId".into(), json!(call.id));
            m.insert("tool".into(), json!(call.name));
            m.insert("ok".into(), json!(!res.is_error));
            m.insert("output".into(), json!(res.output));
            if res.is_error && !res.code.is_empty() {
                m.insert("code".into(), json!(res.code));
            }
            if res.truncated {
                m.insert("truncated".into(), json!(true));
            }
            if let Some(meta) = &res.meta {
                if meta.as_object().map(|o| !o.is_empty()).unwrap_or(true) {
                    m.insert("meta".into(), meta.clone());
                }
            }
            self.emit(turn, step, (EventType::TOOL_RESULT, Value::Object(m)))?;
            if res.code == CODE_CANCELLED {
                return Err(cancelled_error());
            }
            // Providers reject an empty tool message, so substitute a marker.
            let output = if res.output.is_empty() {
                if res.is_error && !res.code.is_empty() {
                    format!("the tool failed with code {}", res.code)
                } else {
                    "(no output)".to_string()
                }
            } else {
                res.output
            };
            msgs.push(Message::tool_result(&call.id, output));
            if !res.parts.is_empty() {
                parts.extend(res.parts);
            }
        }
        history.extend(msgs);
        // A tool result can only be text in the chat-completions protocol, so
        // the bytes a tool hands back ride in on the next user turn. The log
        // recorded the path and the size; the bytes were never written there,
        // which is why the next turn can re-read them from disk.
        if !parts.is_empty() {
            history.push(
                Message::user(format!(
                    "[{} image(s) returned by the tool call(s) above]",
                    parts.len()
                ))
                .with_parts(parts),
            );
        }
        Ok(())
    }
}

// ---- internal result shapes -----------------------------------------------

struct StepsOut {
    count: usize,
    stop: &'static str,
    text: String,
    error: Option<Error>,
}

struct StepOut {
    text: String,
    stop: &'static str,
    calls: Vec<ToolCall>,
    error: Option<Error>,
}

struct StreamOut {
    text: String,
    usage: Usage,
    calls: Vec<ToolCall>,
    stop: &'static str,
    error: Option<Error>,
}

struct Consumed {
    text: String,
    stop_reason: Option<String>,
    cancelled: bool,
    error: Option<Error>,
}

/// Guarantees `Stream::close` is called exactly once on every return path.
struct StreamClose {
    stream: Box<dyn Stream>,
    closed: bool,
}

impl StreamClose {
    fn close_once(&mut self) {
        if !self.closed {
            self.stream.close();
            self.closed = true;
        }
    }
}

impl Drop for StreamClose {
    fn drop(&mut self) {
        self.close_once();
    }
}
