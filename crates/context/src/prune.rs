//! Pruning old tool outputs without unpairing a tool call from its result.
//!
//! A long session accumulates `tool/result` records whose output was useful once
//! and is dead weight the model does not need on every later turn. Compaction
//! hides a *range*; this prunes *text inside* a range, so the record that ties a
//! call to its result is still there — only the body shrinks.
//!
//! The pairing rule is the one from `history::closed_hidden_set`, stated as a
//! guard here: a result is eligible only when its call is present, and this
//! module never returns a removal. Truncating a result's text cannot create a
//! "result with no call" (the record stays) or a "call with no result" (the
//! result stays); the only failure it could cause is a result whose call was
//! itself pruned away, which is why orphan results are left untouched.
//!
//! This is a pure plan: it takes neutral values and returns the new text for the
//! results it would trim. It reads no log, opens no file and knows nothing about
//! a provider.

use std::collections::BTreeSet;

/// One tool result's output, addressed by the `seq` of the record that carries
/// it. `call_id` is the pairing key shared with the assistant record that made
/// the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub seq: u64,
    pub call_id: String,
    pub output: String,
}

/// One trimming the caller should apply: the record at `seq` now reads `output`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trim {
    pub seq: u64,
    pub output: String,
}

/// Plans which old tool outputs to shorten.
///
/// - `paired_call_ids` is the set of call ids that do have an assistant record;
///   a result whose `call_id` is absent is an orphan and is never pruned.
/// - `keep_recent` is how many of the newest eligible results stay whole (the
///   live edge — recent output is exactly what the model is reasoning about).
/// - `max_output_bytes` is the threshold: a result at or below it is left alone,
///   one above it is cut to a prefix of at most that many bytes plus a marker
///   that names how much was dropped.
///
/// Only results that are both old and over the threshold appear in the result;
/// the caller can use "the returned plan is empty" to mean "nothing to do".
pub fn prune_tool_outputs(
    outputs: &[ToolOutput],
    paired_call_ids: &BTreeSet<String>,
    keep_recent: usize,
    max_output_bytes: usize,
) -> Vec<Trim> {
    let mut eligible: Vec<&ToolOutput> = outputs
        .iter()
        .filter(|output| paired_call_ids.contains(&output.call_id))
        .collect();
    // Oldest first, so "keep the last N" is deterministic regardless of the
    // order the caller happened to hand the records over in.
    eligible.sort_by_key(|output| output.seq);

    let keep = keep_recent.min(eligible.len());
    let prune_count = eligible.len() - keep;
    let mut trims = Vec::new();
    for output in &eligible[..prune_count] {
        if output.output.len() <= max_output_bytes {
            continue;
        }
        let (prefix, omitted) = truncate_at_boundary(&output.output, max_output_bytes);
        trims.push(Trim {
            seq: output.seq,
            output: format!("{prefix}\n…[pruned {omitted} bytes]"),
        });
    }
    trims
}

/// Cuts `text` to at most `max` bytes on a UTF-8 boundary. Returns the kept
/// prefix and how many bytes were dropped.
///
/// Cutting mid-codepoint would make the log unreadable, so the boundary is
/// walked back even though it can keep fewer than `max` bytes.
fn truncate_at_boundary(text: &str, max: usize) -> (&str, usize) {
    if text.len() <= max {
        return (text, 0);
    }
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], text.len() - end)
}
