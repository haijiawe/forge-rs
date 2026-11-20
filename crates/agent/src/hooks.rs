//! The loop's three waterfall seams: `pre-step`, `request`, and `request-error`.
//! See [ADR-0004](../../docs/11-adr-agent-hooks.md).
//!
//! A hook is opt-in and logged. With no hook installed the loop is byte-for-byte
//! the loop it was; a rewrite is only ever reached when a caller sets
//! [`Options::hooks`](crate::Options::hooks), and every rewrite that changes a
//! request writes a `hook/rewrite` event naming the phase and the plugin. That
//! log line is the auditable stand-in for "the loop does not rewrite requests".
//!
//! `pre-step` and `request` rewrite; `request-error` does not rewrite — it decides
//! whether a failed request is retried, the one thing the transport policy alone
//! cannot know (a hook may know an error is transient when its code does not say
//! so). It never writes a `hook/rewrite` line because it changes nothing durable.

use std::sync::Arc;

use forge_core::{Error, ErrorKind, Result};
use forge_llm::{Message, Request, ToolSpec};

/// The closure a [`Hook`] runs. Factored into an alias so the field's type is a
/// name rather than a `dyn Fn(...)` spelling repeated at every mention.
pub type HookFn = Arc<dyn for<'a, 'b> Fn(&'a HookCall<'b>) -> Result<HookOutcome> + Send + Sync>;

/// The closed set of phases a hook may run in. A phase outside this list is
/// refused by name, the way `crates/plugin/src/protocol.rs` refuses an unknown
/// hook phase rather than ignoring it.
pub const HOOK_PHASES: &[&str] = &["pre-step", "request", "request-error"];

/// Whether `phase` is one the loop knows how to run.
pub fn is_hook_phase(phase: &str) -> bool {
    HOOK_PHASES.contains(&phase)
}

/// What a hook is handed, per phase. Borrowed so a hook can read everything it
/// needs without the loop cloning the whole history for a hook that may not
/// change anything.
#[derive(Debug)]
pub enum HookCall<'a> {
    /// `pre-step`: the message list the provider is about to receive.
    PreStep {
        turn: u32,
        step: u32,
        messages: &'a [Message],
    },
    /// `request`: the fully built request, before it is sent.
    Request {
        turn: u32,
        step: u32,
        request: &'a Request,
    },
    /// `request-error`: a request attempt failed and the transport policy alone
    /// would give up. The hook sees the failure and may ask for a retry.
    RequestError {
        turn: u32,
        step: u32,
        attempt: u32,
        message: &'a str,
    },
}

/// What a hook returns. Each variant is the whole replacement for that phase's
/// subject; a hook that wants to change nothing returns the same value it was
/// given ([`HookOutcome::Unchanged`] makes that explicit and cheap).
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// The hook did not change anything. Cheap: no event is written.
    Unchanged,
    /// `pre-step`: replace the message list.
    Messages(Vec<Message>),
    /// `request`: replace the request's messages and tool descriptions. The model
    /// id is deliberately not part of the replacement — selecting a model is its
    /// own operation, never a rewrite.
    Request {
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    },
    /// `request-error`: ask the loop to retry the failed request. Only meaningful
    /// in that phase; the rewrite phases reject it as a wrong-phase answer.
    Retry,
}

/// One registered hook: a name (the plugin name, for the log line) and the
/// closure. `Send + Sync` because the loop runs one turn on one thread but a
/// caller may share the hook across turns.
pub struct Hook {
    pub name: String,
    pub run: HookFn,
}

impl Hook {
    /// A hook from a name and a closure.
    pub fn new<F>(name: impl Into<String>, run: F) -> Self
    where
        F: Fn(&HookCall<'_>) -> Result<HookOutcome> + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            run: Arc::new(run),
        }
    }
}

impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hook").field("name", &self.name).finish()
    }
}

/// The set of hooks the loop consults, in install order. Empty is the default and
/// means "no seam": the loop skips both call sites entirely.
#[derive(Default, Clone)]
pub struct Hooks {
    installed: Vec<Arc<Hook>>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks")
            .field("installed", &self.installed.len())
            .finish()
    }
}

impl Hooks {
    /// No hooks: the inert default.
    pub fn none() -> Self {
        Self::default()
    }

    /// Install one hook (appended, so later hooks see earlier hooks' changes).
    pub fn push(&mut self, hook: Hook) {
        self.installed.push(Arc::new(hook));
    }

    /// Whether any hook is installed. The loop checks this to keep the inert path
    /// allocation-free.
    pub fn is_empty(&self) -> bool {
        self.installed.is_empty()
    }

    /// Number of installed hooks.
    pub fn len(&self) -> usize {
        self.installed.len()
    }

