//! The goal race fence: re-check a reservation against the log at a checkpoint.

use std::path::{Path, PathBuf};

use forge_agent::{Hook, HookCall, HookOutcome};
use forge_core::{Error, ErrorKind};
use forge_goal::{fence, FenceOutcome, Reservation};

/// Build a `pre-step` hook that fences a continuation round.
///
/// The hook runs before every model step of the round. It re-reads the session
/// log (the single truth) and compares it to the reservation the loop opened the
/// round with. A mismatch returns an error, which the agent loop records as
/// `hook_failed` and aborts the step — so a round that was superseded, or whose
/// goal ended or was cancelled, stops before the next model call rather than
/// after it.
pub fn hook(log: &Path, reservation: Reservation) -> Hook {
    let log: PathBuf = log.to_path_buf();
    Hook::new("goal-fence", move |call: &HookCall<'_>| {
        if matches!(call, HookCall::PreStep { .. }) {
            let events = crate::goalcmd::read_events(&log).map_err(|e| {
                Error::new(
                    ErrorKind::Io,
                    format!("goal fence: cannot read the log: {}", e.message()),
                )
            })?;
            match fence(&events, &reservation) {
                FenceOutcome::Valid => {}
                FenceOutcome::Ended { status } => {
                    return Err(Error::new(
                        ErrorKind::Conflict,
                        format!(
                            "goal {} is {}; stopping this round",
                            reservation.goal_id,
                            status.as_str()
                        ),
                    ));
                }
                FenceOutcome::Stale { revision, rounds } => {
                    return Err(Error::new(
                            ErrorKind::Conflict,
                            format!(
                                "goal {} was advanced by another entry (revision {revision}, round {}); this round is stale",
                                reservation.goal_id,
                                rounds + 1
                            ),
                        ));
                }
            }
        }
        Ok(HookOutcome::Unchanged)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_goal::{goal_created, goal_ended, Goal, StopReason};

    fn log_with(label: &str, events: &[forge_core::Event]) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("forge-goalfence-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("s.jsonl");
        let store = forge_session::Store::open(&path, forge_session::Options::default())
            .expect("open store");
        for event in events {
            store.append(event).expect("append");
        }
        path
    }

    fn reservation(goal: &Goal) -> Reservation {
        Reservation {
            goal_id: goal.id.clone(),
            revision: goal.revision,
            round: goal.rounds,
        }
    }

    #[test]
    fn a_current_round_passes_the_fence() {
        let goal = Goal::new("g1", "keep going", 3);
        let path = log_with("valid", &[goal_created("s", &goal)]);
        let mut hooks = forge_agent::Hooks::none();
        hooks.push(hook(&path, reservation(&goal)));
        let (messages, changed) = hooks
            .run_pre_step(1, 1, Vec::new())
            .expect("valid round passes");
        assert!(messages.is_empty());
        assert!(changed.is_empty());
    }

    #[test]
    fn an_ended_goal_stops_the_round_at_the_next_step() {
        let goal = Goal::new("g1", "keep going", 3);
        let created = goal_created("s", &goal);
        let ended = goal_ended("s", &goal, StopReason::Cancelled, None);
        let path = log_with("ended", &[created, ended]);
        let mut hooks = forge_agent::Hooks::none();
        hooks.push(hook(&path, reservation(&goal)));
        let err = hooks
            .run_pre_step(1, 1, Vec::new())
            .expect_err("ended goal must fence");
        // `Hooks` re-wraps a hook error as `hook_failed` (ErrorKind::Invalid) with the
        // hook's name; the goal verdict is still in the message.
        assert_eq!(err.kind(), forge_core::ErrorKind::Invalid);
        assert!(err.message().contains("goal-fence"));
        assert!(err.message().contains("cancelled"));
    }
}
