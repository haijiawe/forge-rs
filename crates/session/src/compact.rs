//! Context compaction (port of `archive/forge-go/internal/session/compact.go` and
//! `compact_wire.go`).
//!
//! Compaction replaces the oldest complete history with one summary record. The
//! log is append-only, so nothing is rewritten: a `compaction/summary` event
//! declares the seq range it stands in for, and every reader folds the log
//! through it.
//!
//! The module is split the same way the Go original is, and for the same reason:
//! [`plan_compaction`] is a pure selector that decides *what* could be replaced,
//! and [`compact`] is a writer that only ever appends one record. The rules exist
//! to protect a transcript — a cut inside a turn, or between a tool call and its
//! result, produces a projection a UI cannot render and a resume path that
//! misattributes work. A pure selector makes each rule individually testable, and
//! a writer that only appends makes it impossible for compaction to corrupt
//! history even when handed a bad plan.

use std::collections::HashMap;

use forge_core::{Error, ErrorKind, Event, EventType, Result};
use serde_json::{json, Value};

use crate::store::Store;

/// A plan's range already overlaps a range an earlier summary replaced.
/// Appending a second summary for history that is already hidden is a bug rather
/// than idempotency, so the write is refused and nothing is appended.
pub const ERR_ALREADY_COMPACTED: &str = "session: range already compacted";

/// A structurally impossible plan: a zero `from`, a `to` before `from`, or a
/// `to` naming a seq the log does not have yet. Such a plan cannot describe real
/// history, and guessing at the caller's intent would hide the wrong events.
pub const ERR_INVALID_PLAN: &str = "session: invalid compaction plan";

/// An event slice that cannot be reasoned about: a zero seq, or seqs that do not
/// ascend. The store never produces such a slice, so it is a caller bug, and a
/// guess would cut the wrong range.
pub const ERR_INVALID_LOG: &str = "session: invalid event log for compaction";

/// A compaction whose summary carries no text. An empty summary hides the
/// conversation and replaces it with nothing, so it is refused rather than
/// appended.
pub const ERR_EMPTY_SUMMARY: &str = "session: compaction summary is empty";

/// How many of the most recent turns [`plan_compaction`] keeps verbatim when the
/// caller passes 0. Two is the current turn plus the one before it: the pair a
/// follow-up question almost always refers to.
pub const DEFAULT_KEEP_TURNS: usize = 2;

/// The size budget [`TruncatingSummarizer`] uses when its budget is unset: large
/// enough to keep the shape of a long history, small enough that the summary
/// never becomes the context problem it was meant to solve.
pub const DEFAULT_SUMMARY_BYTES: usize = 2048;

/// Caps one event's excerpt inside a generated summary, so a single 64 KiB tool
/// result cannot consume the whole budget and hide the rest of the history.
const SNIPPET_BYTES: usize = 120;

/// `forge_core::Error` carries no downcast target, so the four refusals are
/// identified by the message each one starts with — the same strings the Go
/// sentinels use, which is what lets a ported test read either implementation.
pub fn is_already_compacted(err: &Error) -> bool {
    err.message().starts_with(ERR_ALREADY_COMPACTED)
}

pub fn is_invalid_plan(err: &Error) -> bool {
    err.message().starts_with(ERR_INVALID_PLAN)
}

pub fn is_invalid_log(err: &Error) -> bool {
    err.message().starts_with(ERR_INVALID_LOG)
}

pub fn is_empty_summary(err: &Error) -> bool {
    err.message().starts_with(ERR_EMPTY_SUMMARY)
}

/// Names the seq range a compaction replaces. `from` and `to` are inclusive, so
/// `[from, to]` is exactly the set of events whose content the summary stands in
/// for. `reason` is for humans and operator logs; it is never persisted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactPlan {
    pub from: u64,
    pub to: u64,
    pub reason: String,
}

impl CompactPlan {
    pub fn from(&self) -> u64 {
        self.from
    }

    pub fn to(&self) -> u64 {
        self.to
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// A zero plan means "nothing worth compacting", not failure.
    pub fn is_empty(&self) -> bool {
        self.to == 0
    }
}

/// An inclusive seq range. Compaction reasons in ranges rather than slice
/// offsets because the log is the durable artifact: a range still means the same
/// thing after a replay, an index does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeqSpan {
    from: u64,
    to: u64,
}

impl SeqSpan {
    fn new(from: u64, to: u64) -> SeqSpan {
        SeqSpan { from, to }
    }

