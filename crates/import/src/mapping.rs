//! Record-by-record mapping from the old vocabulary to v1 events.
//!
//! One compiler produces both `MakePlan` and the bytes `Write` commits, so a
//! plan can never describe a different log than the one written. Every old
//! record either produces an event or a named `dropped` reason; degradation
//! (an event that loses information) is recorded as a `degraded:` reason so
//! the caller can see it without having to read the new log.

use std::collections::{HashMap, HashSet};

use hearth_core::event::FORMAT_VERSION;
use hearth_core::json::encode;
use hearth_core::{new_id, Error, ErrorKind, Event, EventType, Result};
use ring::digest::{Context, SHA256};
use serde_json::{json, Map, Value};

use crate::detect::{locate, OldHeader, OldRecord, Source};
use crate::jsonx::{
    clip_bytes, decode_json_string, first_string, has_block_type, join_text, nested, parse_blocks,
    raw_array, raw_bool, raw_i64, raw_string,
};

const MAX_SYSTEM_BYTES: usize = 8 << 10;
const MAX_TOOL_OUTPUT: usize = 64 << 10;
const STOP_END_TURN: &str = "end_turn";
const STOP_TOOL_USE: &str = "tool_use";

type Payload = Map<String, Value>;

/// What `MakePlan` returns and `Write` consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The new session id; also the file stem of the imported log.
    pub session_id: String,
    /// Number of v1 events the writer will commit.
    pub events: usize,
    /// Dropped and degraded reasons, collapsed with counts, deterministic.
    pub dropped: Vec<String>,
}

/// Plan the import of `src` without writing anything.
pub fn make_plan(src: &Source) -> Result<Plan> {
    if src.dir.as_os_str().is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            "empty source directory: call Detect before MakePlan",
        ));
    }
    let session_id = new_id()?;
    let (events, dropped) = compile(&src.dir, &session_id)?;
    Ok(Plan {
        session_id,
        events: events.len(),
        dropped,
    })
}

/// Contract alias for [`make_plan`].
#[allow(non_snake_case)]
pub fn MakePlan(src: &Source) -> Result<Plan> {
    make_plan(src)
}

/// Rebuild the events a plan describes. `Write` uses this to prove the plan is
/// still current before it commits anything.
pub(crate) fn events_for(src: &Source, session_id: &str) -> Result<(Vec<Event>, Vec<String>)> {
    if src.dir.as_os_str().is_empty() {
        return Err(Error::new(
            ErrorKind::Invalid,
            "empty source directory: call Detect before Write",
        ));
    }
    compile(&src.dir, session_id)
}

fn compile(dir: &std::path::Path, session_id: &str) -> Result<(Vec<Event>, Vec<String>)> {
    if session_id.is_empty() {
        return Err(Error::new(ErrorKind::Invalid, "empty session id"));
    }
    let located = locate(dir)?;
    let mut compiler = Compiler {
        hdr: located.header,
        sess_id: session_id.to_owned(),
        recs: located.records,
        evs: Vec::new(),
        dropped: Vec::new(),
        prev_ts: 0,
        cur_turn: 0,
        cur_step: 0,
        turn_step: HashMap::new(),
        step_stop: HashMap::new(),
        tool_name: HashMap::new(),
        explicit: HashSet::new(),
        model: String::new(),
        system: String::new(),
    };
    compiler.collect_facts();
    compiler.emit_session_start()?;
    // Clone the rows so the mapping methods can borrow self mutably for notes.
    let recs = compiler.recs.clone();
    for (index, record) in recs.iter().enumerate() {
        compiler.map_record(index, record)?;
    }
    let dropped = compiler.drop_reasons();
    Ok((compiler.evs, dropped))
}

struct Compiler {
    hdr: OldHeader,
    sess_id: String,
    recs: Vec<OldRecord>,

    evs: Vec<Event>,
    dropped: Vec<String>,
    prev_ts: i64,

