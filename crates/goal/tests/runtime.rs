//! The round driver as a function: every case below builds the log by hand, so
//! the decision is tested without a session, a store, a clock or a turn loop.
//!
//! This is the H3 claim made checkable. DSH drives goal rounds from inside an
//! async state machine that reacts to agent events; here the same lifecycle is
//! six assertions over a `Vec<Event>`.

use forge_core::{Event, EventType};
use forge_goal::{
    fold, goal_continued, goal_created, goal_ended, next, reservation_to_open, Goal, Next,
    Reservation, Snapshot, Status, StopReason,
};
use serde_json::json;

fn created(text: &str, max_rounds: u32) -> Event {
    goal_created("s", &Goal::new("g1", text, max_rounds))
}

#[test]
fn an_empty_log_has_no_goal_to_act_on() {
    let events: Vec<Event> = Vec::new();
    assert_eq!(
        next(Snapshot {
            events: &events,
            claim: None,
            running: false
        }),
        Next::NoGoal,
        "no goal is not a stopped goal: there is nothing to close"
    );
}

#[test]
fn a_fresh_goal_opens_its_first_round_with_the_continuation_prompt() {
    let events = vec![created("ship it", 3)];
    let decision = next(Snapshot {
        events: &events,
        claim: None,
        running: false,
    });
    let Next::Open { identity, prompt } = decision else {
        panic!("a fresh goal must open a round, got {decision:?}");
    };
    assert_eq!(identity.round, 1);
    assert_eq!(identity.revision, 0);
    assert!(
        prompt.contains("ship it"),
        "the prompt must name the goal: {prompt}"
    );
}

#[test]
fn a_busy_host_is_told_to_wait_rather_than_double_spend() {
    let events = vec![created("ship it", 3)];
    let decision = next(Snapshot {
        events: &events,
        claim: None,
        running: true,
    });
    let Next::Busy { identity } = decision else {
        panic!("a running host must not be told to open another round: {decision:?}");
    };
    assert_eq!(identity.round, 1);
}

#[test]
fn a_claimed_round_keeps_going_while_the_fence_holds() {
    // The claim is taken *after* the continuation is written: a live round is one
    // the log already counts, which is what makes `rounds == round` the fence.
    let base = created("ship it", 3);
    let folded = fold(std::slice::from_ref(&base)).expect("created folds");
    let opened = goal_continued("s", &folded, 1);
    let events = vec![base, opened];
    let reservation = Reservation {
        goal_id: "g1".to_string(),
        revision: 1,
        round: 1,
    };
    let decision = next(Snapshot {
        events: &events,
        claim: Some(&reservation),
        running: true,
    });
    let Next::Running { identity } = decision else {
        panic!("a live claim must read as running: {decision:?}");
    };
    assert_eq!(identity.round, 1);
    assert_eq!(identity.revision, 1);
}

#[test]
fn a_claim_superseded_by_another_entry_steps_aside_without_closing_the_goal() {
    // Created at revision 0, then a continuation took it to revision 1. A caller
    // still holding the revision-0 claim is out of date.
    let base = created("ship it", 5);
    let folded = fold(std::slice::from_ref(&base)).expect("created folds");
    let advanced = goal_continued("s", &folded, 1);
    let events = vec![base, advanced];
    let stale = Reservation {
        goal_id: "g1".to_string(),
        revision: 0,
        round: 1,
    };

    let decision = next(Snapshot {
        events: &events,
        claim: Some(&stale),
        running: true,
    });
    let Next::Superseded { goal, detail } = decision else {
        panic!("a stale claim must step aside, got {decision:?}");
    };
    assert_eq!(
        goal.status,
        Status::Active,
        "stepping aside must not end a goal another entry is working on"
    );
    assert!(
        detail.contains("stale"),
        "the detail should say why: {detail}"
    );
}

#[test]
fn a_goal_that_ended_under_its_claim_reports_the_reason_it_ended() {
    let goal = Goal::new("g1", "ship it", 5);
    let base = goal_created("s", &goal);
    let ended = goal_ended("s", &goal, StopReason::Complete, Some("done"));
    let events = vec![base, ended];
    let claim = Reservation {
        goal_id: "g1".to_string(),
        revision: 0,
        round: 1,
    };

    let decision = next(Snapshot {
        events: &events,
        claim: Some(&claim),
        running: true,
    });
    let Next::Ended { reason, goal } = decision else {
        panic!("an ended goal must report Ended, got {decision:?}");
    };
    assert_eq!(reason, StopReason::Complete);
    assert_eq!(goal.status, Status::Complete);
}

#[test]
fn a_goal_whose_budget_is_spent_reports_exhausted_instead_of_opening() {
    // maxRounds 1 means one turn, which `goal/created` already spent.
    let events = vec![created("ship it", 1)];
    let decision = next(Snapshot {
        events: &events,
        claim: None,
        running: false,
    });
    let Next::Ended { reason, .. } = decision else {
        panic!("a spent budget must not open another round, got {decision:?}");
    };
    assert_eq!(reason, StopReason::Exhausted);
}

#[test]
fn the_reservation_helper_agrees_with_the_decision() {
    let events = vec![created("ship it", 3)];
    let snapshot = Snapshot {
        events: &events,
        claim: None,
        running: false,
    };
    let reservation =
        reservation_to_open(snapshot).expect("a fresh goal has a reservation to take");
    assert_eq!(reservation.round, 1);
    assert_eq!(reservation.revision, 0);

    let busy = Snapshot {
        events: &events,
        claim: None,
        running: true,
    };
    assert!(
        reservation_to_open(busy).is_none(),
        "a busy host has nothing to reserve"
    );
}

#[test]
fn a_payload_with_no_recognised_reason_does_not_close_the_goal() {
    // A future `goal/ended` reason this build does not know must be data, not an
    // instruction to stop (AGENTS.md rule 2).
    let created = Event::new(
        "s",
        EventType::GOAL_CREATED,
        Some(json!({ "goalId": "g1", "text": "x", "maxRounds": 4, "revision": 0 })),
    );
    let ended = Event::new(
        "s",
        EventType::GOAL_ENDED,
        Some(json!({ "goalId": "g1", "reason": "teleported", "revision": 0 })),
    );
    let events = vec![created, ended];
    let folded = fold(&events).expect("the goal still folds");
    assert_eq!(
        folded.status,
        Status::Active,
        "an unreadable reason must leave the goal running"
    );
    assert!(
        matches!(
            next(Snapshot {
                events: &events,
                claim: None,
                running: false
            }),
            Next::Open { .. }
        ),
        "so the loop keeps going instead of stopping on a word it cannot read"
    );
}
