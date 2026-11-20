//! Who may interrupt whom.
//!
//! The cross-process interrupt in [`crate::cancel_file`] is a file write: any
//! process that can write `<data_dir>/sessions` can drop `<child>.cancel` and
//! stop that child. That is the transport, not the policy. This module is the
//! policy, ported from DSH `packages/subagent/subagent/src/manager.ts`
//! (`interrupt(targetSessionId, authority)`): an operator may always interrupt,
//! and a session may interrupt only a descendant of itself — never itself, never
//! an ancestor, never an unrelated session.
//!
//! The ancestor chain is not stored anywhere. It is rebuilt on demand from the
//! one parent link the log has: the `parent` member of `session/start` on each
//! session's own log (AGENTS.md law 1 — the log is the only truth). A session
//! whose head has no `parent` is the root of the tree.
//!
//! Failing closed is the point: every read that cannot answer (a missing log, a
//! log with no `session/start`, a forged cycle) is a [`Verdict::Deny`], because
//! the wrong direction here is a silent, unaccountable kill.

use std::collections::BTreeSet;
use std::path::Path;

use hearth_core::event::EventType;
use hearth_core::{Error, ErrorKind, Result};

use crate::child::{child_log_path, is_child_id};
use crate::list::read_events;
use crate::names::ROOT_AGENT_NAME;

/// How many hops up the tree a walk may take. A real delegation depth is capped
/// by `DEFAULT_MAX_DEPTH`; exceeding this many links means the chain is not a
/// chain, so the walk refuses instead of spinning.
const MAX_ANCESTRY: usize = 64;

/// Who is asking for the interrupt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The operator in front of the CLI, or anything holding a user token.
    /// Always allowed.
    User,
    /// A session is asking on its own behalf.
    Session(String),
}

impl Caller {
    /// The session id behind this caller, if there is one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Caller::User => None,
            Caller::Session(id) => Some(id),
        }
    }

    /// A stable tag for logs and payloads: `user` or `session:<id>`.
    pub fn tag(&self) -> String {
        match self {
            Caller::User => "user".to_string(),
            Caller::Session(id) => format!("session:{id}"),
        }
    }
}

/// The answer to "may this caller interrupt that child?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Refused, with one sentence that can be shown to the user as-is (no
    /// `subagent:` prefix — the CLI adds its own).
    Deny(String),
}

impl Verdict {
    pub fn is_allow(&self) -> bool {
        matches!(self, Verdict::Allow)
    }

    /// The refusal reason, when refused.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Allow => None,
            Verdict::Deny(reason) => Some(reason),
        }
    }
}

/// The `parent` recorded by the `session/start` of `session_id`, or `None` when
/// that session is the root (no `parent`, or an empty one).
fn parent_of(data_dir: &Path, session_id: &str) -> Result<Option<String>> {
    let path = child_log_path(data_dir, session_id);
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("no session log for {session_id} at {}", path.display()),
        ));
    }
    let events = read_events(&path)?;
    let head = events
        .iter()
        .find(|event| event.typ == EventType::SESSION_START.as_str())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("no session/start in {}", path.display()),
            )
        })?;
    let parent = head
        .payload
        .as_ref()
        .and_then(|payload| payload.get("parent"))
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if parent.is_empty() {
        return Ok(None);
    }
    Ok(Some(parent))
}

/// The ancestor chain of `child_id`: `[direct parent, grandparent, ...]`, ending
/// at the root. Does not include `child_id` itself.
///
/// Every hop reads the *previous* session's log for its `session/start`
/// `parent`. A missing log is [`ErrorKind::NotFound`] naming the id; a chain
/// that points back at itself (or runs past [`MAX_ANCESTRY`] hops) is
/// [`ErrorKind::Refused`] — a forged log must not turn into a hang. A parent
/// that is not shaped like a session id is [`ErrorKind::Invalid`] and is never
/// joined onto a path.
pub fn ancestry(data_dir: &Path, child_id: &str) -> Result<Vec<String>> {
    if !is_child_id(child_id) {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("not a session id: {child_id:?}"),
        ));
    }
    let mut chain: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(child_id.to_string());
    let mut current = child_id.to_string();
    loop {
        if chain.len() >= MAX_ANCESTRY {
            return Err(Error::new(
                ErrorKind::Refused,
                format!("the ancestry of {child_id} is deeper than {MAX_ANCESTRY} links"),
            ));
        }
        let Some(parent) = parent_of(data_dir, &current)? else {
            return Ok(chain);
        };
        if !is_child_id(&parent) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("the session/start of {current} names an unusable parent {parent:?}"),
            ));
        }
        if !seen.insert(parent.clone()) {
            return Err(Error::new(
                ErrorKind::Refused,
                format!("the ancestry of {child_id} loops back to {parent}"),
            ));
        }
        chain.push(parent.clone());
        current = parent;
    }
}