    cur_turn: u32,
    cur_step: u32,
    turn_step: HashMap<u32, usize>,
    step_stop: HashMap<(u32, u32), String>,
    tool_name: HashMap<String, String>,
    explicit: HashSet<String>,
    model: String,
    system: String,
}

impl Compiler {
    fn collect_facts(&mut self) {
        for record in &self.recs {
            let Some(value) = record.data.as_ref() else {
                continue;
            };
            let Some(obj) = value.as_object() else {
                continue;
            };
            match record.typ.as_str() {
                "request/header" => {
                    let Some(header) = nested(obj, "header") else {
                        continue;
                    };
                    if let Some(config) = nested(header, "config") {
                        if self.model.is_empty() {
                            if let Some(model) = raw_string(config, "model") {
                                self.model = model;
                            }
                        }
                    }
                    if self.system.is_empty() {
                        if let Some(text) = raw_string(header, "system") {
                            self.system = text;
                        }
                    }
                }
                "request/context" => {
                    if self.model.is_empty() {
                        if let Some(model) = raw_string(obj, "model") {
                            self.model = model;
                        }
                    }
                }
                "system/message" => {
                    if !self.system.is_empty() {
                        continue;
                    }
                    let Some(message) = nested(obj, "message") else {
                        continue;
                    };
                    let blocks = parse_blocks(message.get("content"));
                    let text = join_text(&blocks);
                    if !text.is_empty() {
                        self.system = text;
                    }
                }
                "assistant/message" => {
                    let Some(message) = nested(obj, "message") else {
                        continue;
                    };
                    if let Some(source) = nested(message, "source") {
                        if self.model.is_empty() {
                            if let Some(model) = raw_string(source, "model") {
                                self.model = model;
                            }
                        }
                    }
                }
                "tool/call" => {
                    let Some(call_id) = call_id_of(obj) else {
                        continue;
                    };
                    self.explicit.insert(call_id.clone());
                    if let Some(name) = raw_string(obj, "name") {
                        self.tool_name.insert(call_id, name);
                    }
                }
                _ => {}
            }
        }
    }

    fn emit_session_start(&mut self) -> Result<()> {
        let mut payload = Payload::new();
        payload.insert("startedBy".into(), json!("import"));
        if !self.hdr.cwd.is_empty() {
            payload.insert("workspace".into(), json!(self.hdr.cwd));
        }
        if !self.model.is_empty() {
            payload.insert("model".into(), json!(self.model));
        }
        if !self.hdr.profile.is_empty() {
            payload.insert("profile".into(), json!(self.hdr.profile));
        }
        if !self.system.is_empty() {
            let original = self.system.len();
            let (system, _) = clip_bytes(&self.system, MAX_SYSTEM_BYTES);
            payload.insert("system".into(), json!(system));
            payload.insert("systemBytes".into(), json!(original));
            if system.len() < original {
                payload.insert("systemTruncated".into(), json!(true));
            }
        }
        let mut ts = self.hdr.created_at;
        if ts <= 0 {
            ts = 1;
        }
        self.prev_ts = ts;
        self.append(EventType::SESSION_START, 0, 0, payload, ts)
    }

    fn map_record(&mut self, idx: usize, record: &OldRecord) -> Result<()> {
        let Some(value) = record.data.as_ref() else {
            self.drop(record, "data is not a JSON object");
            return Ok(());
        };
        let Some(obj) = value.as_object() else {
            self.drop(record, "data is not a JSON object");
            return Ok(());
        };
        match record.typ.as_str() {
            "turn/start" => self.map_turn_start(idx, record, obj),
            "turn/end" => self.map_turn_end(idx, record, obj),
            "step/start" => self.map_step_start(idx, record, obj),
            "step/end" => self.map_step_end(idx, record, obj),
            "user/message" => self.map_user_message(idx, record, obj),
            "system/message" => {
                // v1 has no system-message event; the prompt belongs to
                // session/start.system, so this row is folded and accounted for.
                self.drop(
                    record,
                    "folded into session/start.system; v1 has no system-message event",
                );
                Ok(())
            }
            "assistant/message" => self.map_assistant_message(idx, record, obj),
            "tool/call" => self.map_tool_call(idx, record, obj),
            "tool/result" => self.map_tool_result(idx, record, obj),
            "approval/asked" => self.map_approval_asked(idx, record, obj),
            "approval/decided" => self.map_approval_decided(idx, record, obj),
            "compaction/summary" => self.map_compaction_summary(idx, record, obj),
            "request/header" => self.map_request_header(idx, record, obj),
            _ => {
                self.drop(record, unmapped_reason(&record.typ));
                Ok(())
            }
        }
    }

