//! `goal_end` — the model's only way to say "this goal is finished" or
//! "something is blocking me".
//!
//! The tool writes nothing. It returns the reason in its result `meta`, and the
//! turn loop that owns the log turns that into a `goal/ended` event. That split
//! is the same one the approval flow uses, for the same reason: a tool is a
//! child of the loop and may run in a process that does not own the session
//! file, while the log has exactly one writer. A tool that appended the event
//! itself would be a second writer, and two writers cannot produce one honest
//! ordering.

use serde::Deserialize;
use serde_json::json;

use crate::contract::{decode_args, Call, Result, ERR_BAD_ARGS};
use crate::truncate::{truncate, Limits};
use crate::Tool;

/// The `meta` key the turn loop reads. Named here so the writer of the tool and
/// the reader in the loop cannot drift apart silently.
pub const GOAL_META_KEY: &str = "goalEnd";

const COMPLETE: &str = "complete";
const BLOCKED: &str = "blocked";

pub struct GoalEndTool {
    limits: Limits,
}

#[derive(Deserialize)]
struct GoalEndArgs {
    reason: String,
    #[serde(default)]
    note: Option<String>,
}

impl GoalEndTool {
    pub fn new(limits: Limits) -> Self {
        Self { limits }
    }

    fn effective_limits(&self) -> Limits {
        if self.limits.max_bytes <= 0 && self.limits.max_lines <= 0 {
            Limits::default()
        } else {
            self.limits
        }
    }
}

impl Default for GoalEndTool {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Tool for GoalEndTool {
    fn spec(&self) -> forge_llm::ToolSpec {
        forge_llm::ToolSpec {
            name: "goal_end".into(),
            description: "Declare the current goal finished, or say what blocks it.\n\nWhen to use: a goal is active — the operator asked you to keep working until something is done, and the harness keeps opening new rounds for you. Call this as soon as the goal's condition is actually met, and call it with reason \"blocked\" when you cannot make progress for a reason the next round cannot fix (a missing credential, a decision only a human can make, a failing external service).\n\nHow to use it: \"complete\" means the work is done and checkable — say in the note what proves it. \"blocked\" means you stopped for a reason; say precisely what is blocking and what a human would have to do. Do not call it to escape difficulty: being unsure how to continue is what the next round is for.\n\nWhat happens next: the harness records the reason in the session log and stops opening rounds. Nothing here writes files; this call returns the reason to the loop.\n\nExample arguments:\n{\"reason\":\"complete\",\"note\":\"cargo test --workspace and check.ps1 are green\"}".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "enum": [COMPLETE, BLOCKED],
                        "description": "complete when the goal's condition is met; blocked when no further round can make progress."
                    },
                    "note": {
                        "type": "string",
                        "description": "One or two sentences: what proves it is done, or what is blocking and what a human would have to do."
                    }
                },
                "required": ["reason"]
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: GoalEndArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        let reason = args.reason.trim().to_lowercase();
        if reason != COMPLETE && reason != BLOCKED {
            return Result::error(
                ERR_BAD_ARGS,
                format!(
                    "goal_end: unknown reason {:?}; use {COMPLETE} or {BLOCKED}",
                    args.reason
                ),
            );
        }
        let note = args
            .note
            .map(|note| note.trim().to_string())
            .filter(|note| !note.is_empty());

        // The model is told to justify a stop, so a stop with no note is worth
        // naming in the output even though it is accepted: an unexplained
        // "blocked" is the single least useful line a log can hold.
        let complete = reason == COMPLETE;
        let mut output = match (complete, note.as_deref()) {
            (true, Some(note)) => format!("goal_end: complete — {note}"),
            (true, None) => "goal_end: complete".to_string(),
            (false, Some(note)) => format!("goal_end: blocked — {note}"),
            (false, None) => {
                "goal_end: blocked, but no note was given; the log will not explain this stop"
                    .to_string()
            }
        };
        output.push_str(
            "\nThe harness will record this and stop opening rounds when this turn ends.",
        );
        let (output, truncated) = truncate(&output, self.effective_limits());

        let mut result = Result::ok(output);
        result.truncated = truncated;
        result.meta.insert(GOAL_META_KEY.to_string(), json!(reason));
        if let Some(note) = note {
            result.meta.insert("note".to_string(), json!(note));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::truncate::DefaultLimits;

    fn run(args: serde_json::Value) -> Result {
        let tool = GoalEndTool::new(DefaultLimits);
        tool.call(Call::new("c1", "goal_end", args, "/ws"))
    }

    #[test]
    fn the_reason_travels_in_meta_for_the_loop_to_record() {
        let result = run(json!({"reason": "complete", "note": "all four tests pass"}));
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(result.meta.get(GOAL_META_KEY), Some(&json!("complete")));
        assert_eq!(result.meta.get("note"), Some(&json!("all four tests pass")));
        assert!(
            !result.output.contains("will not explain"),
            "a noted stop is explained: {}",
            result.output
        );
    }

    #[test]
    fn a_blocked_stop_without_a_note_is_accepted_and_named_as_unexplained() {
        let result = run(json!({"reason": "blocked"}));
        assert!(!result.is_error);
        assert_eq!(result.meta.get(GOAL_META_KEY), Some(&json!("blocked")));
        assert_eq!(result.meta.get("note"), None);
        assert!(
            result.output.contains("will not explain this stop"),
            "{}",
            result.output
        );
    }

    #[test]
    fn the_reason_is_normalised_but_an_unknown_one_is_refused() {
        let result = run(json!({"reason": "  Complete "}));
        assert!(!result.is_error, "{}", result.output);
        assert_eq!(result.meta.get(GOAL_META_KEY), Some(&json!("complete")));

        let result = run(json!({"reason": "done"}));
        assert!(result.is_error);
        assert_eq!(result.code, ERR_BAD_ARGS);
        assert!(
            result.output.contains("done"),
            "the refusal quotes what was sent: {}",
            result.output
        );
        assert_eq!(result.meta.get(GOAL_META_KEY), None, "no stop is recorded");
    }

    #[test]
    fn a_blank_note_is_the_same_as_no_note() {
        let result = run(json!({"reason": "blocked", "note": "   "}));
        assert_eq!(result.meta.get("note"), None);
        assert!(result.output.contains("will not explain this stop"));
    }

    #[test]
    fn a_missing_reason_is_a_bad_argument_not_a_default() {
        let result = run(json!({}));
        assert!(result.is_error);
        assert_eq!(result.code, ERR_BAD_ARGS);
        assert_eq!(result.meta.get(GOAL_META_KEY), None);
    }
}
