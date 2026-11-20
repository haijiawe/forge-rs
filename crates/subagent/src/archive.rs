//! Archiving: the one way to say "nothing new starts here", and the check that
//! makes it mean something.
//!
//! DSH installs a listener on `agent/pre-step` that refuses an archived ancestor.
//! That mechanism is not available here — there is no pre-step listener that can
//! veto a turn — so the check runs at the two places where a *new* piece of work
//! is admitted: starting a child and resuming one. Those are the doors; a running
//! turn is not re-checked, because a turn that is already going has its
//! permission (and its cancel token) and stopping it mid-flight is a different
//! verb.
//!
//! What "archived" means is deliberately the weakest useful thing: a record in a
//! session's own log. No flag on a process, no field in a config file, no row in
//! a database. A later process reads the log and reaches the same conclusion —
//! which is the property that makes it usable across a restart.

use std::path::Path;

use hearth_core::{Error, ErrorKind, EventType, Result};

use crate::authority;

/// The reason recorded when the operator did not give one.
pub const DEFAULT_REASON: &str = "archived by the operator";

/// Whether `session_id` has an archive record in its log.
///
/// A session with no log is *not* archived. That is not fail-open: an archived
/// session always has the record in its own log, so a missing log cannot be
/// hiding one. It matters for the common case of asking about a session that
/// never ran.
pub fn is_archived(data_dir: &Path, session_id: &str) -> Result<bool> {
    if !crate::child::is_child_id(session_id) {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!("not a session id: {session_id:?}"),
        ));
    }
    let path = crate::child::child_log_path(data_dir, session_id);
    if !path.is_file() {
        return Ok(false);
    }
    let events = crate::list::read_events(&path)?;
    Ok(events
        .iter()
        .any(|event| event.typ == EventType::SESSION_ARCHIVED.as_str()))
}

/// Refuse when `session_id` or any ancestor of it is archived.
///
/// The message names the session that is archived rather than the one that was
/// being admitted, because the operator can act on the first and not the second:
/// "a child of an archived session cannot start" leaves them looking at a session
/// they never archived.
pub fn admit(data_dir: &Path, session_id: &str) -> Result<()> {
    if is_archived(data_dir, session_id)? {
        return Err(refused(session_id, session_id));
    }
    // A missing log is not an error here. `admit` is a guard, not a validator:
    // its job is to find an archive record, and a session whose log was never
    // written cannot have one. Failing instead would turn "the caller passed a
    // parent id we never logged" into a refusal that reads like a policy
    // decision, and the code that actually opens that log reports the real
    // problem a moment later.
    let ancestors = match authority::ancestry(data_dir, session_id) {
        Ok(ancestors) => ancestors,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for ancestor in ancestors {
        if is_archived(data_dir, &ancestor)? {
            return Err(refused(session_id, &ancestor));
        }
    }
    Ok(())
}

fn refused(subject: &str, archived: &str) -> Error {
    let text = if subject == archived {
        format!("session {archived} is archived and cannot start new work")
    } else {
        format!(
            "session {subject} is below {archived}, which is archived; \
             unarchive it or start elsewhere"
        )
    };
    Error::new(ErrorKind::Refused, format!("subagent: {text}"))
}

/// The payload of a `session/archived` record.
pub fn payload(reason: &str) -> serde_json::Value {
    let reason = reason.trim();
    if reason.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::json!({ "reason": reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hearth_core::{new_id, Event};
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hearth-archive-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).expect("sessions dir");
        dir
    }

    /// Write one record the way the store would: dense `seq`, a fresh id, a
    /// timestamp. Handing `Event::new` straight to the file leaves `seq` at zero,
    /// and a log whose second record says `seq: 1` does not parse — which reads
    /// as "no such record" rather than "bad test".
    fn append(dir: &Path, session: &str, typ: EventType, payload: serde_json::Value) {
        let path = crate::child::child_log_path(dir, session);
        let existing = if path.is_file() {
            std::fs::read_to_string(&path).expect("existing log")
        } else {
            String::new()
        };
        let seq = existing
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count() as u64
            + 1;
        let mut event = Event::new(session, typ, Some(payload));
        event.id = new_id().expect("event id");
        event.seq = seq;
        event.ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as i64;
        let mut text = existing;
        text.push_str(&event.canonical().expect("canonical"));
        text.push('\n');
        std::fs::write(&path, text).expect("append");
    }

    fn start(dir: &Path, session: &str, parent: Option<&str>) {
        let mut payload = serde_json::json!({ "workspace": ".", "model": "m" });
        if let Some(parent) = parent {
            payload["parent"] = serde_json::json!(parent);
        }
        append(dir, session, EventType::SESSION_START, payload);
    }

    #[test]
    fn a_session_with_no_record_is_not_archived() {
        let dir = scratch("plain");
        start(&dir, "s-1", None);
        assert!(!is_archived(&dir, "s-1").expect("check"));
        assert!(admit(&dir, "s-1").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_log_is_not_archived_rather_than_an_error() {
        let dir = scratch("missing");
        assert!(!is_archived(&dir, "s-never-ran").expect("check"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_record_flips_the_answer() {
        let dir = scratch("flip");
        start(&dir, "s-1", None);
        append(
            &dir,
            "s-1",
            EventType::SESSION_ARCHIVED,
            payload(DEFAULT_REASON),
        );
        assert!(is_archived(&dir, "s-1").expect("check"));
        let err = admit(&dir, "s-1").expect_err("archived refuses");
        assert!(
            err.message().contains("s-1 is archived"),
            "{}",
            err.message()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_of_an_archived_session_cannot_start() {
        let dir = scratch("child");
        start(&dir, "root", None);
        start(&dir, "child", Some("root"));
        append(
            &dir,
            "root",
            EventType::SESSION_ARCHIVED,
            payload("done with this line of work"),
        );
        let err = admit(&dir, "child").expect_err("ancestor archived");
        assert!(
            err.message().contains("below root"),
            "the message must name the archived session: {}",
            err.message()
        );
        assert!(
            !err.message().contains("done with this line of work"),
            "the reason belongs to the record, not to every refusal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archived_grandparent_is_refused_too() {
        let dir = scratch("grand");
        start(&dir, "root", None);
        start(&dir, "mid", Some("root"));
        start(&dir, "leaf", Some("mid"));
        append(&dir, "root", EventType::SESSION_ARCHIVED, payload(""));
        assert!(admit(&dir, "leaf").is_err(), "two hops is still ancestry");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_reason_writes_no_member() {
        assert_eq!(payload("   "), serde_json::json!({}));
        assert_eq!(
            payload(" because "),
            serde_json::json!({ "reason": "because" })
        );
    }

    #[test]
    fn a_hostile_id_is_refused_before_any_path_is_built() {
        let dir = scratch("hostile");
        let err = is_archived(&dir, "../../etc/passwd").expect_err("refused");
        assert!(matches!(err.kind(), ErrorKind::Invalid));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
