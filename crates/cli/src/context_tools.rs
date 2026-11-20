//! The three tools that make a compressed block addressable.
//!
//! They live in the CLI rather than in `hearth-tool` for the same reason the MCP
//! and plugin bridges do: they need the *log*, which is an L2 concern owned by
//! the surface that knows which session is running. `hearth-tool` sees an
//! `Arc<dyn Tool>` and nothing else.
//!
//! Three rules:
//!
//! 1. **A tool never appends.** The agent loop is the only writer, so `compress`
//!    returns the payload it wants written in `meta.contextBlock` and the loop
//!    emits `context/compressed`. A tool that appended would be a second writer
//!    racing the first, and the log would grow two sequences.
//! 2. **A block must point at records that already exist.** `compress` refuses a
//!    `to` past the end of the log: an address into the future is not an
//!    address, and it would become one only if the log happened to grow into
//!    it.
//! 3. **Restoring is a read.** `decompress` re-reads the range from the same
//!    file, so a block never has to be trusted for its contents — only for the
//!    range it names.

use std::path::{Path, PathBuf};

use hearth_context::{Block, Ledger, ERR_OVERLAP};
use hearth_core::event::Event;
use hearth_core::ids::new_id;
use hearth_core::{Error, Result as CoreResult};
use hearth_llm::ToolSpec;
use hearth_session::Store;
use hearth_tool::{decode_args, truncate, Call, Limits, Result as ToolResult, Tool, ERR_BAD_ARGS};
use serde::Deserialize;
use serde_json::{json, Value};

pub const TOOL_COMPRESS: &str = "compress";
pub const TOOL_DECOMPRESS: &str = "decompress";
pub const TOOL_SEARCH: &str = "search_context";

/// The member of `Result.meta` that carries the payload the loop must append.
pub const META_BLOCK: &str = "contextBlock";

pub const CODE_NO_SUCH_BLOCK: &str = "no_such_block";
pub const CODE_RANGE_NOT_CLOSED: &str = "range_not_closed";
pub const CODE_OVERLAP: &str = "overlap";
pub const CODE_BAD_RANGE: &str = "bad_range";

/// The store for one log. Opening is cheap; the file is only read when asked.
fn open(log: &Path) -> CoreResult<Store> {
    Store::open(log, hearth_session::Options::default())
}

fn read_events(store: &Store) -> CoreResult<Vec<Event>> {
    let mut events = Vec::new();
    store.read_from(0, |event| {
        events.push(event.clone());
        Ok(())
    })?;
    Ok(events)
}

fn ledger_of(store: &Store) -> CoreResult<Ledger> {
    Ledger::from_events(&read_events(store)?)
}

fn block_payload(block: &Block) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("blockId".into(), Value::String(block.id.clone()));
    payload.insert("from".into(), Value::from(block.from));
    payload.insert("to".into(), Value::from(block.to));
    payload.insert("summary".into(), Value::String(block.summary.clone()));
    if block.messages > 0 {
        payload.insert("messages".into(), Value::from(block.messages));
    }
    Value::Object(payload)
}

/// Turns a core error into a tool error, keeping the code the model can act on.
fn failed(err: Error) -> ToolResult {
    let message = err.message().to_string();
    let code = if message.starts_with(ERR_OVERLAP) {
        CODE_OVERLAP
    } else if message.starts_with(hearth_context::ERR_BAD_BLOCK)
        || message.starts_with(hearth_context::ERR_EMPTY_SUMMARY)
        || message.starts_with(hearth_context::ERR_BLOCK_TOO_LARGE)
    {
        ERR_BAD_ARGS
    } else {
        hearth_tool::CODE_IO_ERROR
    };
    ToolResult::error(code, message)
}

// ---- compress -------------------------------------------------------------

pub struct CompressTool {
    limits: Limits,
    log: PathBuf,
}

impl CompressTool {
    pub fn new(limits: Limits, log: impl Into<PathBuf>) -> CompressTool {
        CompressTool {
            limits,
            log: log.into(),
        }
    }
}

