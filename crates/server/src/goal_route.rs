//! The goal a session is working toward, as a read-only wire value.
//!
//! Layer L3. A goal is not server state: it is three `goal/*` lines in the
//! session log, and `hearth_goal::fold` is the state machine that turns them
//! back into a [`hearth_goal::Goal`]. This module carries that fold to the
//! browser and nothing else, so `GET /api/sessions/{id}/goal` answers on a box
//! that has never run a turn and on a session whose model provider is dead.
//!
//! Three rules for what belongs here:
//!
//! 1. **Read, do not create.** A missing log is [`Problem::NotFound`], never a
//!    file to open for append. `hearth_session::Store::open` would create the
//!    file it cannot find, so this module reads the bytes itself and parses
//!    them with `Event::parse_line`, the same way `fold_session` in
//!    `server.rs` does for the other read-only routes.
//! 2. **Report the decision, do not re-derive it.** `rounds` and `nextRound`
//!    come from `Goal::next_action`, so the round the browser shows is the
//!    round the turn loop would act on. `nextRound` is a **round number**, not
//!    the continuation index `Action::Continue` carries: `rounds` counts
//!    continuations, every other surface counts rounds, and `goal/created` is
//!    round 1. The two differ by one, and that one is a round the loop may
//!    never run.
//! 3. **An unreadable line is data.** `fold` skips a goal payload this build
//!    cannot project (AGENTS.md rule 2); this module skips an envelope it
//!    cannot parse. One torn byte never hides a good goal, and neither turns
//!    into a `500`.
//!
//! What it cannot say, it does not guess: a goal that is not active has no
//! `nextRound` member at all, because a number there would describe a round
//! nobody will run.

use std::fs;
use std::io;
use std::path::Path;

use hearth_core::Event;
use hearth_goal::{Action, Goal};
use serde::{Deserialize, Serialize};

/// Why [`read`] could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// There is no log at that path, or it holds no record this build can read.
    /// Both are the answer `GET /api/sessions/{id}` gives a session that does
    /// not exist, so the route maps this to `404 not_found`.
    NotFound,
    /// The log is there but its bytes could not be read. A route may not call
    /// that "no such session": the session may be perfectly fine.
    Read,
}

/// The wire shape of the answer: `{"goal": null}` or `{"goal": {...}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// Absent in the JSON sense: always present as a key, `null` when the
    /// session has never had a goal. A missing key would read as "this build
    /// does not answer that question", which is a different and untrue claim.
    pub goal: Option<GoalView>,
}

/// One goal, as the browser sees it. camelCase, like the rest of the API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalView {
    /// The id the goal events carry.
    pub id: String,
    /// What the operator asked to keep working on.
    pub text: String,
    /// The budget, in **turns**, that `goal/created` recorded.
    pub max_rounds: u32,
    /// Continuations already opened, counted from the log. Not turns: the first
    /// turn came with `goal/created`, so a goal that has continued twice is on
    /// its third turn and reports `2`.
    pub rounds: u32,
    /// `active`, `complete`, `blocked`, `cancelled` or `exhausted`.
    pub status: String,
    /// The round `Goal::next_action` would open, or no member at all when it
    /// would stop. This is deliberately `None` for an `active` goal whose
    /// budget is already spent: `fold` still says `active` because no
    /// `goal/ended` was written, and the honest answer is that no round comes
    /// next, not that round `rounds + 1` does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_round: Option<u32>,
}

/// Fold a list of events into the answer. Pure: no I/O, no clock, no lock.
pub fn report(events: &[Event]) -> Reply {
    Reply {
        goal: hearth_goal::fold(events).as_ref().map(view),
    }
}

/// Read one session log and fold its goal.
///
/// The caller owns the path (`Server::session_path` builds it from a validated
/// id in `server.rs`). This function opens the file for reading only; it never
/// creates one, appends nothing, and takes no lock.
pub fn read(path: &Path) -> Result<Reply, Problem> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Problem::NotFound
        } else {
            Problem::Read
        }
    })?;
    if !metadata.is_file() {
        return Err(Problem::NotFound);
    }
    let content = fs::read_to_string(path).map_err(|_| Problem::Read)?;
    let events = events_in(&content);
    if events.is_empty() {
        // A log with no record this build can read is "no such session"
        // everywhere else in this server (`fold_session`), and two routes
        // disagreeing about whether a session exists is worse than agreeing
        // about an empty file.
        return Err(Problem::NotFound);
    }
    Ok(report(&events))
}

/// The records of one log, skipping the lines this build cannot read.
fn events_in(content: &str) -> Vec<Event> {
    let mut events = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A torn or foreign line is data we do not understand, not a reason to
        // refuse the whole log (AGENTS.md rule 2). `seq == 0` is not a record
        // the store ever wrote; `fold_session` drops it too, and a zero here
        // would make a hand-edited line look like the first event.
        if let Ok(event) = Event::parse_line(line) {
            if event.seq == 0 {
                continue;
            }
            events.push(event);
        }
    }
    events
}