    fn map_turn_start(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let (turn, _) = self.scope(obj);
        let turn = if turn == 0 { self.cur_turn + 1 } else { turn };
        self.cur_turn = turn;
        self.turn_step.entry(turn).or_insert(0);
        let mut payload = Payload::new();
        payload.insert("number".into(), json!(turn));
        self.emit(idx, record, EventType::TURN_START, turn, 0, payload)
    }

    fn map_turn_end(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let (turn, _) = self.scope(obj);
        let (reason, cancelled_by) = self.turn_end_reason(record, obj);
        let mut payload = Payload::new();
        payload.insert("reason".into(), json!(reason));
        if let Some(steps) = self.turn_step.get(&turn) {
            if *steps > 0 {
                payload.insert("steps".into(), json!(steps));
            }
        }
        if !cancelled_by.is_empty() {
            payload.insert("cancelledBy".into(), json!(cancelled_by));
        }
        self.emit(idx, record, EventType::TURN_END, turn, 0, payload)
    }

    fn turn_end_reason(&mut self, record: &OldRecord, obj: &Payload) -> (String, String) {
        let Some(raw) = obj.get("reason") else {
            self.note(
                record,
                "turn/end has no reason; imported as error rather than guessed as completed",
            );
            return ("error".into(), String::new());
        };
        let mut word = raw_string(obj, "reason").unwrap_or_default();
        if word.is_empty() {
            if let Some(reason_obj) = raw.as_object() {
                word = raw_string(reason_obj, "kind").unwrap_or_default();
                if let Some(nested_reason) = nested(reason_obj, "reason") {
                    if let Some(kind) = raw_string(nested_reason, "kind") {
                        if !kind.is_empty() {
                            word = kind;
                            self.note(
                                record,
                                "turn/end aborted; cancelledBy records the nested abort reason",
                            );
                        }
                    }
                }
            }
        }
        match word.as_str() {
            "completed" => ("completed".into(), String::new()),
            "aborted" | "abort" | "interrupted" | "interrupt" | "forked" | "blocked" | "user"
            | "parent" | "hook" | "disposed" | "legacy" => ("cancelled".into(), word),
            "error" | "max-tokens" | "failed" | "timeout" => ("error".into(), String::new()),
            "" => {
                self.note(
                    record,
                    "turn/end reason has no kind; imported as error rather than guessed as completed",
                );
                ("error".into(), String::new())
            }
            _ => {
                self.note(
                    record,
                    format!("turn/end reason kind {word:?} is not a v1 reason; imported as error"),
                );
                ("error".into(), String::new())
            }
        }
    }

    fn map_step_start(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let (turn, mut step) = self.scope(obj);
        if step == 0 {
            step = self.cur_step + 1;
        }
        self.cur_step = step;
        *self.turn_step.entry(turn).or_insert(0) += 1;
        let mut payload = Payload::new();
        payload.insert("index".into(), json!(step - 1));
        self.emit(idx, record, EventType::STEP_START, turn, step, payload)
    }

    fn map_step_end(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let (turn, step) = self.scope(obj);
        let reason = self
            .step_stop
            .get(&(turn, step))
            .cloned()
            .unwrap_or_else(|| STOP_END_TURN.to_owned());
        let mut payload = Payload::new();
        payload.insert("index".into(), json!(step.saturating_sub(1)));
        payload.insert("reason".into(), json!(reason));
        self.emit(idx, record, EventType::STEP_END, turn, step, payload)
    }