#[derive(Deserialize)]
struct CompressArgs {
    from: u64,
    to: u64,
    summary: String,
    #[serde(default)]
    messages: u32,
}

impl Tool for CompressTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_COMPRESS.to_string(),
            description:
                "Replace a closed range of this session's earlier messages with a summary \
                 you write, keeping the range restorable by id. Use it when the history is long \
                 and one stretch is finished with: you pay for the summary from then on, and \
                 `decompress` brings the originals back. You must say what the range contained, \
                 so keep the summary dense and specific."
                    .to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "from": {"type": "integer", "description": "first seq to hide, inclusive"},
                    "to": {"type": "integer", "description": "last seq to hide, inclusive"},
                    "summary": {"type": "string", "description": "what the model should know instead"},
                    "messages": {"type": "integer", "description": "how many messages the range covered, if you counted them"}
                },
                "required": ["from", "to", "summary"]
            }),
        }
    }

    fn call(&self, call: Call) -> ToolResult {
        let args: CompressArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(err) => return err,
        };
        if args.from == 0 {
            return ToolResult::error(
                CODE_BAD_RANGE,
                "from must be at least 1: seq starts at 1, and 0 names no record",
            );
        }
        if args.to < args.from {
            return ToolResult::error(
                CODE_BAD_RANGE,
                format!("the range [{}, {}] is inverted", args.from, args.to),
            );
        }
        let store = match open(&self.log) {
            Ok(store) => store,
            Err(err) => return failed(err),
        };
        let last = store.last_seq();
        if args.to > last {
            return ToolResult::error(
                CODE_RANGE_NOT_CLOSED,
                format!(
                    "the log ends at seq {last}, so [{}, {}] is not a closed range yet",
                    args.from, args.to
                ),
            );
        }
        let ledger = match ledger_of(&store) {
            Ok(ledger) => ledger,
            Err(err) => return failed(err),
        };
        if let Some(other) = ledger
            .blocks()
            .iter()
            .find(|other| other.overlaps(args.from, args.to))
        {
            return ToolResult::error(
                CODE_OVERLAP,
                format!(
                    "[{}, {}] overlaps block {} which already covers [{}, {}]; \
                     a record may only be hidden once",
                    args.from, args.to, other.id, other.from, other.to
                ),
            );
        }
        let id = match new_id() {
            Ok(id) => id,
            Err(err) => return failed(err),
        };
        let block = match Block::new(id, args.from, args.to, args.summary) {
            Ok(block) => block.with_messages(args.messages),
            Err(err) => return failed(err),
        };
        let text = format!(
            "compressed seq {}..{} as block {} ({} record(s)).\n\n\
             The summary is now what this window shows for that range. \
             `decompress {}` re-reads the originals from the log.",
            block.from,
            block.to,
            block.id,
            block.span(),
            block.id
        );
        let (text, truncated) = truncate(&text, self.limits);
        ToolResult::ok(text)
            .with_meta(META_BLOCK, block_payload(&block))
            .with_meta("blockId", json!(block.id))
            .with_meta("from", json!(block.from))
            .with_meta("to", json!(block.to))
            .with_meta("truncated", json!(truncated))
    }
}

// ---- decompress -----------------------------------------------------------

pub struct DecompressTool {
    limits: Limits,
    log: PathBuf,
}

impl DecompressTool {
    pub fn new(limits: Limits, log: impl Into<PathBuf>) -> DecompressTool {
        DecompressTool {
            limits,
            log: log.into(),
        }
    }
}

#[derive(Deserialize)]
struct DecompressArgs {
    id: String,
}

