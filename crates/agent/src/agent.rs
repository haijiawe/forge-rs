//! The one-turn agent loop (port of forge-go/internal/agent/agent.go).
//!
//! One public call [`Agent::run`] drives a single user turn. History is only
//! ever derived from the append-only log, and the fold that derives it keeps a
//! cursor: a resident loop replays the tail it has not seen instead of the whole
//! file every turn (invariant I1 still holds — the cursor is a position in the
//! log, not a second copy of anything in it). Events for a turn are emitted in
//! the frozen events-v1 order; the envelope step is 1-based while payload
//! indices stay 0-based.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hearth_core::{Error, ErrorKind, Event, EventType, Result};
use hearth_llm::{Aggregator, ContentPart, Message, Provider, Request, Stream, ToolCall, Usage};
use hearth_session::{Projection, Snapshot};
use serde_json::{json, Map, Value};

use crate::cancel::Cancel;
use crate::err::{
    cancelled_error, code_of, is_cancelled, refusal_error, retry_after_of, retryable_of,
};
use crate::history::{next_turn, open_turn, to_llm_messages_hiding};
use crate::inbox::{Inbox, Msg, Target};
use crate::payload::{
    assistant_attempt, clip_system, error_event, llm_call, normalize_stop, request_context,
    request_header, step_end, step_start, stop_with_calls, turn_end, turn_reason, turn_start,
    usage_payload, user_message, CODE_CANCELLED, MAX_SYSTEM_BYTES, STOP_CANCELLED, STOP_END_TURN,
    STOP_ERROR, STOP_MAX_TOKENS, STOP_TOOL_USE,
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
    Arc<dyn Fn(&hearth_session::projection::Message) -> Vec<ContentPart> + Send + Sync>;
/// Answers whether a projected message's seq sits behind a compressed block.
///
/// The ledger that knows lives with the surface owning the log, so the loop is
/// told which seqs are hidden instead of being handed a log to fold.
pub type HiddenHook = Arc<dyn Fn(u64) -> bool + Send + Sync>;
/// The member of a tool result's `meta` that asks the loop to append a
/// `context/compressed` record.
///
/// The tool cannot append it itself: the loop is the only writer, and a second
/// writer would race it into two sequences. So `compress` hands back the payload
/// it wants written, exactly as an approver hands back a decision.
pub const CONTEXT_BLOCK_META: &str = "contextBlock";
/// The member of a tool result's `meta` that asks the loop to append a
/// `subagent/catalog` record on this (the parent) log.
///
/// Same reason as [`CONTEXT_BLOCK_META`]: the subagent tool knows which child it
/// started and under what name, but only the loop may write the parent log, so
/// the tool hands back the row and the loop appends it.
pub const SUBAGENT_CATALOG_META: &str = "subagentCatalog";
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
    /// Which seqs are behind a compressed block. `None` is the whole history,
    /// which is what every caller saw before blocks existed.
    pub hidden: Option<HiddenHook>,
    /// The rewrite seams (`pre-step`, `request`). Empty by default: the loop is
    /// inert and byte-for-byte what it was until a caller installs a hook. See
    /// [ADR-0004](../../docs/11-adr-agent-hooks.md) and [`crate::hooks`].
    pub hooks: crate::hooks::Hooks,
    /// Observers, not rewriters: every appended event is offered to `emit`, and
    /// `serial` runs at the turn boundary. Empty by default, so a caller that
    /// installs nothing gets the same behaviour and the same bytes.
    pub dispatch: crate::dispatch::Dispatch,
    pub clock: Clock,
    pub cancel: Cancel,
    /// Where requests go, as far as the caller is willing to say. The loop talks
    /// to a `Provider` and cannot see an endpoint through it, so this is a label
    /// the caller supplies (usually the host of the configured base URL). Empty
    /// means "not stated" and the row omits the field rather than guessing.
    pub provider_name: String,
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
            hidden: None,
            hooks: crate::hooks::Hooks::none(),
            dispatch: crate::dispatch::Dispatch::none(),
            clock: Arc::new(wall_clock_ms),
            cancel: Cancel::default(),
            provider_name: String::new(),
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

/// Phase values for [`Agent::phase`]. Kept as `u8` in an atomic because the
/// loop is `&self` and callers may observe the phase from another thread.
const PHASE_IDLE: u8 = 0;
const PHASE_RUNNING: u8 = 1;
const PHASE_MAINTENANCE: u8 = 2;

/// A fold cursor over the log.
///
/// The projection inside is derived state, never truth: it only ever advances
/// by events the store just handed over, and `Projection::apply` drops a record
/// whose `seq` is not newer than the last one it folded. That guard is what
/// makes a stale cursor harmless — re-reading a few events costs a few skips,
/// not double-counted messages.
struct Folded {
    projection: Projection,
    /// Byte position just past the last complete line folded in.
    offset: u64,
}

/// The one-turn loop. Cheap to clone handles; all state is in `Options`.
pub struct Agent {
    opt: Options,
    cur_turn: AtomicU32,
    cur_step: AtomicU32,
    /// Tool names of the previous request in this run, so `request/header` can
    /// say `change` when the set moved instead of always claiming `series`.
    last_tools: Mutex<Vec<String>>,
    /// The resident phase. `run` moves it to `running` for its duration and back
    /// to `idle`; `kick` drives many `run`s. Nothing else touches it.
    phase: AtomicU8,
    /// The fold cursor behind [`Agent::snapshot`]. Before this existed every turn
    /// read and re-parsed the whole log: at the bench's measured 88,916 projected
    /// events/second that is 1.8 s of work at 100k events, spent before the
    /// first byte reaches the model.
    folded: Mutex<Option<Folded>>,
    /// FNV-1a of the system prompt, computed once at construction. It is a
    /// change detector, not a checksum of trust: it lets `request/context` say
    /// "these two requests carried the same prompt" without spending eight
    /// kilobytes per step to say it.
    system_fingerprint: String,
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
        let system_fingerprint = hearth_core::fingerprint(opt.system.as_bytes());
        Ok(Self {
            opt,
            cur_turn: AtomicU32::new(0),
            cur_step: AtomicU32::new(0),
            last_tools: Mutex::new(Vec::new()),
            phase: AtomicU8::new(PHASE_IDLE),
            folded: Mutex::new(None),
            system_fingerprint,
        })
    }

    /// A handle callers use to cancel an in-flight turn.
    pub fn canceller(&self) -> Cancel {
        self.opt.cancel.clone()
    }

    // ---- the resident surface ---------------------------------------------
    //
    // DSH's `ReactLoopAgent` is a long-lived object with a phase, an inbox and a
    // `kick()` that runs turns until there is nothing left. hearth's loop has
    // always been `run(text)` = exactly one turn, and that stays: the CLI, the
    // HTTP server and every golden file depend on it. What this section adds is
    // the *resident* shape on top, so a caller that wants a queue gets one that
    // survives a restart instead of one that dies with the process.
    //
    // The queue lives in the log (`agent/inbox` rows), never in a field here.
    // That is deliberate: an in-process deque would be a second copy of the
    // truth, and the whole point is that a second process can pick the work up.

    /// The phase a resident loop is in.
    pub fn phase(&self) -> &'static str {
        match self.phase.load(Ordering::SeqCst) {
            PHASE_RUNNING => "running",
            PHASE_MAINTENANCE => "maintenance",
            _ => "idle",
        }
    }

    /// Move to a phase and record it. A same-phase transition writes nothing:
    /// the log records changes, not heartbeats.
    fn set_phase(&self, next: u8, record: bool) -> Result<()> {
        let prev = self.phase.swap(next, Ordering::SeqCst);
        if prev == next || !record {
            return Ok(());
        }
        let name = match next {
            PHASE_RUNNING => "running",
            PHASE_MAINTENANCE => "maintenance",
            _ => "idle",
        };
        self.emit(0, 0, (EventType::AGENT_STATUS, json!({"phase": name})))
            .map(|_| ())
    }

    /// The pending queues, folded out of the log.
    pub fn pending(&self) -> Result<Inbox> {
        let events = self.opt.log.read_all()?;
        Ok(crate::inbox::fold(&events))
    }

    /// Queue a message for a later turn or step.
    pub fn enqueue(&self, text: &str, target: Target) -> Result<Msg> {
        self.ensure_session()?;
        let pending = self.pending()?;
        let msg = Msg::new(text)?;
        let ev = crate::inbox::enqueue(&self.opt.session_id, target, &pending, &msg);
        self.opt.log.append(&ev)?;
        Ok(msg)
    }

    /// Take the head of a queue out of the log.
    fn claim(&self, target: Target) -> Result<Option<Msg>> {
        let pending = self.pending()?;
        let Some(head) = pending.queue(target).first().cloned() else {
            return Ok(None);
        };
        let ev = crate::inbox::splice(&self.opt.session_id, target, 0, 1, &[]);
        self.opt.log.append(&ev)?;
        Ok(Some(head))
    }

    /// Run turns until both queues are empty.
    ///
    /// `next-step` is drained before `next-turn`: a message queued for the next
    /// step was queued when something was running, so it is the more urgent of
    /// the two. Each claimed message becomes exactly one `run`, which keeps the
    /// one-turn-per-prompt invariant the rest of the tree relies on.
    ///
    /// This is synchronous, so `when_idle` is trivially true once it returns —
    /// see `kick` for why that is honest here rather than a stub.
    pub fn kick(&self) -> (Vec<RunResult>, Option<Error>) {
        let mut out = Vec::new();
        loop {
            let target = match self.claim(Target::NextStep) {
                Ok(Some(msg)) => {
                    let (result, err) = self.run(&msg.text);
                    out.push(result);
                    if let Some(e) = err {
                        return (out, Some(e));
                    }
                    continue;
                }
                Ok(None) => Target::NextTurn,
                Err(e) => return (out, Some(e)),
            };
            match self.claim(target) {
                Ok(Some(msg)) => {
                    let (result, err) = self.run(&msg.text);
                    out.push(result);
                    if let Some(e) = err {
                        return (out, Some(e));
                    }
                }
                Ok(None) => return (out, None),
                Err(e) => return (out, Some(e)),
            }
        }
    }

    /// Run one maintenance job while the loop is between turns.
    ///
    /// DSH's maintenance phase exists so background work (compaction, indexing)
    /// can happen without a model turn in flight. Here the job is a plain
    /// closure, and the phase is recorded on the log because unlike a turn, a
    /// maintenance window is something a reader has no other way to see: without
    /// agent/status an unexplained gap between two turns is indistinguishable
    /// from a slow provider.
    pub fn run_maintenance<F>(&self, job: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        self.ensure_session()?;
        self.set_phase(PHASE_MAINTENANCE, true)?;
        let out = job();
        let _ = self.set_phase(PHASE_IDLE, true);
        out
    }

    /// True when nothing is running. A caller that needs "wait until the loop
    /// stops" gets the same answer, because `kick` cannot return while a turn is
    /// in flight.
    pub fn when_idle(&self) -> bool {
        self.phase() == "idle"
    }

    /// Run one user turn. The optional error accompanies (never replaces) the
    /// result: a cancelled or failed turn still carries its partial text and
    /// reason, and the `turn/end` event is always written.
    pub fn run(&self, text: &str) -> (RunResult, Option<Error>) {
        // The phase moves in memory only, not on the log: a resident loop's
        // liveness is not part of a session's history, and writing two rows per
        // turn would put a diagnostic in every golden file for no reader's
        // benefit. run_maintenance is the one caller that does record a phase,
        // because there the phase is the whole point.
        let previous = self.phase.swap(PHASE_RUNNING, Ordering::SeqCst);
        let out = self.run_adopted(text);
        self.phase.store(previous, Ordering::SeqCst);
        out
    }

    /// The body of one turn. Split out so the phase is restored at exactly one
    /// place instead of at every early return.
    fn run_adopted(&self, text: &str) -> (RunResult, Option<Error>) {
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
        let hidden = self.opt.hidden.clone();
        let hide = move |seq: u64| hidden.as_ref().map(|hook| hook(seq)).unwrap_or(false);
        let mut history = match &self.opt.attachments {
            Some(hook) => to_llm_messages_hiding(&snap, &self.opt.system, hook.as_ref(), &hide),
            None => to_llm_messages_hiding(&snap, &self.opt.system, &|_| Vec::new(), &hide),
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
        // Serial listeners run at the boundary between the last step and the
        // turn's own close: this is the one place a listener may need to finish
        // something that must happen before the turn is over.
        if !self.opt.dispatch.is_empty() {
            let stopping = json!({"reason": reason, "steps": steps.count});
            let view =
                crate::dispatch::EventView::new(&EventType::TURN_END, turn, 0, Some(&stopping));
            self.opt.dispatch.serial(&view);
        }
        // A cancelled turn records why, when the token knows. The operator
        // stopping a turn and an owner being disposed are both `cancelled`, and
        // telling them apart afterwards is the whole point of the member.
        let cancelled_by = self.opt.cancel.cause_str();
        // endErr only overrides err when err is nil (Go parity).
        let end_res = self.emit(turn, 0, turn_end(reason, steps.count, cancelled_by));
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

    /// I1: derive history from the log, and from nothing else.
    ///
    /// A resident loop folds once and then replays only what was appended since.
    /// The fallback path is not dead code: an in-memory log cannot report byte
    /// positions, so `read_after` answers `Ok(None)` there and this folds the
    /// whole file exactly as it always did. That keeps every existing test on
    /// the same code path it was written against.
    fn snapshot(&self) -> Result<Snapshot> {
        let mut guard = self.folded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = guard.as_mut() {
            let offset = state.offset;
            let projection = &mut state.projection;
            match self
                .opt
                .log
                .read_after(offset, &mut |event| projection.apply(event))
            {
                Ok(Some(moved)) => {
                    state.offset = moved;
                    return Ok(state.projection.snapshot());
                }
                // No cursor support: fall through and fold afresh.
                Ok(None) => {}
                Err(e) => {
                    // Leave the cursor where it was. Applying is idempotent by
                    // seq, so the next turn re-reads from here without
                    // double-counting anything it already folded.
                    return Err(e);
                }
            }
        }
        let mut projection = Projection::new();
        let offset = self
            .opt
            .log
            .read_after(0, &mut |event| projection.apply(event))?;
        let snapshot = projection.snapshot();
        if let Some(offset) = offset {
            *guard = Some(Folded { projection, offset });
        }
        Ok(snapshot)
    }

    // ---- event emission ---------------------------------------------------

    fn emit(&self, turn: u32, step: u32, spec: (EventType, Value)) -> Result<u64> {
        let (typ, payload) = spec;
        let mut ev = Event::new(self.opt.session_id.clone(), typ, Some(payload));
        ev.turn = turn;
        ev.step = step;
        let seq = self.opt.log.append(&ev)?;
        // Observers run after the append, never before: an event a listener sees
        // is an event the log already holds, so a listener cannot promise
        // anything the log does not.
        if !self.opt.dispatch.is_empty() {
            let view = crate::dispatch::EventView::new(&typ, turn, step, ev.payload.as_ref());
            self.opt.dispatch.emit(&view);
        }
        Ok(seq)
    }

    fn record_error(&self, message: &str, code: &str, retryable: bool) {
        // Append failures on the diagnostic channel are deliberately ignored;
        // the primary outcome is carried by the turn's own events.
        let _ = self.emit(0, 0, error_event(message, code, retryable));
    }

    fn record_error_from(&self, err: &Error) {
        self.record_error(err.message(), code_of(err), retryable_of(err));
    }

    /// Record a hook rewrite as one `hook/rewrite` event, one line per phase that
    /// actually changed something. `changed` is the list of hook names that
    /// rewrote the request; an empty list means no hook changed anything and no
    /// event is written (the inert case must stay silent).
    fn record_hook_rewrite(&self, turn: u32, step: u32, phase: &str, changed: &[String]) {
        if changed.is_empty() {
            return;
        }
        let _ = self.emit(
            turn,
            step,
            (
                EventType::HOOK_REWRITE,
                json!({"phase": phase, "hook": changed.join(","), "changed": changed.len()}),
            ),
        );
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
            // Steering, two ways, in that order.
            //
            // First the durable queue: a `next-step` row was written by someone
            // who may not be this process at all, so claiming it here and
            // recording what the model saw is the only way the delivery can
            // survive a restart. The row becomes a `user/message` with
            // `steering: true`, which is also what the next turn rebuilds
            // history from — so a steered prompt is not a one-turn ghost.
            //
            // Then the in-memory hook, kept for callers that push text they
            // already recorded themselves (the HTTP layer does this). It stays
            // history-only, as it always was.
            if index > 0 {
                loop {
                    match self.claim(Target::NextStep) {
                        Ok(Some(msg)) => {
                            if msg.text.trim().is_empty() {
                                continue;
                            }
                            let payload = json!({"text": msg.text, "steering": true});
                            if let Err(e) =
                                self.emit(turn, index as u32, (EventType::USER_MESSAGE, payload))
                            {
                                return StepsOut {
                                    count: index,
                                    stop: STOP_ERROR,
                                    text,
                                    error: Some(e),
                                };
                            }
                            history.push(Message::user(&msg.text));
                        }
                        Ok(None) => break,
                        Err(e) => {
                            return StepsOut {
                                count: index,
                                stop: STOP_ERROR,
                                text,
                                error: Some(e),
                            }
                        }
                    }
                }
                if let Some(steer) = &self.opt.steering {
                    for s in steer() {
                        if !s.trim().is_empty() {
                            history.push(Message::user(s));
                        }
                    }
                }
            }

            let step_no = index as u32; // 0-based payload index, same as step()
            let out = self.step(turn, step_no, history);
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

        // Seam 1 (`pre-step`): the message list the provider is about to receive.
        // The rewrite is transient — it shapes this request, it does not edit the
        // durable history, or a redaction would compound on every later step. No
        // hook installed => `history.to_vec()` unchanged, byte-for-byte the loop
        // this crate shipped before.
        let mut messages = history.to_vec();
        if !self.opt.hooks.is_empty() {
            match self.opt.hooks.run_pre_step(turn, index, messages) {
                Ok((next, changed)) => {
                    self.record_hook_rewrite(turn, step_no, "pre-step", &changed);
                    messages = next;
                }
                Err(e) => {
                    self.record_error(e.message(), "hook_failed", false);
                    let _ = self.emit(turn, step_no, step_end(index, STOP_ERROR));
                    return StepOut {
                        text: String::new(),
                        stop: STOP_ERROR,
                        calls: Vec::new(),
                        error: Some(e),
                    };
                }
            }
        }

        let mut req = Request {
            model: self.opt.model.clone(),
            messages,
            tools: specs,
            ..Request::default()
        };

        // Seam 2 (`request`): the fully built request, before it is sent. Only
        // messages and tool descriptions may change; the model id is restored
        // inside `run_request`, so a hook cannot pick a model.
        if !self.opt.hooks.is_empty() {
            match self.opt.hooks.run_request(turn, index, req) {
                Ok((next, changed)) => {
                    self.record_hook_rewrite(turn, step_no, "request", &changed);
                    req = next;
                }
                Err(e) => {
                    self.record_error(e.message(), "hook_failed", false);
                    let _ = self.emit(turn, step_no, step_end(index, STOP_ERROR));
                    return StepOut {
                        text: String::new(),
                        stop: STOP_ERROR,
                        calls: Vec::new(),
                        error: Some(e),
                    };
                }
            }
        }

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
        // One row per request built, before it goes out. The reason names why a
        // fresh request exists at all: adopting an HTTP-recorded turn, the tool
        // set moving, the first step, or just the next step of the same series.
        let reason = self.request_reason(step_no, &names);
        let _ = self.emit(
            turn,
            step_no,
            request_header(&reason, &self.opt.model, req.messages.len(), &names),
        );
        // What the request carries, as opposed to why it exists. The system
        // prompt is fingerprinted rather than repeated: it is already on
        // `session/start`, and a per-step copy would be the largest row in the
        // log for the smallest amount of new information.
        let _ = self.emit(
            turn,
            step_no,
            request_context(
                &self.opt.provider_name,
                &self.opt.model,
                req.messages.len(),
                req.tools.len(),
                req.temperature,
                req.max_tokens,
                &req.stop,
                self.opt.system.len(),
                &self.system_fingerprint,
            ),
        );
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

    /// Why this request exists. `resume` when the loop is adopting a turn the
    /// HTTP layer already recorded, `change` when the tool set differs from the
    /// previous request, `initial` for the first step of a turn, `series` for
    /// every step after that. The answer is a pure function of what this agent
    /// has already sent, so replaying a log gives the same words.
    fn request_reason(&self, step_no: u32, names: &[String]) -> String {
        let mut last = match self.last_tools.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let moved = *last != names;
        let reason = if step_no == 1 && self.opt.prompt_recorded {
            "resume"
        } else if moved && !last.is_empty() {
            "change"
        } else if step_no == 1 {
            "initial"
        } else {
            "series"
        };
        *last = names.to_vec();
        reason.to_string()
    }

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
                        self.record_attempt(attempt, "cancelled", &e);
                        return StreamOut {
                            text,
                            usage,
                            calls: Vec::new(),
                            stop: STOP_CANCELLED,
                            error: Some(e),
                        };
                    }
                    // Transport policy: retry a retryable failure before any text.
                    if retryable_of(&e) && text.is_empty() && attempt < self.opt.max_retries {
                        self.record_attempt(attempt, "retry", &e);
                        self.wait_retry(&e);
                        continue;
                    }
                    // Opt-in override: a `request-error` hook may know a failure is
                    // transient when its code does not say so.
                    match self.hooks_request_retry(attempt, &e) {
                        Ok(true) => {
                            self.record_attempt(attempt, "retry", &e);
                            self.wait_retry(&e);
                            continue;
                        }
                        Ok(false) => {}
                        Err(hook_err) => {
                            self.record_attempt(attempt, "error", &e);
                            self.record_error_from(&hook_err);
                            return StreamOut {
                                text,
                                usage,
                                calls: Vec::new(),
                                stop: STOP_ERROR,
                                error: Some(hook_err),
                            };
                        }
                    }
                    self.record_attempt(attempt, "error", &e);
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

            let consumed = self.consume(stream, &mut agg, &mut usage);
            text.push_str(&consumed.text);
            if consumed.cancelled {
                let err = cancelled_error();
                self.record_attempt(attempt, "cancelled", &err);
                return StreamOut {
                    text,
                    usage,
                    calls: Vec::new(),
                    stop: STOP_CANCELLED,
                    error: Some(err),
                };
            }
            if let Some(e) = consumed.error {
                // Only retry before the first text fragment; a retry after text
                // would duplicate that fragment in the log.
                if retryable_of(&e) && text.is_empty() && attempt < self.opt.max_retries {
                    self.record_attempt(attempt, "retry", &e);
                    self.wait_retry(&e);
                    continue;
                }
                // A retry after text would duplicate it, so the hook is consulted
                // only when nothing has been streamed yet.
                if text.is_empty() {
                    match self.hooks_request_retry(attempt, &e) {
                        Ok(true) => {
                            self.record_attempt(attempt, "retry", &e);
                            self.wait_retry(&e);
                            continue;
                        }
                        Ok(false) => {}
                        Err(hook_err) => {
                            self.record_attempt(attempt, "error", &e);
                            self.record_error_from(&hook_err);
                            return StreamOut {
                                text,
                                usage,
                                calls: Vec::new(),
                                stop: STOP_ERROR,
                                error: Some(hook_err),
                            };
                        }
                    }
                }
                self.record_attempt(attempt, "error", &e);
                self.record_error_from(&e);
                return StreamOut {
                    text,
                    usage,
                    calls: Vec::new(),
                    stop: STOP_ERROR,
                    error: Some(e),
                };
            }
            let calls = match agg.finish() {
                Ok(c) => c,
                Err(e) => {
                    self.record_attempt(attempt, "error", &e);
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

    /// One row for a provider attempt that did not become the visible answer.
    /// `outcome` is `retry` (the loop will try again) or `error` (it will not).
    /// The successful attempt writes no row of this type: it is the single
    /// `assistant/message` the step records.
    fn record_attempt(&self, attempt: usize, outcome: &str, err: &Error) {
        let _ = self.emit(
            self.cur_turn.load(Ordering::SeqCst),
            self.cur_step.load(Ordering::SeqCst),
            assistant_attempt(
                attempt as u32,
                outcome,
                err.message(),
                code_of(err),
                retryable_of(err),
            ),
        );
    }

    /// Consult a `request-error` hook for a retry the transport policy refused.
    /// Inert (`false`) when no hook is installed or the retry budget is spent, so
    /// the loop's behaviour with no hook is exactly the policy above.
    fn hooks_request_retry(&self, attempt: usize, err: &Error) -> hearth_core::Result<bool> {
        if self.opt.hooks.is_empty() || attempt >= self.opt.max_retries {
            return Ok(false);
        }
        self.opt.hooks.run_request_error(
            self.cur_turn.load(Ordering::SeqCst),
            self.cur_step.load(Ordering::SeqCst),
            attempt as u32,
            err.message(),
        )
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
            // A tool may ask the loop to write a record it is not allowed to
            // write itself. `compress` is the only such tool today: it knows the
            // range and the summary, and the loop knows how to append.
            if let Some(meta) = &res.meta {
                if let Some(block) = meta
                    .get(CONTEXT_BLOCK_META)
                    .filter(|value| value.is_object())
                {
                    self.emit(turn, step, (EventType::CONTEXT_COMPRESSED, block.clone()))?;
                }
                if let Some(row) = meta
                    .get(SUBAGENT_CATALOG_META)
                    .filter(|value| value.is_object())
                {
                    self.emit(turn, step, (EventType::SUBAGENT_CATALOG, row.clone()))?;
                }
            }
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