    /// Whether two inclusive ranges share at least one seq.
    fn overlaps(self, other: SeqSpan) -> bool {
        self.from <= other.to && other.from <= self.to
    }
}

/// One turn's inclusive seq range. `closed` reports that a `turn/end` closed it:
/// an open block is the live tail of the session, so it is never a compaction
/// boundary.
#[derive(Debug, Clone, Copy)]
struct TurnBlock {
    span: SeqSpan,
    closed: bool,
}

/// Selects the oldest complete history that can be replaced by a summary, keeping
/// the most recent `keep_turns` turns verbatim (0 means [`DEFAULT_KEEP_TURNS`]).
///
/// A zero plan is not a failure: a short session, a session whose turns are all
/// inside the kept window, and a log whose tool pairs straddle every candidate
/// boundary are all normal states. Callers use [`CompactPlan::is_empty`] to decide
/// whether to call [`compact`] at all.
///
/// The rules, in the order they are applied:
///
/// 1. Turn atomicity. A turn runs from `turn/start` to `turn/end`, so the range
///    may end only at a `turn/end`, never inside one.
/// 2. Tool pairing. A `tool/call` and its `tool/result` are one unit; a call whose
///    result lands in a later turn glues the turns together.
/// 3. Already-compacted ranges are never re-included.
/// 4. The most recent `keep_turns` turns stay verbatim.
///
/// The function is pure: it reads the slice, returns a value, performs no I/O and
/// keeps no state.
pub fn plan_compaction(events: &[Event], keep_turns: usize) -> Result<CompactPlan> {
    let keep_turns = if keep_turns == 0 {
        DEFAULT_KEEP_TURNS
    } else {
        keep_turns
    };

    // Validate before reasoning: every later rule assumes log order, and a slice
    // out of order would produce a plan naming the wrong events.
    let mut prev = 0u64;
    for (i, event) in events.iter().enumerate() {
        if event.seq == 0 {
            return Err(Error::new(
                ErrorKind::NotDense,
                format!("{ERR_INVALID_LOG}: event at index {i} has seq 0"),
            ));
        }
        if event.seq <= prev {
            return Err(Error::new(
                ErrorKind::NotDense,
                format!(
                    "{ERR_INVALID_LOG}: seq {} at index {i} does not follow {prev}",
                    event.seq
                ),
            ));
        }
        prev = event.seq;
    }
    let Some(last) = events.last() else {
        return Ok(CompactPlan::default());
    };
    let last_seq = last.seq;

    // Rule 3: everything up to the last summary is already replaced.
    let mut settled = 0u64;
    for event in events {
        if event.typ != EventType::COMPACTION_SUMMARY.as_str() {
            continue;
        }
        let (_, end) = replaced_span(event);
        if end > settled {
            settled = end;
        }
    }
    let mut from = settled.saturating_add(1);
    if from > last_seq {
        return Ok(CompactPlan::default()); // the whole log is already summarized
    }

    let (turns, mut spans) = turn_blocks(events);

    // Rule 2: tool pairs widen the atomic units.
    spans.extend(tool_pair_spans(events));
    let atomic = merge_spans(spans);

    // A plan must also *start* on an atomic boundary: a range that begins inside a
    // turn would split it just as surely as one that ends there.
    from = advance_boundary(from, &atomic);

    // Rule 4: the last keep_turns turns are off limits, and the cut is the end of
    // the newest turn that is still eligible. The arithmetic is signed because
    // `keep_turns` can exceed the number of turns, and the Go original then walks
    // to a negative index and returns the zero plan.
    let eligible = turns.len() as isize - keep_turns as isize;
    let mut idx = eligible - 1;
    while idx >= 0 && !turns[idx as usize].closed {
        idx -= 1; // an unclosed turn can never be a boundary; look further back
    }
    if idx < 0 {
        return Ok(CompactPlan::default());
    }

    // A candidate cut that would split an atomic unit retreats to the start of
    // that unit. It never advances: advancing would compact a turn the caller
    // asked to keep.
    let cut = retreat_cut(turns[idx as usize].span.to, &atomic);
    if cut < from {
        return Ok(CompactPlan::default());
    }
    // A range that holds no complete turn replaces nothing: it would swap a
    // session/start or a stray event for a summary and shrink no conversation.
    if !covers_turn(&turns, from, cut) {
        return Ok(CompactPlan::default());
    }
    Ok(CompactPlan {
        from,
        to: cut,
        reason: format!(
            "replace seq {from}..{cut} with a summary; keep the last {keep_turns} of {} turns verbatim",
            turns.len()
        ),
    })
}

