//! A general listener surface for the loop's own events.
//!
//! The `hooks` module gives a hook the power to *change* a request. This module
//! gives a listener the power to *observe* one, plus every other event the loop
//! appends, without any say over what happens next:
//!
//! - [`Dispatch::emit`] is fire-and-forget: it calls every listener and returns
//!   nothing. A listener cannot veto an event, because the event has already
//!   been appended by the time it runs.
//! - [`Dispatch::serial`] runs its listeners in registration order and awaits
//!   each one, so a listener that needs to finish before the turn closes can.
//!
//! Two rules make the surface safe to hand to a plugin:
//!
//! 1. **One bad listener must not starve the rest.** A panicking listener is
//!    caught, recorded in [`Dispatch::problems`], and the next listener still
//!    runs. Cordis's native emit maps over callbacks, so one throw stops the
//!    chain; this module exists partly to not have that property.
//! 2. **Listeners cannot write the log.** They receive a borrowed view. The log
//!    has exactly one writer (AGENTS.md rule 1), and an observer that could
//!    append would be a second one.
//!
//! A listener that *does* need to change behaviour belongs in `hooks`, where the
//! change is a declared, recorded outcome (`hook/rewrite`).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use forge_core::EventType;
use serde_json::Value;

/// A borrowed view of one event, handed to listeners.
///
/// It is deliberately not `forge_core::Event`: a listener has no business with
/// the session id or the sequence number (it is being called *during* the
/// append), and a borrowed view makes it impossible to keep one.
#[derive(Debug, Clone, Copy)]
pub struct EventView<'a> {
    /// The type name, e.g. `tool/result`. Unknown types are passed through as
    /// their literal string (AGENTS.md rule 2: unknown types are data).
    pub typ: &'a str,
    pub turn: u32,
    pub step: u32,
    pub payload: Option<&'a Value>,
}

impl<'a> EventView<'a> {
    pub fn new(typ: &'a EventType, turn: u32, step: u32, payload: Option<&'a Value>) -> Self {
        Self {
            typ: typ.as_str(),
            turn,
            step,
            payload,
        }
    }

    /// True when this row is the given type.
    pub fn is(&self, typ: &EventType) -> bool {
        self.typ == typ.as_str()
    }
}

/// A listener. `Fn` rather than `FnMut`: listeners are called from several
/// threads' worth of call sites and must not need a lock to be re-entered.
pub type Listener = Arc<dyn Fn(&EventView<'_>) + Send + Sync>;

/// Per-listener outcomes that were swallowed so the rest could run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// `emit` or `serial`, so a noisy listener can be told from a quiet one.
    pub surface: &'static str,
    pub index: usize,
    pub message: String,
}

#[derive(Clone, Default)]
pub struct Dispatch {
    emit: Vec<Listener>,
    serial: Vec<Listener>,
    problems: Arc<Mutex<Vec<Problem>>>,
}

impl Dispatch {
    /// No listeners. Every method is then a no-op, so the loop's behaviour with
    /// nothing installed is byte for byte the loop this crate shipped before.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn on_emit(mut self, listener: Listener) -> Self {
        self.emit.push(listener);
        self
    }

    pub fn on_serial(mut self, listener: Listener) -> Self {
        self.serial.push(listener);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.emit.is_empty() && self.serial.is_empty()
    }

    pub fn len(&self) -> usize {
        self.emit.len() + self.serial.len()
    }

    /// Call every `emit` listener. Cannot fail and cannot veto: the event has
    /// already landed on the log.
    pub fn emit(&self, view: &EventView<'_>) {
        self.run("emit", &self.emit, view);
    }

    /// Call every `serial` listener in registration order. Used at the points
    /// where a listener may need to finish before the loop moves on.
    pub fn serial(&self, view: &EventView<'_>) {
        self.run("serial", &self.serial, view);
    }

    /// The swallowed listener failures so far, oldest first. Callers surface
    /// these rather than dropping them: a listener that throws every time is a
    /// silent outage otherwise.
    pub fn problems(&self) -> Vec<Problem> {
        match self.problems.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn run(&self, surface: &'static str, listeners: &[Listener], view: &EventView<'_>) {
        for (index, listener) in listeners.iter().enumerate() {
            // `AssertUnwindSafe`: the listener is a plain `Fn` over borrowed
            // data and owns nothing this module hands back, so a panic inside
            // it cannot leave a half-updated structure behind. The whole point
            // of catching is that the next listener still runs.
            let outcome = catch_unwind(AssertUnwindSafe(|| listener(view)));
            if let Err(payload) = outcome {
                let message = panic_message(&payload);
                if let Ok(mut guard) = self.problems.lock() {
                    guard.push(Problem {
                        surface,
                        index,
                        message,
                    });
                }
            }
        }
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "listener panicked".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view<'a>(typ: &'a EventType, payload: &'a Value) -> EventView<'a> {
        EventView::new(typ, 1, 1, Some(payload))
    }

    #[test]
    fn an_emit_listener_sees_every_event() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let dispatch = Dispatch::none().on_emit(Arc::new(move |v: &EventView<'_>| {
            sink.lock().unwrap().push(v.typ.to_string());
        }));
        let payload = json!({"ok": true});
        dispatch.emit(&view(&EventType::TOOL_RESULT, &payload));
        dispatch.emit(&view(&EventType::TURN_END, &payload));
        assert_eq!(*seen.lock().unwrap(), vec!["tool/result", "turn/end"]);
    }

    #[test]
    fn a_panicking_listener_does_not_starve_the_next_one() {
        let hits = Arc::new(Mutex::new(0usize));
        let sink = Arc::clone(&hits);
        let dispatch = Dispatch::none()
            .on_emit(Arc::new(|_: &EventView<'_>| panic!("boom")))
            .on_emit(Arc::new(move |_: &EventView<'_>| {
                *sink.lock().unwrap() += 1;
            }));
        let payload = json!({});
        dispatch.emit(&view(&EventType::TURN_END, &payload));
        assert_eq!(*hits.lock().unwrap(), 1, "the second listener still ran");
        let problems = dispatch.problems();
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].surface, "emit");
        assert_eq!(problems[0].index, 0);
        assert!(problems[0].message.contains("boom"));
    }

    #[test]
    fn serial_listeners_run_in_registration_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let a = Arc::clone(&order);
        let b = Arc::clone(&order);
        let dispatch = Dispatch::none()
            .on_serial(Arc::new(move |_: &EventView<'_>| {
                a.lock().unwrap().push("first");
            }))
            .on_serial(Arc::new(move |_: &EventView<'_>| {
                b.lock().unwrap().push("second");
            }));
        let payload = json!({});
        dispatch.serial(&view(&EventType::TURN_END, &payload));
        assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    }

    #[test]
    fn an_empty_dispatch_is_inert() {
        let dispatch = Dispatch::none();
        assert!(dispatch.is_empty());
        assert_eq!(dispatch.len(), 0);
        let payload = json!({});
        dispatch.emit(&view(&EventType::TURN_END, &payload));
        dispatch.serial(&view(&EventType::TURN_END, &payload));
        assert!(dispatch.problems().is_empty());
    }

    #[test]
    fn a_view_recognises_its_own_type_and_carries_the_envelope() {
        let payload = json!({"x": 1});
        let view = EventView::new(&EventType::TOOL_RESULT, 3, 2, Some(&payload));
        assert!(view.is(&EventType::TOOL_RESULT));
        assert!(!view.is(&EventType::TOOL_CALL));
        assert_eq!(view.turn, 3);
        assert_eq!(view.step, 2);
        assert_eq!(view.payload.unwrap()["x"], json!(1));
    }
}
