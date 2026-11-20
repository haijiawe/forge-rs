//! The two durable input queues: `next-turn` and `next-step`.
//!
//! A message handed to a busy agent has to go *somewhere* that survives a
//! restart. Two shapes are useful, and they are different promises:
//!
//! - **next-step**: deliver before the agent's next model call. This is how a
//!   running turn is steered without starting another one.
//! - **next-turn**: deliver when the current turn is over, as the prompt of a
//!   fresh turn.
//!
//! Both live in the log, not in memory. The queue is a fold of `agent/inbox`
//! rows, so a second process sees the same pending work and a crash cannot lose
//! a message that was already accepted. The in-process deque a synchronous loop
//! would otherwise use is exactly the second copy of the truth that AGENTS.md
//! rule 1 forbids.
//!
//! The row is a splice, not an append-only list of additions, because a queue
//! has to express removal too:
//!
//! ```text
//! {"target":"next-turn","start":0,"removed":1,"inserted":[{"id":"...","text":"..."}]}
//! ```
//!
//! `start` and `removed` describe the slice taken out of the existing queue and
//! `inserted` is what goes in its place — so appending is `{start: len,
//! removed: 0}`, claiming the head is `{start: 0, removed: 1, inserted: []}`,
//! and clearing is `{start: 0, removed: len}`. One shape covers every operation,
//! which is why a reader does not need to know which verb the writer had in
//! mind.

use forge_core::{new_id, Event, EventType, Result};
use serde_json::{json, Value};

/// Which queue a message goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    NextTurn,
    NextStep,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::NextTurn => "next-turn",
            Target::NextStep => "next-step",
        }
    }

    pub fn parse(text: &str) -> Option<Target> {
        match text {
            "next-turn" => Some(Target::NextTurn),
            "next-step" => Some(Target::NextStep),
            // An unknown target is skipped by the fold rather than guessed at:
            // putting a message in the wrong queue changes when the model sees
            // it, and that is not a decision a reader gets to make.
            _ => None,
        }
    }
}

/// One pending message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg {
    pub id: String,
    pub text: String,
}

impl Msg {
    /// A new message. The id comes from `forge_core::new_id`, which can fail if
    /// the platform entropy source does, so this returns a `Result` rather than
    /// inventing an id: two prompts with the same words must stay distinct rows.
    pub fn new(text: impl Into<String>) -> Result<Msg> {
        Ok(Msg {
            id: new_id()?,
            text: text.into(),
        })
    }
}

/// The folded queues.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inbox {
    pub next_turn: Vec<Msg>,
    pub next_step: Vec<Msg>,
}

impl Inbox {
    pub fn is_empty(&self) -> bool {
        self.next_turn.is_empty() && self.next_step.is_empty()
    }

    pub fn len(&self) -> usize {
        self.next_turn.len() + self.next_step.len()
    }

    pub fn queue(&self, target: Target) -> &[Msg] {
        match target {
            Target::NextTurn => &self.next_turn,
            Target::NextStep => &self.next_step,
        }
    }
}

/// Build one splice row. `start`/`removed` address what is already there;
/// `inserted` is what replaces it.
pub fn splice(
    session: &str,
    target: Target,
    start: usize,
    removed: usize,
    inserted: &[Msg],
) -> Event {
    let inserted: Vec<Value> = inserted
        .iter()
        .map(|m| json!({"id": m.id, "text": m.text}))
        .collect();
    let mut ev = Event::new(
        session,
        EventType::AGENT_INBOX,
        Some(json!({
            "target": target.as_str(),
            "start": start,
            "removed": removed,
            "inserted": inserted,
        })),
    );
    // The queue belongs to no turn or step, so the envelope leaves both zero.
    ev.turn = 0;
    ev.step = 0;
    ev
}

/// Append a message to a queue.
pub fn enqueue(session: &str, target: Target, existing: &Inbox, msg: &Msg) -> Event {
    let at = existing.queue(target).len();
    splice(session, target, at, 0, std::slice::from_ref(msg))
}

/// Remove the head of a queue, if it has one.
pub fn claim(session: &str, target: Target, existing: &Inbox) -> Option<Event> {
    if existing.queue(target).is_empty() {
        return None;
    }
    Some(splice(session, target, 0, 1, &[]))
}

/// Drop everything pending in both queues.
pub fn clear(session: &str, existing: &Inbox) -> Vec<Event> {
    let mut out = Vec::new();
    for target in [Target::NextTurn, Target::NextStep] {
        let len = existing.queue(target).len();
        if len > 0 {
            out.push(splice(session, target, 0, len, &[]));
        }
    }
    out
}