/// Render a chain from its root to the target's parent, `a/b/c`. The last hop of
/// a chain built by [`ancestry`] is the root session, which is rendered as
/// [`ROOT_AGENT_NAME`]; the hops in between are session ids, because that is
/// what the logs hold.
pub fn describe_ancestry(chain: &[String]) -> String {
    if chain.is_empty() {
        return ROOT_AGENT_NAME.to_string();
    }
    let mut parts: Vec<&str> = Vec::with_capacity(chain.len());
    for (index, id) in chain.iter().rev().enumerate() {
        // The oldest entry (last in the chain, first when reversed) is the root:
        // its head has no parent, so it is the session `lead` names.
        if index == 0 {
            parts.push(ROOT_AGENT_NAME);
        } else {
            parts.push(id.as_str());
        }
    }
    parts.join("/")
}

/// Decide whether `caller` may interrupt `target_child`.
///
/// The order is the order of the DSH rules: the operator always may; a hostile
/// target id is refused; a session may interrupt its parent or any deeper
/// ancestor; a session may not interrupt itself, one of its descendants, or an
/// unrelated session. Any failure to read the tree denies, never allows.
///
/// # Panics
///
/// Never: every internal failure is folded into [`Verdict::Deny`], because this
/// function is a security boundary and the safe direction is refusal.
pub fn check(data_dir: &Path, caller: &Caller, target_child: &str) -> Verdict {
    let Some(caller_id) = caller.session_id() else {
        return Verdict::Allow;
    };
    if !is_child_id(target_child) {
        return Verdict::Deny(format!("{target_child:?} is not a child session id"));
    }
    if caller_id == target_child {
        return Verdict::Deny(format!("cannot interrupt yourself ({target_child})"));
    }
    let chain = match ancestry(data_dir, target_child) {
        Ok(chain) => chain,
        Err(error) => {
            return Verdict::Deny(format!(
                "the ancestry of {target_child} is incomplete: {error}"
            ));
        }
    };
    if chain.iter().any(|id| id == caller_id) {
        // The direct parent and every grandparent land here: authority flows
        // down the tree, so any ancestor may stop any descendant.
        return Verdict::Allow;
    }
    if let Ok(caller_chain) = ancestry(data_dir, caller_id) {
        if caller_chain.iter().any(|id| id == target_child) {
            return Verdict::Deny(format!(
                "cannot interrupt an ancestor ({target_child} is above {caller_id})"
            ));
        }
    }
    Verdict::Deny(format!(
        "only an ancestor of {target_child} can interrupt it (this caller is {caller_id}); \
         the ancestry of {target_child} is {}",
        describe_ancestry(&chain)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The integration tests in `tests/authority.rs` build real logs; these
    // unit tests only pin the pure rendering and tagging helpers.
    #[test]
    fn describe_ancestry_renders_root_down() {
        let chain = vec!["agent-1".to_string(), "root-1".to_string()];
        assert_eq!(describe_ancestry(&chain), "lead/agent-1");
        assert_eq!(describe_ancestry(&[]), ROOT_AGENT_NAME);
    }

    #[test]
    fn a_caller_tag_names_the_session() {
        assert_eq!(Caller::User.tag(), "user");
        assert_eq!(Caller::Session("s-1".into()).tag(), "session:s-1");
    }

    #[test]
    fn a_verdict_reports_its_reason_only_when_refused() {
        assert!(Verdict::Allow.is_allow());
        assert!(Verdict::Allow.reason().is_none());
        let denied = Verdict::Deny("no".into());
        assert!(!denied.is_allow());
        assert_eq!(denied.reason(), Some("no"));
    }
}