/// Splits the log into turn blocks (in order) and the atomic spans they cover.
/// Events that belong to no turn — `session/start`, a session-level error, a
/// compaction marker — are not blocks: a cut is always allowed on either side of
/// them.
fn turn_blocks(events: &[Event]) -> (Vec<TurnBlock>, Vec<SeqSpan>) {
    let mut blocks: Vec<TurnBlock> = Vec::new();
    let mut spans: Vec<SeqSpan> = Vec::new();
    let mut current = SeqSpan::new(0, 0);
    let mut open = false;

    for (i, event) in events.iter().enumerate() {
        let typ = event.typ.as_str();
        if typ == EventType::TURN_START.as_str() {
            if open {
                // A new turn while the previous one is still open means its
                // turn/end is missing. Keep the block so no cut can land inside
                // it, but mark it unclosed so it is never chosen as a boundary.
                let last = events[i.saturating_sub(1)].seq;
                current.to = last;
                blocks.push(TurnBlock {
                    span: current,
                    closed: false,
                });
                spans.push(current);
            }
            current = SeqSpan::new(event.seq, event.seq);
            open = true;
        } else if typ == EventType::TURN_END.as_str() {
            if open {
                current.to = event.seq;
                blocks.push(TurnBlock {
                    span: current,
                    closed: true,
                });
                spans.push(current);
                open = false;
            } else {
                // A turn/end whose turn/start is lost (a log tail): the projection
                // synthesises a turn from it, so it is a one-event turn here too.
                let span = SeqSpan::new(event.seq, event.seq);
                blocks.push(TurnBlock { span, closed: true });
                spans.push(span);
            }
        } else if open {
            current.to = event.seq;
        } else if event.turn != 0 {
            // Mid-turn start: the log begins inside a turn, so the first event
            // carrying a turn number opens an implicit block.
            current = SeqSpan::new(event.seq, event.seq);
            open = true;
        }
    }
    if open {
        if let Some(last) = events.last() {
            current.to = last.seq;
            blocks.push(TurnBlock {
                span: current,
                closed: false,
            });
            spans.push(current);
        }
    }
    (blocks, spans)
}

/// One span per tool call that has a matching result. The span is a single atomic
/// unit: compacting the call while keeping the result, or the reverse, leaves the
/// projection holding half of a fact.
///
/// Calls are keyed by the same alias precedence the projection uses
/// (`callId`/`call_id`/`id`), so the selector and the view can never disagree
/// about which result belongs to which call.
fn tool_pair_spans(events: &[Event]) -> Vec<SeqSpan> {
    let mut spans: Vec<SeqSpan> = Vec::new();
    let mut calls: HashMap<String, u64> = HashMap::new();
    for event in events {
        let typ = event.typ.as_str();
        if typ == EventType::TOOL_CALL.as_str() {
            let Some(id) = payload_call_id(event.payload.as_ref()) else {
                continue; // an unreadable call can never be paired
            };
            calls.entry(id).or_insert(event.seq);
        } else if typ == EventType::TOOL_RESULT.as_str() {
            let Some(id) = payload_call_id(event.payload.as_ref()) else {
                continue;
            };
            if let Some(&call_seq) = calls.get(&id) {
                spans.push(SeqSpan::new(call_seq, event.seq));
            }
        }
    }
    spans
}

