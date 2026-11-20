//! The goal race fence is a pure comparison over the log; these tests pin the
//! three answers a checkpoint can get.

use forge_goal::{
    fence, fold, goal_continued, goal_created, goal_ended, FenceOutcome, Goal, Reservation, Status,
    StopReason,
};

fn goal() -> Goal {
    Goal::new("g1", "ship the fence", 3)
}

#[test]
fn a_fresh_round_is_current() {
    let goal = goal();
    let events = vec![goal_created("s", &goal)];
    let folded = fold(&events).expect("folds");
    let reservation = Reservation {
        goal_id: folded.id.clone(),
        revision: folded.revision,
        round: folded.rounds,
    };
    assert_eq!(fence(&events, &reservation), FenceOutcome::Valid);
}

#[test]
fn another_entry_opening_the_next_round_makes_ours_stale() {
    let goal = goal();
    let mut events = vec![goal_created("s", &goal)];
    let folded = fold(&events).expect("folds");
    // Another entry opened round 1 (revision 1). Our reservation still says round 0.
    events.push(goal_continued("s", &folded, 1));
    let reservation = Reservation {
        goal_id: folded.id.clone(),
        revision: 0,
        round: 0,
    };
    assert_eq!(
        fence(&events, &reservation),
        FenceOutcome::Stale {
            revision: 1,
            rounds: 1
        }
    );
}

#[test]
fn an_ended_goal_stops_the_round() {
    let goal = goal();
    let mut events = vec![goal_created("s", &goal)];
    let folded = fold(&events).expect("folds");
    events.push(goal_ended("s", &folded, StopReason::Cancelled, None));
    let reservation = Reservation {
        goal_id: folded.id.clone(),
        revision: 0,
        round: 0,
    };
    assert_eq!(
        fence(&events, &reservation),
        FenceOutcome::Ended {
            status: Status::Cancelled
        }
    );
}

#[test]
fn a_log_with_no_goal_is_stale() {
    let reservation = Reservation {
        goal_id: "g1".to_string(),
        revision: 0,
        round: 0,
    };
    assert_eq!(
        fence(&[], &reservation),
        FenceOutcome::Stale {
            revision: 0,
            rounds: 0
        }
    );
}
