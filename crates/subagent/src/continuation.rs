//! The words a parent and a child exchange when a delegated turn ends.
//!
//! DSH models continuation as messages on the parent's inbox: an
//! `agent-message` relayed from a named child, or a `subagent-settled` notice
//! that says the child finished and what it stopped on
//! (`subagent/src/continuation-messages.ts`). hearth runs a child
//! synchronously — its answer is the tool result, not a later message — but the
//! same text still has to be produced when a caller wants to relay a child's
//! progress into a parent it is no longer attached to (cold resume, a UI that
//! lists children). Keeping the wording here means it is written, tested and
//! quoted the same way in every such place.
//!
//! Two rules, both from DSH: a relayed message says who sent it, and a
//! settlement says why it stopped (because "finished" and "gave up" read the
//! same in a bare summary line).

use crate::descriptor::Mode;

/// The prefix on a message relayed from a named child, so the recipient knows
/// it is reading a person, not the operator.
pub fn agent_message(sender_name: &str, content: &str) -> String {
    format!("Agent {sender_name} sent a message: {content}")
}

/// Appended to a delegated prompt so the child knows its closing text is the
/// answer. Without it a child sometimes calls a tool to "deliver" the result and
/// then stops with an empty closing message.
pub const RESULT_GUIDANCE: &str =
    "Your final answer is delivered to the caller automatically when you stop. Do not repeat it as a tool call.";

pub fn with_result_guidance(prompt: &str) -> String {
    let prompt = prompt.trim_end();
    if prompt.is_empty() {
        RESULT_GUIDANCE.to_string()
    } else {
        format!("{prompt}\n\n{RESULT_GUIDANCE}")
    }
}

/// One line saying a child settled, in the form a list or a parent would show.
///
/// `stop_reason` is the harness spelling (`completed`, `max-tokens`, `aborted`,
/// `error`, `refusal`). A continuable child says it can be resumed; a one-shot
/// child does not, because promising it would be a lie.
pub fn settlement_summary(name: &str, stop_reason: &str, mode: Mode) -> String {
    let resume = match mode {
        Mode::Continuable => ", continuable",
        Mode::OneShot => "",
    };
    format!("{name} {stop_reason}{resume}")
}

/// A fuller settlement notice: the summary line, then the child's closing text
/// (or a note that it produced none), then a structured result when one exists.
pub fn settlement_notice(
    name: &str,
    stop_reason: &str,
    mode: Mode,
    closing_text: &str,
    structured: Option<&serde_json::Value>,
) -> String {
    let mut out = settlement_summary(name, stop_reason, mode);
    let closing = closing_text.trim();
    if closing.is_empty() {
        out.push_str("\n(no closing message)");
    } else {
        out.push('\n');
        out.push_str(closing);
    }
    if let Some(value) = structured {
        out.push_str("\nStructured result: ");
        out.push_str(&value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_relayed_message_names_its_sender() {
        assert_eq!(
            agent_message("agent-2", "the migration is half done"),
            "Agent agent-2 sent a message: the migration is half done"
        );
    }

    #[test]
    fn the_result_guidance_is_appended_once_and_not_twice() {
        let prompt = with_result_guidance("summarise the diff");
        assert!(prompt.starts_with("summarise the diff\n\n"));
        assert!(prompt.ends_with(RESULT_GUIDANCE));
        // An empty prompt is just the guidance, not a leading blank line.
        assert_eq!(with_result_guidance("   "), RESULT_GUIDANCE);
    }

    #[test]
    fn a_settlement_says_whether_it_can_be_resumed() {
        assert_eq!(
            settlement_summary("agent-1", "completed", Mode::Continuable),
            "agent-1 completed, continuable"
        );
        assert_eq!(
            settlement_summary("agent-1", "error", Mode::OneShot),
            "agent-1 error"
        );
    }

    #[test]
    fn a_notice_carries_the_closing_text_and_the_structured_result() {
        let notice = settlement_notice(
            "agent-1",
            "completed",
            Mode::Continuable,
            "  done  ",
            Some(&json!({"files": 3})),
        );
        assert_eq!(
            notice,
            "agent-1 completed, continuable\ndone\nStructured result: {\"files\":3}"
        );
    }

    #[test]
    fn a_silent_child_is_reported_as_silent_not_blank() {
        let notice = settlement_notice("agent-1", "aborted", Mode::OneShot, "   ", None);
        assert_eq!(notice, "agent-1 aborted\n(no closing message)");
    }
}