/// The `callId`/`call_id`/`id` alias chain, first non-empty wins — the same order
/// `projection` reads them in.
fn payload_call_id(payload: Option<&Value>) -> Option<String> {
    let object = payload?.as_object()?;
    for key in ["callId", "call_id", "id"] {
        if let Some(text) = object.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

/// Flattens overlapping spans into disjoint ones, sorted by start. Merely adjacent
/// spans are deliberately not merged: two spans that touch still leave a legal cut
/// between them, and merging would throw away usable history.
fn merge_spans(mut spans: Vec<SeqSpan>) -> Vec<SeqSpan> {
    if spans.len() < 2 {
        return spans;
    }
    spans.sort_by_key(|span| span.from);
    let mut out: Vec<SeqSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match out.last_mut() {
            Some(last) if span.from <= last.to => {
                if span.to > last.to {
                    last.to = span.to;
                }
            }
            _ => out.push(span),
        }
    }
    out
}

/// Whether `[from, to]` holds at least one whole closed turn. A range without one
/// is not worth compacting, and is the shape a greedy cut takes when tool pairs
/// have glued the whole history to the kept tail.
fn covers_turn(turns: &[TurnBlock], from: u64, to: u64) -> bool {
    turns
        .iter()
        .any(|block| block.closed && block.span.from >= from && block.span.to <= to)
}

/// Moves `seq` forward past any atomic span it falls inside, so a plan never
/// starts in the middle of a turn or of a call/result pair. Returns the smallest
/// boundary at or after `seq`.
fn advance_boundary(mut seq: u64, atomic: &[SeqSpan]) -> u64 {
    loop {
        let mut moved = false;
        for span in atomic {
            if seq > span.from && seq <= span.to {
                seq = span.to + 1;
                moved = true;
            }
        }
        if !moved {
            return seq;
        }
    }
}

/// Returns `cut`, or the last boundary before it when `cut` would split an atomic
/// span. It only ever moves backwards, because moving forward would compact a turn
/// the keep-turns rule protects.
fn retreat_cut(mut cut: u64, atomic: &[SeqSpan]) -> u64 {
    while cut > 0 {
        let mut moved = false;
        for span in atomic {
            if cut >= span.from && cut < span.to {
                cut = span.from.saturating_sub(1);
                moved = true;
            }
        }
        if !moved {
            return cut;
        }
    }
    cut
}

/// Reads the seq range a compaction event stands in for: the declared range from
/// its payload, and the boundary at or before which history counts as summarized.
///
/// A summary with no readable `replaces` range — hand-written, or written by a
/// future producer that spells the range differently — is treated as covering
/// everything up to the marker itself. That is the strongest claim the event can
/// be making, and assuming less would let a later plan re-compact history the
/// summary already stands in for. The marker's own seq is included in `settled` so
/// a new plan never re-includes its predecessor.
fn replaced_span(event: &Event) -> (SeqSpan, u64) {
    let declared = event
        .payload
        .as_ref()
        .and_then(|payload| payload.get("replaces"))
        .and_then(|replaces| {
            let from = replaces.get("from").and_then(Value::as_u64)?;
            let to = replaces.get("to").and_then(Value::as_u64)?;
            Some(SeqSpan::new(from, to))
        })
        .filter(|span| span.to > 0)
        .map(|span| SeqSpan::new(span.from.max(1), span.to))
        .unwrap_or(SeqSpan::new(1, event.seq));
    let settled = declared.to.max(event.seq);
    (declared, settled)
}

/// Appends exactly one `compaction/summary` event that replaces `[plan.from,
/// plan.to]` with `summary`. It never rewrites, truncates or reorders anything: the
/// event goes through [`Store::append`], which assigns the next seq, so an existing
/// record cannot change.
///
/// A plan is refused, appending nothing, when it is structurally impossible, when
/// its `to` names a seq the log does not have yet, when it overlaps a range an
/// earlier summary already replaced, or when the summary is empty. Refusals are
/// safe to retry with a corrected plan because no bytes were written.
///
/// `now_ms` supplies the event timestamp so a test can be deterministic; `None`
/// means "use the wall clock" (the store fills it in). The session id is taken from the
/// log itself: the plan names history, and the log it was computed from already
/// knows which session that history belongs to.
pub fn compact(
    store: &Store,
    plan: &CompactPlan,
    summary: &str,
    now_ms: Option<i64>,
) -> Result<()> {
    if plan.from == 0 || plan.to < plan.from {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("{ERR_INVALID_PLAN}: from={} to={}", plan.from, plan.to),
        ));
    }
    if summary.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::Refused,
            format!(
                "{ERR_EMPTY_SUMMARY}: refusing to replace seq {}..{} with no text",
                plan.from, plan.to
            ),
        ));
    }

    // One pass over the log yields the three facts the write depends on: the
    // session id the event must carry, the last seq the plan may name, and the
    // ranges already replaced.
    let mut replaced: Vec<SeqSpan> = Vec::new();
    let mut last_seq = 0u64;
    let mut session_id = String::new();
    store.read_from(1, |event| {
        if session_id.is_empty() {
            session_id = event.session.clone();
        }
        if event.seq > last_seq {
            last_seq = event.seq;
        }
        if event.typ == EventType::COMPACTION_SUMMARY.as_str() {
            replaced.push(replaced_span(event).0);
        }
        Ok(())
    })?;
    if session_id.is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!(
                "{ERR_INVALID_PLAN}: log {} has no events to compact",
                store.path().display()
            ),
        ));
    }
    if plan.to > last_seq {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!(
                "{ERR_INVALID_PLAN}: to={} is past the log's last seq {last_seq}",
                plan.to
            ),
        ));
    }

    let want = SeqSpan::new(plan.from, plan.to);
    for span in &replaced {
        if want.overlaps(*span) {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "{ERR_ALREADY_COMPACTED}: plan seq {}..{} overlaps the summary covering seq {}..{}",
                    plan.from, plan.to, span.from, span.to
                ),
            ));
        }
    }

    let payload = json!({
        "summary": summary,
        "replaces": {"from": plan.from, "to": plan.to},
    });
    let mut event = Event::new(session_id, EventType::COMPACTION_SUMMARY, Some(payload));
    if let Some(ms) = now_ms {
        event.ts = ms;
    }
    store.append(&event)?;
    Ok(())
}