    fn map_user_message(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let blocks = parse_blocks(obj.get("content"));
        let text = join_text(&blocks);
        if text.is_empty() {
            self.drop(
                record,
                "no text block in content; a sections-only user message is not a shape any verified fixture carries",
            );
            return Ok(());
        }
        if has_block_type(&blocks, "image") {
            self.note(
                record,
                "non-text content blocks (e.g. image) are not representable in v1 and were dropped",
            );
        }
        let mut payload = Payload::new();
        payload.insert("text".into(), json!(text));
        payload.insert("attachments".into(), json!([]));
        let turn = self.cur_turn;
        self.emit(idx, record, EventType::USER_MESSAGE, turn, 0, payload)
    }

    fn map_assistant_message(
        &mut self,
        idx: usize,
        record: &OldRecord,
        obj: &Payload,
    ) -> Result<()> {
        let (turn, step) = self.scope(obj);
        let Some(message) = nested(obj, "message") else {
            self.drop(record, "no message object");
            return Ok(());
        };
        let blocks = parse_blocks(message.get("content"));
        let mut stop_reason = STOP_END_TURN.to_owned();
        if has_block_type(&blocks, "tool-call") {
            stop_reason = STOP_TOOL_USE.to_owned();
        }
        let mut payload = Payload::new();
        payload.insert("text".into(), json!(join_text(&blocks)));
        payload.insert("stopReason".into(), json!(stop_reason));
        if let Some(usage) = usage_payload(obj.get("usage")) {
            payload.insert("usage".into(), usage);
        }
        if let Some(source) = nested(message, "source") {
            if let Some(model) = raw_string(source, "model") {
                if !model.is_empty() {
                    payload.insert("model".into(), json!(model));
                }
            }
        }
        self.emit(
            idx,
            record,
            EventType::ASSISTANT_MESSAGE,
            turn,
            step,
            payload,
        )?;
        self.step_stop.insert((turn, step), stop_reason);

        for block in &blocks {
            if block.typ != "tool-call" || block.id.is_empty() || self.explicit.contains(&block.id)
            {
                continue;
            }
            let mut call = Payload::new();
            call.insert("callId".into(), json!(block.id));
            call.insert("tool".into(), json!(block.name));
            if let Some(args) = decode_json_string(&block.args) {
                if args.is_object() {
                    call.insert("args".into(), args);
                }
            }
            self.note(
                record,
                format!(
                    "tool call {} had no tool/call record and was reconstructed from the assistant message",
                    block.id
                ),
            );
            self.emit(idx, record, EventType::TOOL_CALL, turn, step, call)?;
        }
        Ok(())
    }

    fn map_tool_call(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let Some(call_id) = call_id_of(obj) else {
            self.drop(record, "no callId");
            return Ok(());
        };
        let mut payload = Payload::new();
        payload.insert("callId".into(), json!(call_id));
        if let Some(name) = raw_string(obj, "name") {
            if !name.is_empty() {
                payload.insert("tool".into(), json!(name));
            }
        }
        if let Some(args_text) = raw_string(obj, "arguments") {
            if !args_text.is_empty() {
                if let Some(args) = decode_json_string(&args_text) {
                    if args.is_object() {
                        payload.insert("args".into(), args);
                    } else {
                        self.note(
                            record,
                            "arguments is not a JSON object; args omitted rather than reshaped",
                        );
                    }
                } else {
                    self.note(
                        record,
                        "arguments is not a JSON object; args omitted rather than reshaped",
                    );
                }
            }
        }
        let (turn, step) = self.scope(obj);
        self.emit(idx, record, EventType::TOOL_CALL, turn, step, payload)
    }