impl Tool for DecompressTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_DECOMPRESS.to_string(),
            description: "Read back the records a compressed block hid, verbatim and in order. \
                 This costs what the originals cost, so restore a block when you need the exact \
                 text, not to check that it exists."
                .to_string(),
            schema: json!({
                "type": "object",
                "properties": {"id": {"type": "string", "description": "the block id `compress` returned"}},
                "required": ["id"]
            }),
        }
    }

    fn call(&self, call: Call) -> ToolResult {
        let args: DecompressArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(err) => return err,
        };
        let store = match open(&self.log) {
            Ok(store) => store,
            Err(err) => return failed(err),
        };
        let ledger = match ledger_of(&store) {
            Ok(ledger) => ledger,
            Err(err) => return failed(err),
        };
        let block = match ledger.get(args.id.trim()) {
            Some(block) => block.clone(),
            None => {
                let known: Vec<&str> = ledger.blocks().iter().map(|b| b.id.as_str()).collect();
                return ToolResult::error(
                    CODE_NO_SUCH_BLOCK,
                    if known.is_empty() {
                        format!(
                            "no compressed blocks exist in this session (asked for {:?})",
                            args.id
                        )
                    } else {
                        format!(
                            "no block {:?}; this session has {}",
                            args.id,
                            known.join(", ")
                        )
                    },
                );
            }
        };
        let events = match read_events(&store) {
            Ok(events) => events,
            Err(err) => return failed(err),
        };
        let mut lines = Vec::new();
        for event in events.iter().filter(|event| block.covers(event.seq)) {
            match event.canonical() {
                Ok(line) => lines.push(line),
                Err(err) => return failed(err),
            }
        }
        let header = format!(
            "block {} covers seq {}..{} ({} record(s), summary {} bytes)",
            block.id,
            block.from,
            block.to,
            lines.len(),
            block.summary.len()
        );
        let body = lines.join("\n");
        let (body, truncated) = truncate(&body, self.limits);
        ToolResult::ok(format!("{header}\n\n{body}"))
            .with_meta("blockId", json!(block.id))
            .with_meta("from", json!(block.from))
            .with_meta("to", json!(block.to))
            .with_meta("records", json!(lines_len(&block, &events)))
            .with_meta("truncated", json!(truncated))
    }
}

fn lines_len(block: &Block, events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| block.covers(event.seq))
        .count()
}

// ---- search_context -------------------------------------------------------

pub struct SearchContextTool {
    limits: Limits,
    log: PathBuf,
}

impl SearchContextTool {
    pub fn new(limits: Limits, log: impl Into<PathBuf>) -> SearchContextTool {
        SearchContextTool {
            limits,
            log: log.into(),
        }
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
}

impl Tool for SearchContextTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_SEARCH.to_string(),
            description: "Search the summaries of the blocks this session compressed. Every term \
                 must appear. Returns block ids you can hand to `decompress`."
                .to_string(),
            schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "words to look for in the summaries"}},
                "required": ["query"]
            }),
        }
    }

    fn call(&self, call: Call) -> ToolResult {
        let args: SearchArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(err) => return err,
        };
        let store = match open(&self.log) {
            Ok(store) => store,
            Err(err) => return failed(err),
        };
        let ledger = match ledger_of(&store) {
            Ok(ledger) => ledger,
            Err(err) => return failed(err),
        };
        let hits = ledger.search(&args.query);
        if hits.is_empty() {
            return ToolResult::ok(format!(
                "no compressed block mentions {:?}. {} block(s) exist in this session, \
                 covering {} record(s) in total.",
                args.query,
                ledger.len(),
                ledger.hidden_span()
            ));
        }
        let mut text = format!("{} block(s) mention {:?}:\n", hits.len(), args.query);
        for hit in &hits {
            text.push_str(&format!(
                "\n{} seq {}..{} ({} record(s))\n  {}\n",
                hit.block.id,
                hit.block.from,
                hit.block.to,
                hit.block.span(),
                hit.block.summary
            ));
        }
        let (text, truncated) = truncate(&text, self.limits);
        ToolResult::ok(text)
            .with_meta("hits", json!(hits.len()))
            .with_meta("truncated", json!(truncated))
    }
}

/// The three tools, all reading the same log.
pub fn tools(limits: Limits, log: &Path) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(CompressTool::new(limits, log)),
        std::sync::Arc::new(DecompressTool::new(limits, log)),
        std::sync::Arc::new(SearchContextTool::new(limits, log)),
    ]
}