/// Turns a range of events into the text that will stand in for them.
///
/// The trait is declared here, in the consumer, because compaction is the only
/// thing that needs one: a provider-backed summary (an LLM) is an option, not a
/// dependency, so `forge-session` must not import one to name the shape it wants.
pub trait Summarizer {
    fn summarize(&self, events: &[Event]) -> Result<String>;
}

/// A deterministic, offline [`Summarizer`]: it joins each event's type and a short
/// excerpt of its text, then clips the result to the budget at a UTF-8 rune
/// boundary.
///
/// It exists so that tests and the CLI can compact without a provider and without
/// a network call. It reads no clock, uses no randomness and calls no model, so the
/// same events always produce the same summary — which is what makes a compaction
/// event reproducible in a test.
pub struct TruncatingSummarizer {
    /// Maximum size of the returned summary in bytes. Zero means
    /// [`DEFAULT_SUMMARY_BYTES`].
    pub budget: usize,
}

impl TruncatingSummarizer {
    pub fn new(budget: usize) -> TruncatingSummarizer {
        TruncatingSummarizer { budget }
    }
}

impl Default for TruncatingSummarizer {
    fn default() -> Self {
        TruncatingSummarizer {
            budget: DEFAULT_SUMMARY_BYTES,
        }
    }
}

impl Summarizer for TruncatingSummarizer {
    /// Renders events as one line per event and clips the output to the byte
    /// budget. An event whose payload cannot be decoded still contributes its
    /// type: an unreadable body is forward compatibility to tolerate, not a reason
    /// to drop the fact that the event happened.
    fn summarize(&self, events: &[Event]) -> Result<String> {
        let budget = if self.budget == 0 {
            DEFAULT_SUMMARY_BYTES
        } else {
            self.budget
        };
        let mut out = String::new();
        for event in events {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&event.seq.to_string());
            out.push(' ');
            out.push_str(&event.typ);
            let snippet = event_snippet(event);
            if !snippet.is_empty() {
                out.push_str(": ");
                out.push_str(&snippet);
            }
            // Stop early once the budget is spent: the tail is clipped anyway, and
            // a 10k-event log should not build a megabyte string to throw it away.
            if out.len() >= budget {
                break;
            }
        }
        Ok(clip_bytes(&out, budget))
    }
}

/// A one-line excerpt of an event's payload, using the same field aliases the
/// projection accepts so the summary names what the view shows.
fn event_snippet(event: &Event) -> String {
    let Some(payload) = event.payload.as_ref().and_then(Value::as_object) else {
        return String::new();
    };
    let mut snippet = String::new();
    for key in [
        "text", "content", "summary", "output", "message", "error", "name", "tool", "reason",
    ] {
        if let Some(text) = payload.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                snippet = text.to_string();
                break;
            }
        }
    }
    if snippet.is_empty() {
        return String::new();
    }
    // Collapse whitespace so one event stays one line: a multi-line tool output
    // would otherwise turn the summary into an unreadable block and make the line
    // count depend on payload formatting.
    clip_bytes(&snippet, SNIPPET_BYTES)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// At most `budget` bytes of `s`, cut at a UTF-8 rune boundary so the result is
