//! The goal state machine is small enough that the tests are the specification:
//! every rule in `fold` and `next_action` is pinned here, including the ones
//! that only matter when a log is written by a version that is not this build.

use hearth_core::{Event, EventType};
use hearth_goal::{
    continuation_prompt, fold, goal_continued, goal_created, goal_ended, validate, Action, Goal,
    Status, StopReason, CONTINUATION_PROMPT, DEFAULT_MAX_ROUNDS,
};
use serde_json::{json, Value};

const SESSION: &str = "s_01jya7c9q3r5t7v9w1x3y5z7";

fn event(typ: EventType, payload: Value) -> Event {
    Event::new(SESSION, typ, Some(payload))
}

fn created(id: &str, text: &str, max_rounds: u32) -> Event {
    event(
        EventType::GOAL_CREATED,
        json!({"goalId": id, "text": text, "maxRounds": max_rounds}),
    )
}

fn continued(id: &str, round: u32) -> Event {
    event(
        EventType::GOAL_CONTINUED,
        json!({"goalId": id, "round": round}),
    )
}

fn ended(id: &str, reason: &str) -> Event {
    event(
        EventType::GOAL_ENDED,
        json!({"goalId": id, "reason": reason}),
    )
}

#[test]
fn a_new_goal_is_active_and_wants_round_one() {
    let goal = Goal::new("g1", "finish the port", 3);
    assert_eq!(goal.status, Status::Active);
    assert_eq!(goal.rounds, 0);
    assert_eq!(goal.next_action(), Action::Continue { round: 1 });
}

#[test]
fn rounds_advance_with_each_continuation() {
    let goal = Goal::new("g1", "finish the port", 5);
    let log = vec![
        created("g1", "finish the port", 5),
        continued("g1", 1),
        continued("g1", 2),
    ];
    let folded = fold(&log).expect("a goal was created");
    assert_eq!(folded.rounds, 2);
    assert_eq!(folded.next_action(), Action::Continue { round: 3 });
    assert_eq!(folded, Goal { rounds: 2, ..goal });
}

#[test]
fn the_budget_counts_turns_not_continuations() {
    // `maxRounds` is a budget of turns, and the first turn comes with
    // `goal/created`. So a budget of 2 buys one continuation: after it the log
    // has one `goal/continued` and two turns have been spent.
    let log = vec![created("g1", "finish the port", 2), continued("g1", 1)];
    let folded = fold(&log).expect("a goal was created");
    assert_eq!(folded.rounds, 1);
    assert_eq!(
        folded.next_action(),
        Action::Stop {
            reason: StopReason::Exhausted
        },
        "one continuation of a two-turn budget is the last turn, not permission for another"
    );

    // Before any continuation there is still one turn left in that budget.
    let fresh = fold(&log[..1]).expect("a goal was created");
    assert_eq!(fresh.rounds, 0);
    assert_eq!(fresh.next_action(), Action::Continue { round: 1 });

    // A budget of 1 is a single turn: nothing to continue.
    assert_eq!(
        Goal::new("g2", "one turn only", 1).next_action(),
        Action::Stop {
            reason: StopReason::Exhausted
        }
    );
}

#[test]
fn every_end_reason_maps_to_its_status_and_stops() {
    for (reason, status, stop) in [
        ("complete", Status::Complete, StopReason::Complete),
        ("blocked", Status::Blocked, StopReason::Blocked),
        ("cancelled", Status::Cancelled, StopReason::Cancelled),
        ("exhausted", Status::Exhausted, StopReason::Exhausted),
    ] {
        let log = vec![created("g1", "finish the port", 5), ended("g1", reason)];
        let folded = fold(&log).expect("a goal was created");
        assert_eq!(folded.status, status, "reason {reason}");
        assert!(!folded.is_open(), "reason {reason} left the goal open");
        assert_eq!(folded.next_action(), Action::Stop { reason: stop });
    }
}

#[test]
fn the_last_goal_wins_and_an_older_goals_ending_does_not_end_it() {
    let log = vec![
        created("g1", "first thing", 5),
        continued("g1", 3),
        ended("g1", "complete"),
        created("g2", "second thing", 4),
        // A straggler from the finished goal must not touch the live one.
        ended("g1", "cancelled"),
        continued("g1", 9),
    ];
    let folded = fold(&log).expect("a goal was created");
    assert_eq!(folded.id, "g2");
    assert_eq!(folded.text, "second thing");
    assert_eq!(folded.rounds, 0, "g1's continuation leaked into g2");
    assert_eq!(folded.status, Status::Active);
    assert_eq!(folded.next_action(), Action::Continue { round: 1 });
}