    /// Run every installed hook in order for one `pre-step` phase. The first hook
    /// that returns [`HookOutcome::Messages`] replaces the list for the next hook;
    /// a hook that returns [`HookOutcome::Unchanged`] or a variant for the other
    /// phase is a no-op for this phase.
    ///
    /// Returns the final message list and the names of the hooks that actually
    /// changed it (empty when nothing changed), so the caller can log a rewrite.
    /// An error from a hook is propagated as `hook_failed`; a half-applied rewrite
    /// is worse than none, so the caller aborts the step.
    pub fn run_pre_step(
        &self,
        turn: u32,
        step: u32,
        messages: Vec<Message>,
    ) -> Result<(Vec<Message>, Vec<String>)> {
        let mut current = messages;
        let mut changed: Vec<String> = Vec::new();
        for hook in &self.installed {
            let call = HookCall::PreStep {
                turn,
                step,
                messages: &current,
            };
            match (hook.run)(&call).map_err(|e| hook_failed(&hook.name, e))? {
                HookOutcome::Unchanged => {}
                HookOutcome::Messages(next) => {
                    changed.push(hook.name.clone());
                    current = next;
                }
                HookOutcome::Request { .. } => {
                    return Err(hook_wrong_phase(&hook.name, "pre-step", "request"));
                }
                HookOutcome::Retry => {
                    return Err(hook_wrong_phase(
                        &hook.name,
                        "pre-step",
                        "request-error retry",
                    ));
                }
            }
        }
        Ok((current, changed))
    }

    /// Run every installed hook in order for one `request` phase. Only the
    /// messages and tool descriptions may be replaced; the model id is copied from
    /// the request the loop built and cannot be changed by a hook.
    ///
    /// Returns the (possibly rewritten) request — with its model id restored from
    /// the input — and the names of the hooks that changed it.
    pub fn run_request(
        &self,
        turn: u32,
        step: u32,
        request: Request,
    ) -> Result<(Request, Vec<String>)> {
        let model = request.model.clone();
        let mut current = request;
        let mut changed: Vec<String> = Vec::new();
        for hook in &self.installed {
            let call = HookCall::Request {
                turn,
                step,
                request: &current,
            };
            match (hook.run)(&call).map_err(|e| hook_failed(&hook.name, e))? {
                HookOutcome::Unchanged => {}
                HookOutcome::Request { messages, tools } => {
                    // Rebuild from the request the loop built so every field a hook
                    // may not touch (model, and any future field) is preserved.
                    let mut next = Request {
                        model: model.clone(),
                        messages,
                        tools,
                        ..Request::default()
                    };
                    next.model = model.clone();
                    current = next;
                    changed.push(hook.name.clone());
                }
                HookOutcome::Messages(_) => {
                    return Err(hook_wrong_phase(&hook.name, "request", "pre-step"));
                }
                HookOutcome::Retry => {
                    return Err(hook_wrong_phase(
                        &hook.name,
                        "request",
                        "request-error retry",
                    ));
                }
            }
        }
        Ok((current, changed))
    }

    /// Run every installed hook for one `request-error` phase and report whether
    /// any of them asked for a retry. The loop only reaches this after its own
    /// transport policy has decided to give up (a non-retryable code, a spent retry
    /// budget, or text already streamed), so `true` here means "override the
    /// policy and try again".
    ///
    /// A hook that answers with a rewrite outcome is a wrong-phase error; a hook
    /// that errors is propagated as `hook_failed`. The first hook to answer decides.
    pub fn run_request_error(
        &self,
        turn: u32,
        step: u32,
        attempt: u32,
        message: &str,
    ) -> Result<bool> {
        for hook in &self.installed {
            let call = HookCall::RequestError {
                turn,
                step,
                attempt,
                message,
            };
            match (hook.run)(&call).map_err(|e| hook_failed(&hook.name, e))? {
                HookOutcome::Unchanged => {}
                HookOutcome::Retry => return Ok(true),
                HookOutcome::Messages(_) => {
                    return Err(hook_wrong_phase(&hook.name, "request-error", "pre-step"));
                }
                HookOutcome::Request { .. } => {
                    return Err(hook_wrong_phase(&hook.name, "request-error", "request"));
                }
            }
        }
        Ok(false)
    }
}

fn hook_failed(name: &str, error: Error) -> Error {
    Error::new(
        ErrorKind::Invalid,
        format!("agent: hook {name:?} failed: {}", error.message()),
    )
}