/// always valid UTF-8, and marked with an ellipsis when truncated so a reader can
/// tell a clipped summary from a short one.
fn clip_bytes(s: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if s.len() <= budget {
        return s.to_string();
    }
    const ELLIPSIS: &str = "…"; // 3 bytes
    if budget > ELLIPSIS.len() {
        let mut cut = budget - ELLIPSIS.len();
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}{ELLIPSIS}", &s[..cut])
    } else {
        let mut cut = budget;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s[..cut].to_string()
    }
}

/// Compacts the log in place when there is something to compact, and reports
/// whether it wrote anything.
///
/// It exists because compaction needs both a [`Store`] (to append the one summary
/// record) and a [`Summarizer`] (to produce its text), and neither the agent loop
/// nor a UI needs to know about the other. A caller hands over both and learns only
/// "did a record land", so a surface can treat compaction as best effort: the log
/// is already correct whether or not this runs.
///
/// `keep_turns` is the verbatim tail the plan must preserve (0 means
/// [`DEFAULT_KEEP_TURNS`]). `min_events` is a floor on the log length: below it the
/// whole log is still the conversation the model should see, so planning is
/// skipped.
pub fn compact_log(
    store: &Store,
    summarizer: &dyn Summarizer,
    keep_turns: usize,
    min_events: u64,
) -> Result<bool> {
    // The plan is a pure function of the log, so the log is read once and the
    // summary is built from the same snapshot that was planned. Reading twice could
    // plan a range the second read no longer sees.
    let mut events: Vec<Event> = Vec::new();
    store.read_from(1, |event| {
        events.push(event.clone());
        Ok(())
    })?;

    // Below the floor nothing worthwhile is compactable: the planner keeps the
    // recent tail verbatim, so a short log would lose at most a turn or two while
    // paying for a provider call and appending a record that can never be removed
    // (append-only).
    if (events.len() as u64) < min_events {
        return Ok(false);
    }

    let plan = plan_compaction(&events, keep_turns)?;
    if plan.is_empty() {
        // The log is empty, already summarized, or the recent turns are all that is
        // left. That is an answer, not a failure, and it must not reach the caller
        // as an error or every idle turn would report a broken compaction.
        return Ok(false);
    }

    // Exactly the events the plan covers, in log order: the summary replaces that
    // range and nothing else, so handing it the whole log (or the tail) would
    // describe events the record does not claim.
    let covered: Vec<Event> = events
        .iter()
        .filter(|event| event.seq >= plan.from && event.seq <= plan.to)
        .cloned()
        .collect();

    // No fallback text: a summary that is not the model's words would become the
    // permanent record of what those events said.
    let summary = summarizer.summarize(&covered)?;

    // A real clock, never zero: the summary is a normal log record, and a zero ts
    // would make it the first event on every timestamp-ordered reader.
    match compact(store, &plan, &summary, None) {
        Ok(()) => Ok(true),
        // The plan and the append are not one atomic step: another compactor can
        // write a summary for the same range between our read and our append. That
        // is the "already done" outcome — an expected race, not a broken log, so the
        // caller must not fail the turn over it.
        Err(err) if is_already_compacted(&err) => Ok(false),
        Err(err) => Err(Error::new(
            err.kind(),
            format!("session: compact log {}: {}", store.path().display(), err),
        )),
    }
}

/// The seq range the newest compaction summary in the log replaces, for telling an
/// operator how much history one compaction folded.
///
/// The range is read back from the record rather than taken from the plan that
/// produced it: the number a caller prints must be the one a reader will honour,
/// and a second compactor may have won the append.
pub fn last_compacted_span(store: &Store) -> Option<(u64, u64)> {
    let mut found: Option<(u64, u64)> = None;
    let read = store.read_from(1, |event| {
        if event.typ != EventType::COMPACTION_SUMMARY.as_str() {
            return Ok(());
        }
        let Some(payload) = event.payload.as_ref() else {
            return Ok(());
        };
        let Some(replaces) = payload.get("replaces") else {
            return Ok(());
        };
        let (Some(from), Some(to)) = (
            replaces.get("from").and_then(Value::as_u64),
            replaces.get("to").and_then(Value::as_u64),
        ) else {
            // Undecodable payloads belong to a log this build cannot fully read;
            // the previous summary is still a truthful answer.
            return Ok(());
        };
        // Log order is append order, so the last summary seen wins.
        found = Some((from, to));
        Ok(())
    });
    match read {
        Ok(()) => found,
        Err(_) => None,
    }
}
