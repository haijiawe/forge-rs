//! What a compressed block has to guarantee before anything trusts it.

use forge_context::{
    is_bad_block, is_block_too_large, is_empty_summary, is_overlap, parse_block, Block, Ledger,
    MAX_SUMMARY_BYTES,
};
use forge_core::event::{parse_log, Event, EventType};
use forge_core::ErrorKind;

const SESSION: &str = "s_01jya7c9q3r5t7v9w1x3y5z7";
const BLOCK_ID: &str = "d1b2c3d4e5f60718293a4b5c6d7e8f93";

fn event(seq: u64, typ: EventType, payload: serde_json::Value) -> Event {
    Event {
        v: 1,
        seq,
        id: format!("{seq:032x}"),
        ts: 1_794_787_200_000 + seq as i64,
        session: SESSION.to_string(),
        turn: 0,
        step: 0,
        typ: typ.as_str().to_string(),
        payload: Some(payload),
    }
}

fn block(from: u64, to: u64) -> Block {
    Block::new(BLOCK_ID, from, to, "a summary of the exchange").expect("valid block")
}

fn compressed(seq: u64, block: &Block) -> Event {
    let mut event = forge_context::context_compressed(SESSION, block).expect("encodes");
    event.seq = seq;
    event.id = format!("{seq:032x}");
    event.ts = 1_794_787_200_000 + seq as i64;
    event
}

#[test]
fn a_block_round_trips_through_its_event() {
    let original = block(3, 9).with_messages(4);
    let written = compressed(12, &original);
    assert_eq!(written.typ, EventType::CONTEXT_COMPRESSED.as_str());
    assert_eq!(written.typ.as_str(), "context/compressed");
    let read = parse_block(&written).expect("parses").expect("is a block");
    assert_eq!(read, original);
}

#[test]
fn another_event_type_is_not_a_block() {
    let other = event(
        1,
        EventType::USER_MESSAGE,
        serde_json::json!({"text": "hi"}),
    );
    assert!(parse_block(&other).expect("no error").is_none());
}

#[test]
fn a_bad_id_is_refused_before_it_can_be_a_path() {
    let err = Block::new("../../token", 1, 2, "s").expect_err("refused");
    assert!(is_bad_block(&err), "{:?}", err);
    assert_eq!(err.kind(), ErrorKind::Invalid);
    let short = Block::new("abc", 1, 2, "s").expect_err("refused");
    assert!(is_bad_block(&short));
}

#[test]
fn an_inverted_or_zero_range_is_refused() {
    assert!(is_bad_block(
        &Block::new(BLOCK_ID, 0, 2, "s").expect_err("zero from")
    ));
    assert!(is_bad_block(
        &Block::new(BLOCK_ID, 9, 3, "s").expect_err("inverted")
    ));
    assert!(
        Block::new(BLOCK_ID, 4, 4, "s").is_ok(),
        "a single record is a range"
    );
}

#[test]
fn an_empty_summary_is_refused() {
    let err = Block::new(BLOCK_ID, 1, 2, "   ").expect_err("empty");
    assert!(is_empty_summary(&err), "{:?}", err);
}

#[test]
fn an_oversized_summary_is_refused_rather_than_cut() {
    let long = "x".repeat(MAX_SUMMARY_BYTES + 1);
    let err = Block::new(BLOCK_ID, 1, 2, long).expect_err("too large");
    assert!(is_block_too_large(&err), "{:?}", err);
}

#[test]
fn overlapping_blocks_are_refused_with_both_ranges_named() {
    let mut ledger = Ledger::new();
    ledger.insert(block(10, 20)).expect("first");
    let err = ledger
        .insert(Block::new("e1b2c3d4e5f60718293a4b5c6d7e8f94", 15, 30, "other").expect("valid"))
        .expect_err("overlap");
    assert!(is_overlap(&err), "{:?}", err);
    let message = err.message();
    assert!(message.contains("15"), "{message}");
    assert!(message.contains("[10..20]"), "{message}");
    assert_eq!(ledger.len(), 1);
}