fn hook_wrong_phase(name: &str, ran_as: &str, returned: &str) -> Error {
    Error::new(
        ErrorKind::Invalid,
        format!(
            "agent: hook {name:?} ran as {ran_as} but returned a {returned} outcome; a hook must \
             answer the phase it was called in"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_llm::Message;

    fn user(text: &str) -> Message {
        Message::user(text)
    }

    #[test]
    fn the_phase_set_is_closed_and_named() {
        assert!(is_hook_phase("pre-step"));
        assert!(is_hook_phase("request"));
        assert!(is_hook_phase("request-error"));
        assert!(!is_hook_phase("during-breakfast"));
    }

    #[test]
    fn a_request_error_hook_can_ask_for_a_retry() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("flaky", |call| match call {
            HookCall::RequestError { attempt, .. } => {
                if *attempt == 0 {
                    Ok(HookOutcome::Retry)
                } else {
                    Ok(HookOutcome::Unchanged)
                }
            }
            _ => Ok(HookOutcome::Unchanged),
        }));
        assert!(hooks.run_request_error(1, 1, 0, "temporary").unwrap());
        assert!(!hooks.run_request_error(1, 1, 1, "still down").unwrap());
    }

    #[test]
    fn a_request_error_hook_that_stays_quiet_does_not_retry() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("quiet", |_call| Ok(HookOutcome::Unchanged)));
        assert!(!hooks.run_request_error(1, 1, 0, "bad").unwrap());
    }

    #[test]
    fn a_rewrite_outcome_in_the_request_error_phase_is_an_error() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("confused", |_call| {
            Ok(HookOutcome::Messages(vec![user("nope")]))
        }));
        let err = hooks.run_request_error(1, 1, 0, "bad").unwrap_err();
        assert!(err.message().contains("confused"), "{}", err.message());
        assert!(err.message().contains("request-error"), "{}", err.message());
    }

    #[test]
    fn no_hooks_is_inert() {
        let hooks = Hooks::none();
        assert!(hooks.is_empty());
        let msgs = vec![user("hi")];
        let (out, changed) = hooks.run_pre_step(1, 1, msgs.clone()).unwrap();
        assert_eq!(out, msgs);
        assert!(changed.is_empty());
    }

    #[test]
    fn a_pre_step_hook_that_changes_nothing_reports_no_change() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("quiet", |_call| Ok(HookOutcome::Unchanged)));
        let msgs = vec![user("hi")];
        let (out, changed) = hooks.run_pre_step(1, 1, msgs.clone()).unwrap();
        assert_eq!(out, msgs);
        assert!(changed.is_empty());
    }

    #[test]
    fn a_pre_step_hook_rewrites_the_message_list_and_is_named() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("redactor", |call| match call {
            HookCall::PreStep { messages, .. } => {
                let mut next: Vec<Message> = messages.to_vec();
                for m in &mut next {
                    m.content = m.content.replace("secret", "[redacted]");
                }
                Ok(HookOutcome::Messages(next))
            }
            _ => Ok(HookOutcome::Unchanged),
        }));
        let msgs = vec![user("the secret is 42")];
        let (out, changed) = hooks.run_pre_step(1, 1, msgs).unwrap();
        assert_eq!(out[0].content, "the [redacted] is 42");
        assert_eq!(changed, vec!["redactor".to_string()]);
    }

    #[test]
    fn later_hooks_see_earlier_hooks_changes() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("first", |_call| {
            Ok(HookOutcome::Messages(vec![user("from first")]))
        }));
        hooks.push(Hook::new("second", |call| match call {
            HookCall::PreStep { messages, .. } => {
                assert_eq!(messages[0].content, "from first");
                Ok(HookOutcome::Messages(vec![user("from second")]))
            }
            _ => Ok(HookOutcome::Unchanged),
        }));
        let (out, changed) = hooks.run_pre_step(1, 1, vec![user("original")]).unwrap();
        assert_eq!(out[0].content, "from second");
        assert_eq!(changed, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn a_request_hook_cannot_change_the_model() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("sneaky", |call| match call {
            HookCall::Request { .. } => Ok(HookOutcome::Request {
                messages: vec![user("rewritten")],
                tools: Vec::new(),
            }),
            _ => Ok(HookOutcome::Unchanged),
        }));
        let req = Request {
            model: "the-real-model".into(),
            messages: vec![user("original")],
            ..Request::default()
        };
        let (out, changed) = hooks.run_request(1, 1, req).unwrap();
        assert_eq!(out.model, "the-real-model");
        assert_eq!(out.messages[0].content, "rewritten");
        assert_eq!(changed, vec!["sneaky".to_string()]);
    }

    #[test]
    fn a_hook_that_answers_the_wrong_phase_is_an_error() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("confused", |_call| {
            Ok(HookOutcome::Request {
                messages: Vec::new(),
                tools: Vec::new(),
            })
        }));
        let err = hooks.run_pre_step(1, 1, vec![user("hi")]).unwrap_err();
        assert!(err.message().contains("confused"), "{}", err.message());
        assert!(err.message().contains("pre-step"), "{}", err.message());
    }

    #[test]
    fn a_hook_that_fails_aborts_with_its_name() {
        let mut hooks = Hooks::none();
        hooks.push(Hook::new("broken", |_call| {
            Err(Error::new(ErrorKind::Invalid, "boom"))
        }));
        let err = hooks.run_pre_step(1, 1, vec![user("hi")]).unwrap_err();
        assert!(err.message().contains("broken"), "{}", err.message());
        assert!(err.message().contains("boom"), "{}", err.message());
    }
}