#[test]
fn a_payload_this_build_cannot_read_is_skipped_without_panicking() {
    let log = vec![
        created("g1", "finish the port", 5),
        // No `goalId`: unreadable, so it belongs to nothing.
        event(EventType::GOAL_CONTINUED, json!({"round": 7})),
        // No payload at all.
        Event::new(SESSION, EventType::GOAL_ENDED, None),
        // `round` is a string, not a number.
        event(
            EventType::GOAL_CONTINUED,
            json!({"goalId": "g1", "round": "two"}),
        ),
        // Unknown reason.
        ended("g1", "sunset"),
        // Payload is not an object.
        Event::new(SESSION, EventType::GOAL_ENDED, Some(json!([1, 2, 3]))),
    ];
    let folded = fold(&log).expect("a goal was created");
    assert_eq!(folded.rounds, 0);
    assert_eq!(folded.status, Status::Active);
}

#[test]
fn an_empty_log_or_one_without_goals_folds_to_nothing() {
    assert_eq!(fold(&[]), None);
    let log = vec![event(EventType::TURN_START, json!({"number": 1}))];
    assert_eq!(fold(&log), None);
}

#[test]
fn a_missing_budget_falls_back_to_the_documented_default() {
    let log = vec![event(
        EventType::GOAL_CREATED,
        json!({"goalId": "g1", "text": "no budget given"}),
    )];
    let folded = fold(&log).expect("a goal was created");
    assert_eq!(folded.max_rounds, DEFAULT_MAX_ROUNDS);
}

#[test]
fn the_continuation_prompt_names_the_goal_and_says_how_to_stop() {
    let goal = Goal::new("g1", "make the release workflow run", 3);
    let prompt = continuation_prompt(&goal);
    assert!(prompt.contains("make the release workflow run"));
    assert!(prompt.contains("stop"));
    assert!(
        !prompt.contains("{goal}"),
        "the placeholder must be substituted: {prompt}"
    );
    // Deterministic: the same goal yields the same bytes, so the log explains
    // the prompt that produced the next round.
    assert_eq!(prompt, continuation_prompt(&goal));
    assert!(CONTINUATION_PROMPT.contains("{goal}"));
}

#[test]
fn the_event_builders_write_the_documented_payload() {
    let goal = Goal::new("g1", "finish the port", 7);

    let head = goal_created(SESSION, &goal);
    assert_eq!(head.typ, "goal/created");
    assert_eq!(head.session, SESSION);
    assert_eq!(
        head.payload.expect("created carries a payload"),
        json!({"goalId": "g1", "text": "finish the port", "maxRounds": 7, "revision": 0})
    );

    let round = goal_continued(SESSION, &goal, 2);
    assert_eq!(round.typ, "goal/continued");
    assert_eq!(
        round.payload.expect("continued carries a payload"),
        json!({"goalId": "g1", "round": 2, "revision": 1})
    );

    let done = goal_ended(SESSION, &goal, StopReason::Blocked, Some("needs a runner"));
    assert_eq!(done.typ, "goal/ended");
    assert_eq!(
        done.payload.expect("ended carries a payload"),
        json!({"goalId": "g1", "reason": "blocked", "note": "needs a runner", "revision": 0})
    );

    let quiet = goal_ended(SESSION, &goal, StopReason::Complete, None);
    let payload = quiet.payload.expect("ended carries a payload");
    assert_eq!(
        payload,
        json!({"goalId": "g1", "reason": "complete", "revision": 0})
    );
    assert!(
        payload.get("note").is_none(),
        "an absent note must not be written as null"
    );
}

#[test]
fn a_goal_that_could_never_run_is_refused_before_it_is_written() {
    let empty_text = Goal::new("g1", "   ", 3);
    let error = validate(&empty_text).expect_err("empty text must be refused");
    assert!(
        error.message().contains("empty goal text"),
        "{}",
        error.message()
    );

    let no_budget = Goal::new("g1", "do the thing", 0);
    let error = validate(&no_budget).expect_err("a zero budget must be refused");
    assert!(error.message().contains("maxRounds"), "{}", error.message());

    let no_id = Goal::new("", "do the thing", 3);
    let error = validate(&no_id).expect_err("an empty id must be refused");
    assert!(error.message().contains("goal id"), "{}", error.message());

    validate(&Goal::new("g1", "do the thing", 1)).expect("a well-formed goal passes");
}