/// Fold the queue back out of the log.
///
/// A splice that does not fit the queue it names (`start` past the end, or
/// `removed` reaching past the end) is **skipped, not clamped**: clamping would
/// silently delete a message the writer did not mean to delete, and a queue that
/// loses prompts is worse than a queue that misses one update.
pub fn fold(events: &[Event]) -> Inbox {
    let mut inbox = Inbox::default();
    for ev in events {
        if ev.typ != EventType::AGENT_INBOX.as_str() {
            continue;
        }
        let Some(payload) = ev.payload.as_ref() else {
            continue;
        };
        let Some(target) = payload
            .get("target")
            .and_then(Value::as_str)
            .and_then(Target::parse)
        else {
            continue;
        };
        let start = payload.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
        let removed = payload.get("removed").and_then(Value::as_u64).unwrap_or(0) as usize;
        let inserted: Vec<Msg> = payload
            .get("inserted")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let id = row.get("id").and_then(Value::as_str)?;
                        let text = row.get("text").and_then(Value::as_str)?;
                        Some(Msg {
                            id: id.to_string(),
                            text: text.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let queue = match target {
            Target::NextTurn => &mut inbox.next_turn,
            Target::NextStep => &mut inbox.next_step,
        };
        if start > queue.len() || start + removed > queue.len() {
            continue;
        }
        queue.splice(start..start + removed, inserted);
    }
    inbox
}

/// Fold from a store, for callers that only have the log.
pub fn fold_store(
    events: impl IntoIterator<Item = Result<Event>>,
) -> (Inbox, Option<forge_core::Error>) {
    let mut inbox = Inbox::default();
    for row in events {
        match row {
            Ok(ev) => {
                let one = fold(std::slice::from_ref(&ev));
                // Merge rather than re-fold: a splice was already validated
                // against its own queue above, and this keeps the error path
                // for a torn log separate from the fold itself.
                inbox.next_turn.extend(one.next_turn);
                inbox.next_step.extend(one.next_step);
            }
            Err(e) => return (inbox, Some(e)),
        }
    }
    (inbox, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(rows: Vec<Event>) -> Vec<Event> {
        rows
    }

    #[test]
    fn enqueue_then_claim_folds_to_an_empty_queue() {
        let mut events = Vec::new();
        let mut inbox = Inbox::default();
        let ev = enqueue(
            "s",
            Target::NextStep,
            &inbox,
            &Msg::new("steer me").expect("id"),
        );
        events.push(ev.clone());
        inbox = fold(&log(events.clone()));
        assert_eq!(inbox.next_step.len(), 1);
        assert_eq!(inbox.next_step[0].text, "steer me");

        let claim = claim("s", Target::NextStep, &inbox).expect("a pending message");
        events.push(claim);
        let after = fold(&events);
        assert!(after.is_empty(), "claiming the head empties the queue");
    }

    #[test]
    fn the_two_queues_are_independent() {
        let mut events = Vec::new();
        let mut inbox = Inbox::default();
        events.push(enqueue(
            "s",
            Target::NextTurn,
            &inbox,
            &Msg::new("next turn").expect("id"),
        ));
        inbox = fold(&events);
        events.push(enqueue(
            "s",
            Target::NextTurn,
            &inbox,
            &Msg::new("also next turn").expect("id"),
        ));
        inbox = fold(&events);
        events.push(enqueue(
            "s",
            Target::NextStep,
            &inbox,
            &Msg::new("next step").expect("id"),
        ));
        let inbox = fold(&events);

        assert_eq!(inbox.next_turn.len(), 2);
        assert_eq!(inbox.next_step.len(), 1);
        assert_eq!(inbox.len(), 3);

        let claim_step = claim("s", Target::NextStep, &inbox).expect("pending");
        events.push(claim_step);
        let after = fold(&events);
        assert_eq!(after.next_turn.len(), 2, "claiming a step left turns alone");
        assert!(after.next_step.is_empty());
    }

    #[test]
    fn clear_empties_both_queues() {
        let mut events = Vec::new();
        let mut inbox = Inbox::default();
        events.push(enqueue(
            "s",
            Target::NextTurn,
            &inbox,
            &Msg::new("a").expect("id"),
        ));
        inbox = fold(&events);
        events.push(enqueue(
            "s",
            Target::NextStep,
            &inbox,
            &Msg::new("b").expect("id"),
        ));
        let inbox = fold(&events);

        for ev in clear("s", &inbox) {
            events.push(ev);
        }
        let after = fold(&events);
        assert!(after.is_empty());
    }

    #[test]
    fn a_splice_that_does_not_fit_is_skipped_not_clamped() {
        // Removing two rows from an empty queue would, under clamping, look
        // like "remove nothing" and pass silently. It must be ignored instead,
        // so the queue never claims to have lost something it never had.
        let ev = splice("s", Target::NextTurn, 0, 2, &[]);
        let inbox = fold(&[ev]);
        assert!(inbox.is_empty());
    }

    #[test]
    fn an_unknown_target_is_skipped() {
        let mut ev = splice("s", Target::NextTurn, 0, 0, &[Msg::new("x").expect("id")]);
        ev.payload =
            Some(json!({"target": "somewhere-else", "start": 0, "removed": 0, "inserted": []}));
        let inbox = fold(&[ev]);
        assert!(
            inbox.is_empty(),
            "a queue this build does not know is not guessed at"
        );
    }

    #[test]
    fn inserting_in_the_middle_keeps_order() {
        let mut events = Vec::new();
        let mut inbox = Inbox::default();
        events.push(enqueue(
            "s",
            Target::NextStep,
            &inbox,
            &Msg::new("first").expect("id"),
        ));
        inbox = fold(&events);
        events.push(enqueue(
            "s",
            Target::NextStep,
            &inbox,
            &Msg::new("third").expect("id"),
        ));
        // Insert "second" between them.
        events.push(splice(
            "s",
            Target::NextStep,
            1,
            0,
            &[Msg::new("second").expect("id")],
        ));
        let inbox = fold(&events);
        let texts: Vec<&str> = inbox.next_step.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[test]
    fn a_msg_gets_a_fresh_id_each_time() {
        let a = Msg::new("same text").expect("id");
        let b = Msg::new("same text").expect("id");
        assert_ne!(
            a.id, b.id,
            "two prompts with the same words are two messages"
        );
        assert_eq!(a.id.len(), 32);
    }
}