    fn map_tool_result(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let Some(message) = nested(obj, "message") else {
            self.drop(record, "no message object");
            return Ok(());
        };
        let blocks = parse_blocks(message.get("content"));
        let (call_id, output, ok_result) = if blocks.first().is_some_and(|b| b.typ == "tool-result")
        {
            let wrapper = &blocks[0];
            let mut call_id = wrapper.tool_call_id.clone();
            if call_id.is_empty() {
                if let Some(source) = nested(message, "source") {
                    call_id = raw_string(source, "callId").unwrap_or_default();
                }
            }
            let nested_blocks = parse_blocks(wrapper.content.as_ref());
            (call_id, join_text(&nested_blocks), !wrapper.is_error)
        } else {
            let mut call_id = raw_string(message, "toolCallId").unwrap_or_default();
            if call_id.is_empty() {
                if let Some(source) = nested(message, "source") {
                    call_id = raw_string(source, "callId").unwrap_or_default();
                }
            }
            let ok_result = raw_bool(message, "isError")
                .map(|value| !value)
                .unwrap_or(true);
            (call_id, join_text(&blocks), ok_result)
        };
        if call_id.is_empty() {
            self.drop(record, "no call id on the result message");
            return Ok(());
        }
        let mut payload = Payload::new();
        payload.insert("callId".into(), json!(call_id));
        payload.insert("ok".into(), json!(ok_result));
        payload.insert("output".into(), json!(output));
        if let Some(name) = self.tool_name.get(&call_id) {
            if !name.is_empty() {
                payload.insert("tool".into(), json!(name));
            }
        }
        if let Some(meta) = obj.get("meta") {
            if !meta.is_null() {
                payload.insert("meta".into(), meta.clone());
            }
        }
        let (clipped, truncated) = clip_bytes(&output, MAX_TOOL_OUTPUT);
        if truncated {
            payload.insert("output".into(), json!(clipped));
            payload.insert("truncated".into(), json!(true));
            self.note(
                record,
                format!(
                    "tool output clipped to {MAX_TOOL_OUTPUT} bytes as the v1 contract requires"
                ),
            );
        }
        let (turn, step) = self.scope(obj);
        self.emit(idx, record, EventType::TOOL_RESULT, turn, step, payload)
    }

    fn map_approval_asked(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let Some(approval_id) = first_string(obj, &["id", "approvalId"]) else {
            self.drop(record, "no approval id");
            return Ok(());
        };
        let mut payload = Payload::new();
        payload.insert("approvalId".into(), json!(approval_id));
        if let Some(call_id) = first_string(obj, &["callId", "call_id"]) {
            payload.insert("callId".into(), json!(call_id));
        }
        if let Some(tool) = first_string(obj, &["toolName", "tool"]) {
            payload.insert("tool".into(), json!(tool));
        }
        if let Some(reason) = first_string(obj, &["reason", "detail"]) {
            payload.insert("reason".into(), json!(reason));
        }
        let (turn, step) = self.scope(obj);
        self.emit(
            idx,
            record,
            EventType::APPROVAL_REQUEST,
            turn,
            step,
            payload,
        )
    }

    fn map_approval_decided(
        &mut self,
        idx: usize,
        record: &OldRecord,
        obj: &Payload,
    ) -> Result<()> {
        let Some(approval_id) = first_string(obj, &["id", "approvalId"]) else {
            self.drop(record, "no approval id");
            return Ok(());
        };
        let outcome = first_string(obj, &["outcome", "decision"]).unwrap_or_default();
        let decision = if outcome.starts_with("allowed") {
            "allow"
        } else if matches!(outcome.as_str(), "rejected" | "denied" | "deny") {
            "deny"
        } else {
            self.drop(record, format!("unknown approval outcome {outcome:?}"));
            return Ok(());
        };
        let mut payload = Payload::new();
        payload.insert("approvalId".into(), json!(approval_id));
        payload.insert("decision".into(), json!(decision));
        if let Some(call_id) = first_string(obj, &["callId", "call_id"]) {
            payload.insert("callId".into(), json!(call_id));
        }
        self.note(
            record,
            "the old record does not name the decider, so approval/decision has no decidedBy",
        );
        let (turn, step) = self.scope(obj);
        self.emit(
            idx,
            record,
            EventType::APPROVAL_DECISION,
            turn,
            step,
            payload,
        )
    }