#[test]
fn a_duplicate_id_is_refused_even_without_overlap() {
    let mut ledger = Ledger::new();
    ledger.insert(block(1, 2)).expect("first");
    let err = ledger
        .insert(Block::new(BLOCK_ID, 40, 50, "same id").expect("valid"))
        .expect_err("duplicate");
    assert!(is_bad_block(&err), "{:?}", err);
    assert!(
        err.message().contains("already exists"),
        "{}",
        err.message()
    );
}

#[test]
fn a_touching_range_is_not_an_overlap() {
    let mut ledger = Ledger::new();
    ledger.insert(block(1, 5)).expect("first");
    ledger
        .insert(Block::new("f1b2c3d4e5f60718293a4b5c6d7e8f95", 6, 9, "next").expect("valid"))
        .expect("adjacent is fine");
    assert_eq!(ledger.hidden_span(), 9);
}

#[test]
fn covering_finds_the_one_block_and_nothing_else() {
    let mut ledger = Ledger::new();
    ledger.insert(block(3, 7)).expect("first");
    assert!(ledger.hides(3));
    assert!(ledger.hides(7));
    assert!(!ledger.hides(2));
    assert!(!ledger.hides(8));
    assert_eq!(ledger.covering(5).map(|b| b.id.as_str()), Some(BLOCK_ID));
}

#[test]
fn search_reads_summaries_and_requires_every_term() {
    let mut ledger = Ledger::new();
    ledger
        .insert(Block::new(BLOCK_ID, 1, 4, "the bench startup tolerance was too tight").unwrap())
        .expect("first");
    ledger
        .insert(
            Block::new(
                "a2b2c3d4e5f60718293a4b5c6d7e8f96",
                5,
                9,
                "a discussion about the sandbox",
            )
            .unwrap(),
        )
        .expect("second");

    let hits = ledger.search("BENCH tolerance");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].block.from, 1);

    let only_bench = ledger.search("bench");
    assert_eq!(only_bench.len(), 1);

    assert!(ledger.search("bench sandbox").is_empty(), "terms are ANDed");
    assert!(
        ledger.search("   ").is_empty(),
        "an empty query matches nothing"
    );
}

#[test]
fn search_is_ordered_by_score_then_by_range() {
    let mut ledger = Ledger::new();
    ledger
        .insert(Block::new("a2b2c3d4e5f60718293a4b5c6d7e8f96", 1, 2, "cache").unwrap())
        .expect("first");
    ledger
        .insert(Block::new("b2b2c3d4e5f60718293a4b5c6d7e8f97", 3, 4, "cache cache").unwrap())
        .expect("second");
    let hits = ledger.search("cache");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].block.from, 3, "more mentions first");
    assert_eq!(hits[1].block.from, 1);
}

#[test]
fn a_ledger_folds_from_a_log_and_survives_a_replay() {
    let block = block(1, 2);
    let log = [
        event(1, EventType::USER_MESSAGE, serde_json::json!({"text": "a"})),
        event(
            2,
            EventType::ASSISTANT_MESSAGE,
            serde_json::json!({"text": "b"}),
        ),
        compressed(3, &block),
    ];
    let first = Ledger::from_events(&log).expect("folds");
    assert_eq!(first.len(), 1);
    assert_eq!(first.get(BLOCK_ID), Some(&block));

    // The same events, written as a log and parsed back: a block that only
    // survived because it was held in memory would fail here.
    let canonical = log
        .iter()
        .map(|event| event.canonical().expect("encodes"))
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = parse_log(&(canonical + "\n")).expect("parses");
    let second = Ledger::from_events(&parsed).expect("folds");
    assert_eq!(second.blocks(), first.blocks());
}

#[test]
fn a_malformed_block_event_is_an_error_not_a_skip() {
    let broken = event(
        1,
        EventType::CONTEXT_COMPRESSED,
        serde_json::json!({"blockId": BLOCK_ID, "from": 1, "summary": "a summary"}),
    );
    let err = Ledger::from_events(&[broken]).expect_err("fail closed");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    assert!(err.message().contains("\"to\""), "{}", err.message());
}