/// The `Value` the loop should append, if this result asks for one.
pub fn block_from_result(meta: &serde_json::Map<String, Value>) -> Option<Value> {
    match meta.get(META_BLOCK) {
        Some(value @ Value::Object(_)) => Some(value.clone()),
        _ => None,
    }
}

/// Every block this log records, oldest first.
pub fn blocks(log: &Path) -> CoreResult<Vec<Block>> {
    let store = open(log)?;
    let events = read_events(&store)?;
    Ok(Ledger::from_events(&events)?.blocks().to_vec())
}

/// A predicate answering "is this seq behind a block?" for one turn.
///
/// The ranges are read once, when the turn is built, because the history a turn
/// sends is fixed at that moment: a block written by `compress` later in the
/// same turn cannot retroactively remove records that were already sent. Reading
/// per message would also mean a `stat` per record.
///
/// A log that cannot be read hides nothing. That direction is deliberate:
/// hiding is losing information, so the failure mode has to be showing more,
/// which is exactly what a session without blocks does.
pub fn hidden_hook(log: &Path) -> impl Fn(u64) -> bool + Send + Sync + 'static {
    let ranges: Vec<(u64, u64)> = blocks(log)
        .unwrap_or_default()
        .iter()
        .map(|block| (block.from, block.to))
        .collect();
    move |seq: u64| ranges.iter().any(|(from, to)| seq >= *from && seq <= *to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hearth_core::event::EventType;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hearth-ctx-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir.join("session.jsonl")
    }

    fn seed(log: &Path, count: usize) -> Store {
        let store = Store::open(log, hearth_session::Options::default()).expect("open");
        for _ in 0..count {
            let event = Event::new(
                "s_01jya7c9q3r5t7v9w1x3y5z7",
                EventType::USER_MESSAGE,
                Some(json!({"text": "a message"})),
            );
            store.append(&event).expect("append");
        }
        store
    }

    fn call(tool: &dyn Tool, name: &str, args: Value) -> ToolResult {
        tool.call(Call {
            id: "c1".to_string(),
            name: name.to_string(),
            args,
            workspace: String::new(),
        })
    }

    #[test]
    fn compress_returns_a_payload_the_loop_can_append() {
        let log = scratch("compress");
        let _store = seed(&log, 4);
        let tool = CompressTool::new(Limits::default(), &log);
        let result = call(
            &tool,
            TOOL_COMPRESS,
            json!({"from": 1, "to": 3, "summary": "the first three"}),
        );
        assert!(!result.is_error, "{}", result.output);
        let payload = block_from_result(&result.meta).expect("payload");
        assert_eq!(payload["from"], json!(1));
        assert_eq!(payload["to"], json!(3));
        assert_eq!(payload["summary"], json!("the first three"));
        let parsed = hearth_context::wire::decode(&payload, 9).expect("decodes");
        assert_eq!(parsed.span(), 3);
        assert!(result.output.contains(&parsed.id), "{}", result.output);
    }

    #[test]
    fn compress_refuses_a_range_the_log_has_not_closed() {
        let log = scratch("unclosed");
        let _store = seed(&log, 2);
        let tool = CompressTool::new(Limits::default(), &log);
        let result = call(
            &tool,
            TOOL_COMPRESS,
            json!({"from": 1, "to": 9, "summary": "too far"}),
        );
        assert!(result.is_error);
        assert_eq!(result.code, CODE_RANGE_NOT_CLOSED);
        assert!(result.output.contains("ends at seq 2"), "{}", result.output);
    }

    #[test]
    fn compress_refuses_an_inverted_or_zero_range() {
        let log = scratch("inverted");
        let _store = seed(&log, 4);
        let tool = CompressTool::new(Limits::default(), &log);
        let zero = call(
            &tool,
            TOOL_COMPRESS,
            json!({"from": 0, "to": 2, "summary": "s"}),
        );
        assert_eq!(zero.code, CODE_BAD_RANGE);
        let inverted = call(
            &tool,
            TOOL_COMPRESS,
            json!({"from": 3, "to": 1, "summary": "s"}),
        );
        assert_eq!(inverted.code, CODE_BAD_RANGE);
    }

    #[test]
    fn compress_refuses_an_empty_summary() {
        let log = scratch("empty");
        let _store = seed(&log, 2);
        let tool = CompressTool::new(Limits::default(), &log);
        let result = call(
            &tool,
            TOOL_COMPRESS,
            json!({"from": 1, "to": 1, "summary": "  "}),
        );
        assert!(result.is_error);
        assert_eq!(result.code, ERR_BAD_ARGS, "{}", result.output);
    }

    #[test]
    fn a_second_block_cannot_overlap_the_first() {
        let log = scratch("overlap");
        let store = seed(&log, 6);
        let first =
            hearth_context::Block::new("d1b2c3d4e5f60718293a4b5c6d7e8f93", 1, 3, "the first three")
                .expect("block");
        let event = hearth_context::context_compressed("s_01jya7c9q3r5t7v9w1x3y5z7", &first)
            .expect("event");
        store.append(&event).expect("append");

        let tool = CompressTool::new(Limits::default(), &log);
        let result = call(
            &tool,
            TOOL_COMPRESS,
            json!({"from": 2, "to": 4, "summary": "second"}),
        );
        assert!(result.is_error);
        assert_eq!(result.code, CODE_OVERLAP);
        assert!(result.output.contains(&first.id), "{}", result.output);
    }

    #[test]
    fn decompress_returns_the_original_records() {
        let log = scratch("decompress");
        let store = seed(&log, 5);
        let block = Block::new(
            "e1b2c3d4e5f60718293a4b5c6d7e8f94",
            2,
            3,
            "two messages about a message",
        )
        .expect("block");
        let event = hearth_context::context_compressed("s_01jya7c9q3r5t7v9w1x3y5z7", &block)
            .expect("event");
        store.append(&event).expect("append");

        let tool = DecompressTool::new(Limits::default(), &log);
        let result = call(&tool, TOOL_DECOMPRESS, json!({"id": block.id}));
        assert!(!result.is_error, "{}", result.output);
        let lines: Vec<&str> = result
            .output
            .lines()
            .filter(|line| line.starts_with('{'))
            .collect();
        assert_eq!(lines.len(), 2, "{}", result.output);
        assert!(
            result.output.contains("covers seq 2..3"),
            "{}",
            result.output
        );
    }

    #[test]
    fn decompress_says_what_blocks_exist_when_the_id_is_unknown() {
        let log = scratch("nosuch");
        let _store = seed(&log, 2);
        let tool = DecompressTool::new(Limits::default(), &log);
        let result = call(
            &tool,
            TOOL_DECOMPRESS,
            json!({"id": "f1b2c3d4e5f60718293a4b5c6d7e8f95"}),
        );
        assert!(result.is_error);
        assert_eq!(result.code, CODE_NO_SUCH_BLOCK);
        assert!(
            result.output.contains("no compressed blocks exist"),
            "{}",
            result.output
        );
    }

    #[test]
    fn search_reads_the_summaries_and_reports_hits() {
        let log = scratch("search");
        let store = seed(&log, 4);
        let block = Block::new(
            "a2b2c3d4e5f60718293a4b5c6d7e8f96",
            1,
            2,
            "we settled the bench tolerance question",
        )
        .expect("block");
        let event = hearth_context::context_compressed("s_01jya7c9q3r5t7v9w1x3y5z7", &block)
            .expect("event");
        store.append(&event).expect("append");

        let tool = SearchContextTool::new(Limits::default(), &log);
        let found = call(&tool, TOOL_SEARCH, json!({"query": "bench tolerance"}));
        assert!(!found.is_error, "{}", found.output);
        assert!(found.output.contains(&block.id), "{}", found.output);

        let missing = call(&tool, TOOL_SEARCH, json!({"query": "sandbox"}));
        assert!(!missing.is_error, "a miss is an answer, not a failure");
        assert!(
            missing.output.contains("no compressed block mentions"),
            "{}",
            missing.output
        );
    }
}