/// Project one folded goal, taking the next round from `next_action`.
///
/// `Action::Continue`'s member is the **continuation** index, not the round
/// number: `rounds` counts continuations while `max_rounds` and every surface
/// count rounds, and `goal/created` already spent round 1. So a goal with
/// `rounds: 0` is on round 1 and its next round is round 2 — `round + 1` here.
/// Reporting the raw member would promise a round the loop has already
/// numbered differently, and `hearth goal status` would print "next round 2"
/// while this route said "nextRound": 1 about the same log.
fn view(goal: &Goal) -> GoalView {
    let next_round = match goal.next_action() {
        Action::Continue { round } => Some(round + 1),
        // Every `Stop` reason — including `Exhausted` for a goal the log still
        // calls active — means no round is coming.
        Action::Stop { .. } => None,
    };
    GoalView {
        id: goal.id.clone(),
        text: goal.text.clone(),
        max_rounds: goal.max_rounds,
        rounds: goal.rounds,
        status: goal.status.as_str().to_string(),
        next_round,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hearth_core::EventType;
    use hearth_goal::{goal_continued, goal_created, goal_ended, StopReason};
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    const SESSION: &str = "s_01jya7c9q3r5t7v9w1x3y5z7ab";
    const TEXT: &str = "ship the goal route";

    fn created(max_rounds: u32) -> Event {
        goal_created(SESSION, &Goal::new("g1", TEXT, max_rounds))
    }

    fn continued(round: u32) -> Event {
        goal_continued(SESSION, &Goal::new("g1", TEXT, 10), round)
    }

    fn ended(reason: StopReason) -> Event {
        goal_ended(SESSION, &Goal::new("g1", TEXT, 10), reason, None)
    }

    fn encoded(reply: &Reply) -> String {
        serde_json::to_string(reply).expect("encode reply")
    }

    #[test]
    fn a_session_without_a_goal_answers_null() {
        assert_eq!(encoded(&report(&[])), r#"{"goal":null}"#);
        // A log that exists and holds other events is the same answer: "no
        // goal" is a fact, not an absence of an answer.
        let others = [Event::new(
            SESSION,
            EventType::SESSION_START,
            Some(json!({ "workspace": "D:/work/demo" })),
        )];
        assert_eq!(encoded(&report(&others)), r#"{"goal":null}"#);
    }

    #[test]
    fn an_active_goal_reports_the_round_the_loop_would_open_next() {
        let reply = report(&[created(10), continued(1), continued(2)]);
        assert_eq!(
            encoded(&reply),
            r#"{"goal":{"id":"g1","text":"ship the goal route","maxRounds":10,"rounds":2,"status":"active","nextRound":4}}"#
        );
    }

    #[test]
    fn an_exhausted_goal_has_no_next_round() {
        // `maxRounds: 2` is a budget of two **turns**: the first came with
        // `goal/created`, so one continuation spends it. `next_action` says
        // Stop with the log still calling the goal active, because the loop has
        // not written `goal/ended` yet.
        let spent = report(&[created(2), continued(1)]);
        assert_eq!(
            spent.goal.as_ref().map(|goal| goal.status.as_str()),
            Some("active")
        );
        assert_eq!(
            spent.goal.as_ref().and_then(|goal| goal.next_round),
            None,
            "a spent budget has no next round, whatever the status says"
        );
        assert_eq!(
            encoded(&spent),
            r#"{"goal":{"id":"g1","text":"ship the goal route","maxRounds":2,"rounds":1,"status":"active"}}"#
        );

        let closed = report(&[created(2), continued(1), ended(StopReason::Exhausted)]);
        assert_eq!(
            encoded(&closed),
            r#"{"goal":{"id":"g1","text":"ship the goal route","maxRounds":2,"rounds":1,"status":"exhausted"}}"#
        );
    }

    #[test]
    fn a_blocked_goal_has_no_next_round() {
        let reply = report(&[created(10), continued(1), ended(StopReason::Blocked)]);
        let goal = reply.goal.as_ref().expect("one goal");
        assert_eq!(goal.status, "blocked");
        assert_eq!(goal.rounds, 1);
        assert_eq!(goal.next_round, None);
        assert!(!encoded(&reply).contains("nextRound"));
    }

    #[test]
    fn a_goal_payload_this_build_cannot_read_is_skipped() {
        // No `goalId`: `fold` cannot tell whose goal it is.
        let anonymous = Event::new(
            SESSION,
            EventType::GOAL_CREATED,
            Some(json!({ "text": TEXT, "maxRounds": 10 })),
        );
        // A payload that is not even an object.
        let stringly = Event::new(SESSION, EventType::GOAL_CREATED, Some(json!("goal")));
        // An end reason this build does not know must not close the goal.
        let unknown_reason = Event::new(
            SESSION,
            EventType::GOAL_ENDED,
            Some(json!({ "goalId": "g1", "reason": "exploded" })),
        );
        assert_eq!(
            encoded(&report(&[anonymous.clone(), stringly])),
            r#"{"goal":null}"#
        );

        let surviving = report(&[anonymous, created(10), unknown_reason]);
        assert_eq!(
            encoded(&surviving),
            r#"{"goal":{"id":"g1","text":"ship the goal route","maxRounds":10,"rounds":0,"status":"active","nextRound":2}}"#
        );
    }

    // -----------------------------------------------------------------------
    // The file reader. These are the tests that would catch the one thing this
    // module could do that a read-only route may not: create a file.
    // -----------------------------------------------------------------------

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    struct TempLog(PathBuf);

    impl TempLog {
        fn new(label: &str) -> TempLog {
            TempLog(std::env::temp_dir().join(format!(
                "hearth-goal-route-{}-{}-{}.jsonl",
                label,
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            )))
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, events: &[Event]) -> &TempLog {
            let mut text = String::new();
            for (index, event) in events.iter().enumerate() {
                let mut record = event.clone();
                record.seq = index as u64 + 1;
                record.ts = 1_794_787_200_000 + index as i64;
                record.id = format!("{:032x}", index as u128 + 0xa1f0);
                text.push_str(&record.canonical().expect("canonical line"));
                text.push('\n');
            }
            fs::write(&self.0, text).expect("write log");
            self
        }

        fn append_raw(&self, text: &str) -> &TempLog {
            let mut existing = fs::read_to_string(&self.0).unwrap_or_default();
            existing.push_str(text);
            fs::write(&self.0, existing).expect("append raw");
            self
        }
    }

    impl Drop for TempLog {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_missing_log_is_not_found_and_is_still_missing() {
        let log = TempLog::new("missing");
        assert_eq!(read(log.path()), Err(Problem::NotFound));
        assert!(
            !log.path().exists(),
            "the route created {}",
            log.path().display()
        );
    }

    #[test]
    fn a_log_with_no_readable_record_is_not_found() {
        let log = TempLog::new("empty");
        fs::write(log.path(), "\n{\"v\":9}\nnot json\n").expect("write log");
        assert_eq!(read(log.path()), Err(Problem::NotFound));
    }

    #[test]
    fn a_torn_line_does_not_hide_the_goal() {
        let log = TempLog::new("torn");
        log.write(&[created(10), continued(1)])
            .append_raw("{\"v\":1,\"seq\":3,\"id\":\"torn");
        let reply = read(log.path()).expect("read log");
        let goal = reply.goal.expect("one goal");
        assert_eq!(goal.rounds, 1);
        assert_eq!(goal.next_round, Some(3));
    }

    #[test]
    fn a_malformed_goal_event_does_not_fail_the_read() {
        let log = TempLog::new("malformed");
        log.write(&[created(10)])
            .append_raw(&format!(
                "{{\"v\":1,\"seq\":{}, \"id\":\"bad\",\"ts\":1794787200001,\"session\":\"{SESSION}\",\"type\":\"goal/created\",\"payload\":{{\"text\":\"no id\"}}}}\n",
                2
            ));
        let reply = read(log.path()).expect("read log");
        let goal = reply.goal.expect("the good goal survives");
        assert_eq!(goal.id, "g1");
        assert_eq!(goal.max_rounds, 10);
        assert_eq!(goal.next_round, Some(2));
    }

    #[test]
    fn a_goal_folded_from_a_file_encodes_the_same_wire_words() {
        let log = TempLog::new("wire");
        log.write(&[created(2), continued(1), ended(StopReason::Complete)]);
        assert_eq!(
            encoded(&read(log.path()).expect("read log")),
            r#"{"goal":{"id":"g1","text":"ship the goal route","maxRounds":2,"rounds":1,"status":"complete"}}"#
        );
    }

    /// The payload shape `read` must tolerate is the one a future version may
    /// write, so at least one test reads a member type this build never writes.
    #[test]
    fn an_extra_member_is_ignored_not_refused() {
        let events = [Event::new(
            SESSION,
            EventType::GOAL_CREATED,
            Some(json!({
                "goalId": "g2",
                "text": "future",
                "maxRounds": 3,
                "warnings": ["unknown"]
            })),
        )];
        let reply = report(&events);
        assert_eq!(
            reply.goal.as_ref().and_then(|goal| goal.next_round),
            Some(2)
        );
        let _: Value = serde_json::to_value(reply).expect("encode");
    }
}