    fn map_compaction_summary(
        &mut self,
        idx: usize,
        record: &OldRecord,
        obj: &Payload,
    ) -> Result<()> {
        let blocks = parse_blocks(obj.get("summary"));
        let mut text = join_text(&blocks);
        if text.is_empty() {
            text = raw_string(obj, "summary").unwrap_or_default();
        }
        if text.is_empty() {
            self.drop(record, "no summary text");
            return Ok(());
        }
        // The old `replaces` range addresses old seq numbers, which no longer
        // address the imported log; omitting it is the honest mapping.
        self.note(
            record,
            "replaces omitted: the old shadowed range uses old seq numbers that do not address the imported log",
        );
        let (turn, step) = self.scope(obj);
        let mut payload = Payload::new();
        payload.insert("summary".into(), json!(text));
        self.emit(
            idx,
            record,
            EventType::COMPACTION_SUMMARY,
            turn,
            step,
            payload,
        )
    }

    fn map_request_header(&mut self, idx: usize, record: &OldRecord, obj: &Payload) -> Result<()> {
        let Some(header) = nested(obj, "header") else {
            self.drop(record, "no header object");
            return Ok(());
        };
        let model = nested(header, "config")
            .and_then(|config| raw_string(config, "model"))
            .unwrap_or_default();
        if model.is_empty() {
            self.drop(
                record,
                "no model in header.config; nothing to say as llm/call",
            );
            return Ok(());
        }
        let mut payload = Payload::new();
        payload.insert("model".into(), json!(model));
        if let Some(tools) = raw_array(header, "tools") {
            let names: Vec<String> = tools
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|tool| raw_string(tool, "name").filter(|name| !name.is_empty()))
                .collect::<Vec<_>>();
            if !names.is_empty() {
                payload.insert(
                    "tools".into(),
                    Value::Array(names.into_iter().map(Value::String).collect()),
                );
            }
        }
        self.note(
            record,
            "llm/call carries model and tool names only: the old request/header records no message count or temperature",
        );
        let (turn, step) = self.scope(obj);
        self.emit(idx, record, EventType::LLM_CALL, turn, step, payload)
    }

    fn scope(&self, obj: &Payload) -> (u32, u32) {
        let mut turn = self.cur_turn;
        let mut step = self.cur_step;
        if let Some(value) = raw_i64(obj, "turn") {
            if value > 0 {
                turn = value as u32;
            }
        }
        if let Some(value) = raw_i64(obj, "step") {
            if value > 0 {
                step = value as u32;
            }
        }
        (turn, step)
    }

    fn emit(
        &mut self,
        idx: usize,
        record: &OldRecord,
        typ: EventType,
        turn: u32,
        step: u32,
        payload: Payload,
    ) -> Result<()> {
        let mut ts = record.time;
        if ts <= 0 {
            ts = self.hdr.created_at + idx as i64;
        }
        if ts <= 0 {
            ts = 1;
        }
        if ts <= self.prev_ts {
            ts = self.prev_ts + 1;
        }
        self.prev_ts = ts;
        self.append(typ, turn, step, payload, ts)
    }

    fn append(
        &mut self,
        typ: EventType,
        turn: u32,
        step: u32,
        payload: Payload,
        ts: i64,
    ) -> Result<()> {
        let seq = self.evs.len() as u64 + 1;
        let payload_value = Value::Object(payload);
        let raw = encode(&payload_value);
        let id = event_id(&self.hdr.id, self.hdr.created_at, seq, typ.as_str(), &raw);
        let event = Event {
            v: FORMAT_VERSION,
            seq,
            id,
            ts,
            session: self.sess_id.clone(),
            turn,
            step,
            typ: typ.as_str().to_owned(),
            payload: Some(payload_value),
        };
        event.validate()?;
        self.evs.push(event);
        Ok(())
    }

    fn drop(&mut self, record: &OldRecord, why: impl std::fmt::Display) {
        self.dropped.push(format!(
            "{}: {} (old line {})",
            record.typ, why, record.line
        ));
    }

    fn note(&mut self, record: &OldRecord, why: impl std::fmt::Display) {
        self.dropped.push(format!(
            "degraded: {}: {} (old line {})",
            record.typ, why, record.line
        ));
    }

    fn drop_reasons(&self) -> Vec<String> {
        let mut order = Vec::new();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for reason in &self.dropped {
            let grouped = reason
                .rsplit_once(" (old line ")
                .map(|(prefix, _)| prefix)
                .unwrap_or(reason);
            if !counts.contains_key(grouped) {
                order.push(grouped);
            }
            *counts.entry(grouped).or_insert(0) += 1;
        }
        order
            .into_iter()
            .map(|reason| match counts[reason] {
                1 => reason.to_owned(),
                n => format!("{reason} (x{n})"),
            })
            .collect()
    }
}

/// Stable event id: a SHA-256 of source id, creation time, seq, type and the
/// canonical payload bytes. No clock, no randomness, no dependence on map
/// iteration order.
fn event_id(source_id: &str, created_at: i64, seq: u64, typ: &str, payload: &str) -> String {
    let mut context = Context::new(&SHA256);
    context.update(format!("{source_id}@{created_at}|{seq}|{typ}|").as_bytes());
    context.update(payload.as_bytes());
    let digest = context.finish();
    hex(&digest.as_ref()[..16])
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn call_id_of(obj: &Payload) -> Option<String> {
    first_string(obj, &["callId", "call_id", "id"])
}

fn usage_payload(value: Option<&Value>) -> Option<Value> {
    let obj = value?.as_object()?;
    let mut usage = Payload::new();
    for key in [
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "reasoningTokens",
    ] {
        if let Some(count) = raw_i64(obj, key) {
            if count != 0 {
                usage.insert(key.to_owned(), json!(count));
            }
        }
    }
    if usage.is_empty() {
        None
    } else {
        Some(Value::Object(usage))
    }
}

fn unmapped_reason(typ: &str) -> &'static str {
    const KNOWN: &[(&str, &str)] = &[
        (
            "assistant/chunk",
            "streaming delta; redundant with the final assistant/message text",
        ),
        (
            "assistant/attempt",
            "per-attempt accounting; v1 records the settled assistant/message and usage",
        ),
        (
            "reasoning-chunks",
            "streaming delta; v1 does not record reasoning tokens",
        ),
        (
            "text-chunks",
            "streaming delta; redundant with the final assistant/message text",
        ),
        (
            "tool-call-chunks",
            "streaming delta; redundant with the final assistant/message and tool/call",
        ),
        (
            "agent/inbox/spliced",
            "inbox rewrite; the spliced user message is imported from its own user/message row",
        ),
        (
            "session/title",
            "v1 has no title event; a v1 reader derives the title from the first user message",
        ),
        (
            "session/end-seed",
            "seeded-session marker; v1 has no equivalent",
        ),
        (
            "session/title-llm-request",
            "internal title request; not a session event in v1",
        ),
        (
            "permission/preset",
            "session policy state; v1 records policy on the tool call",
        ),
        (
            "sandbox/mode",
            "session policy state; v1 records policy on the tool call",
        ),
        (
            "approval/policy",
            "session policy state; v1 records policy on the tool call",
        ),
        (
            "request/context",
            "duplicates the model recorded by request/header and assistant/message",
        ),
        (
            "agent-preset/selected",
            "agent composition; v1 session/start.profile carries only the preset name",
        ),
        (
            "model/selection",
            "model selection; v1 records the model on llm/call and assistant/message",
        ),
        (
            "job/registered",
            "background job bookkeeping; v1 has no job events",
        ),
        (
            "job/settled",
            "background job bookkeeping; v1 has no job events",
        ),
        ("todo/write", "todo state; v1 has no todo event"),
        ("hook/invoked", "hook bookkeeping; v1 has no hook events"),
        ("hook/result", "hook bookkeeping; v1 has no hook events"),
        ("plan/mode", "planning mode; v1 has no plan events"),
        ("goal/change", "session goal; v1 has no goal events"),
        (
            "compaction/start",
            "compaction bracket; only compaction/summary is a v1 event",
        ),
        (
            "compaction/end",
            "compaction bracket; only compaction/summary is a v1 event",
        ),
        (
            "compaction/prune",
            "compaction bookkeeping; only compaction/summary is a v1 event",
        ),
        (
            "deliverables/presented",
            "deliverables; v1 has no deliverable events",
        ),
        (
            "error",
            "old error bookkeeping; v1 records failures on step/end and turn/end",
        ),
        (
            "llm/retry",
            "retry bookkeeping; v1 records the outcome on step/end",
        ),
        (
            "llm/retry-started",
            "retry bookkeeping; v1 records the outcome on step/end",
        ),
        (
            "working-directory/change",
            "working directory change; v1 records the workspace once in session/start",
        ),
        (
            "workspace/changes",
            "workspace bookkeeping; v1 has no workspace-change event",
        ),
        (
            "developer/message",
            "developer message; v1 has no developer-message event",
        ),
        ("team/member", "team bookkeeping; v1 has no team events"),
        (
            "team/message/delivered",
            "team bookkeeping; v1 has no team events",
        ),
        (
            "team/message/queued",
            "team bookkeeping; v1 has no team events",
        ),
        ("team/mode", "team bookkeeping; v1 has no team events"),
        ("team/task", "team bookkeeping; v1 has no team events"),
        (
            "subagent/catalog",
            "subagent bookkeeping; v1 has no subagent events",
        ),
        (
            "subagent/descriptor",
            "subagent bookkeeping; v1 has no subagent events",
        ),
        (
            "subagent/model-selection-policy",
            "subagent bookkeeping; v1 has no subagent events",
        ),
        ("tool/ptc-dispatch", "PTC bookkeeping; v1 has no PTC events"),
        (
            "tool/ptc-dispatch-start",
            "PTC bookkeeping; v1 has no PTC events",
        ),
        (
            "tool-workflow/agent-end",
            "workflow bookkeeping; v1 has no workflow events",
        ),
        (
            "tool-workflow/agent-start",
            "workflow bookkeeping; v1 has no workflow events",
        ),
        (
            "tool-workflow/run-end",
            "workflow bookkeeping; v1 has no workflow events",
        ),
        (
            "tool-workflow/run-start",
            "workflow bookkeeping; v1 has no workflow events",
        ),
        (
            "image/offload",
            "image offload bookkeeping; v1 has no image events",
        ),
        (
            "feedback/record",
            "human feedback; v1 has no feedback events",
        ),
        (
            "feedback/message-put",
            "human feedback; v1 has no feedback events",
        ),
        (
            "feedback/message-delete",
            "human feedback; v1 has no feedback events",
        ),
        (
            "schedule/change",
            "schedule bookkeeping; v1 has no schedule events",
        ),
        (
            "command/run",
            "slash-command bookkeeping; v1 has no command events",
        ),
        (
            "command/done",
            "slash-command bookkeeping; v1 has no command events",
        ),
        (
            "session-log-deepseek/delivery-accepted",
            "session-log delivery bookkeeping; v1 has no equivalent",
        ),
        (
            "web/deepseek-search-llm-request",
            "internal search request; v1 has no equivalent",
        ),
    ];
    KNOWN
        .iter()
        .find(|(name, _)| *name == typ)
        .map(|(_, why)| *why)
        .unwrap_or("no v1 event type for this old record")
}

#[cfg(test)]
mod tests {
    use super::{compile, make_plan};
    use crate::detect::detect;
    use std::path::Path;

    #[test]
    fn plan_and_compile_agree() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v4-tool");
        let src = detect(&dir).expect("detect fixture");
        let plan = make_plan(&src).expect("plan");
        let (events, dropped) = compile(&src.dir, &plan.session_id).expect("compile");
        assert_eq!(plan.events, events.len());
        assert_eq!(plan.dropped, dropped);
    }
}
