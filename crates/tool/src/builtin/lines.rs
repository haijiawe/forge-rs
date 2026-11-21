//! Which lines a write changed, as numbers.
//!
//! `fs_write` and `fs_edit` are the two places that hold both texts at the
//! moment one replaces the other: what was on disk a breath ago, and what is
//! being written now. Everything downstream — the log, the transcript, the
//! per-turn review list — only ever sees the second one, so if the first is not
//! compared here, the comparison cannot be made honestly afterwards.
//!
//! The answer is line ranges **in the new text**, 1-based and inclusive, in
//! ascending order: the lines a reader would point at when asked "what did this
//! write touch". Ranges are not hunks; they are what the editor can highlight
//! without a diff algorithm of its own.
//!
//! Three boundaries are deliberate:
//!
//! - Beyond [`MAX_LINES`] on either side the answer is `None`. A 200k-line
//!   regeneration is a real thing, and a reviewer looking at 200k highlighted
//!   lines has learned nothing; refusing beats inventing.
//! - A middle stretch too large for the quadratic comparison is reported as one
//!   range with `coarse: true`. The lines really did change (the text is not
//!   equal), only the fine structure is unavailable.
//! - `added`/`removed` count lines that have no counterpart, so a rewrite of one
//!   line reads as `+1 −1`, not as `+2 −0`.

/// Lines past this many are not compared; the caller gets `None`.
pub const MAX_LINES: usize = 20_000;

/// Ranges past this many are cut. A hundred edits in one write is already a
/// rewrite.
pub const MAX_RANGES: usize = 200;

/// Cell budget for the quadratic middle. Beyond it the comparison is coarse.
const DP_CELLS: usize = 4_000_000;

/// One inclusive line range in the new text, 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub from: usize,
    pub to: usize,
}

/// What a write changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// Lines in the new text with no counterpart in the old one.
    pub added: usize,
    /// Lines in the old text with no counterpart in the new one.
    pub removed: usize,
    /// Ascending, non-overlapping, inclusive ranges in the new text.
    pub ranges: Vec<Range>,
    /// The middle was too large to compare line by line; `ranges` is one range
    /// covering it, and `added`/`removed` are its extent.
    pub coarse: bool,
    /// The range list hit [`MAX_RANGES`]; the counts above are still exact.
    pub cut: bool,
}

impl Changed {
    /// The wire shape recorded in `tool/result` meta and read by the interface.
    ///
    /// Keys are camelCase because the reader is JavaScript and because the
    /// neighbouring meta keys (`totalLines`, `startLine`) are already spelled
    /// that way.
    pub fn to_json(&self) -> serde_json::Value {
        let ranges: Vec<serde_json::Value> = self
            .ranges
            .iter()
            .map(|range| serde_json::json!({"from": range.from, "to": range.to}))
            .collect();
        let mut value = serde_json::json!({
            "added": self.added,
            "removed": self.removed,
            "ranges": ranges,
        });
        if self.coarse {
            value["coarse"] = serde_json::json!(true);
        }
        if self.cut {
            value["cut"] = serde_json::json!(true);
        }
        value
    }
}

/// Compare two texts line by line.
///
/// `None` when either side is past [`MAX_LINES`], when the texts are equal
/// (there is nothing to report), or when the old text cannot be split into
/// lines without lying about it (it contains a `NUL`, so it is not text).
pub fn changed(before: &str, after: &str) -> Option<Changed> {
    if before == after {
        return None;
    }
    if before.contains('\0') || after.contains('\0') {
        return None;
    }
    let old = split_lines(before);
    let new = split_lines(after);
    if old.len() > MAX_LINES || new.len() > MAX_LINES {
        return None;
    }

    // Trim what is obviously shared: a one-line edit in a 5,000-line file must
    // cost one line of work, not twenty-five million cells.
    let mut head = 0;
    while head < old.len() && head < new.len() && old[head] == new[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < old.len() - head
        && tail < new.len() - head
        && old[old.len() - 1 - tail] == new[new.len() - 1 - tail]
    {
        tail += 1;
    }
    let old_mid = &old[head..old.len() - tail];
    let new_mid = &new[head..new.len() - tail];

    // The middle is all insertion or all deletion: no comparison needed.
    if old_mid.is_empty() && new_mid.is_empty() {
        // Same lines, different bytes: a final newline appeared or vanished.
        // An editor shows the same lines, so there is no edit to report.
        return None;
    }
    if old_mid.is_empty() || new_mid.is_empty() {
        let ranges = if new_mid.is_empty() {
            Vec::new()
        } else {
            vec![Range {
                from: head + 1,
                to: head + new_mid.len(),
            }]
        };
        return Some(Changed {
            added: new_mid.len(),
            removed: old_mid.len(),
            ranges,
            coarse: false,
            cut: false,
        });
    }

    if old_mid.len().saturating_mul(new_mid.len()) > DP_CELLS {
        // Too far apart to align. Both sides changed as far as anyone can say,
        // so the whole middle is the range and the counts are its extent.
        return Some(Changed {
            added: new_mid.len(),
            removed: old_mid.len(),
            ranges: vec![Range {
                from: head + 1,
                to: head + new_mid.len(),
            }],
            coarse: true,
            cut: false,
        });
    }

    let keep = matched_lines(old_mid, new_mid);
    let mut ranges = Vec::new();
    let mut added = 0;
    let mut start = 0usize;
    for (index, kept) in keep.iter().enumerate() {
        if *kept {
            if start < index {
                ranges.push(Range {
                    from: head + start + 1,
                    to: head + index,
                });
            }
            start = index + 1;
        } else {
            added += 1;
        }
    }
    if start < keep.len() {
        ranges.push(Range {
            from: head + start + 1,
            to: head + keep.len(),
        });
    }

    // The texts differ but no line does: a file that gained or lost its final
    // newline. Saying "nothing changed" is right — an editor shows the same
    // lines — and reporting +0 −0 with an empty range list would only invite a
    // caller to draw a marker nobody can place.
    if added == 0 && ranges.is_empty() {
        return None;
    }

    let mut cut = false;
    if ranges.len() > MAX_RANGES {
        ranges.truncate(MAX_RANGES);
        cut = true;
    }

    Some(Changed {
        added,
        removed: old_mid
            .len()
            .saturating_sub(keep.iter().filter(|kept| **kept).count()),
        ranges,
        coarse: false,
        cut,
    })
}

/// Split into lines the way an editor numbers them: `"a\nb\n"` is two lines,
/// and `""` is none.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// For each line of `new`, whether it was matched to a line of `old`.
///
/// Longest common subsequence, the textbook one: the middle is bounded by
/// [`DP_CELLS`], so an O(n·m) table is affordable and, unlike a heuristic, it
/// cannot claim a line is new when it merely moved.
fn matched_lines(old: &[&str], new: &[&str]) -> Vec<bool> {
    let rows = old.len() + 1;
    let cols = new.len() + 1;
    let mut table = vec![0u32; rows * cols];
    for i in 1..rows {
        for j in 1..cols {
            table[i * cols + j] = if old[i - 1] == new[j - 1] {
                table[(i - 1) * cols + (j - 1)] + 1
            } else {
                table[(i - 1) * cols + j].max(table[i * cols + (j - 1)])
            };
        }
    }

    let mut keep = vec![false; new.len()];
    let (mut i, mut j) = (old.len(), new.len());
    while i > 0 && j > 0 {
        if old[i - 1] == new[j - 1] {
            keep[j - 1] = true;
            i -= 1;
            j -= 1;
        } else if table[(i - 1) * cols + j] >= table[i * cols + (j - 1)] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(before: &str, after: &str) -> Vec<(usize, usize)> {
        changed(before, after)
            .map(|change| change.ranges.iter().map(|r| (r.from, r.to)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn identical_texts_report_nothing() {
        assert_eq!(changed("a\nb\n", "a\nb\n"), None);
    }

    #[test]
    fn one_changed_line_is_one_one_line_range() {
        let change = changed("a\nb\nc\n", "a\nB\nc\n").expect("changed");
        assert_eq!(change.added, 1);
        assert_eq!(change.removed, 1);
        assert_eq!(
            change.ranges,
            vec![Range { from: 2, to: 2 }],
            "the second line, not the first"
        );
        assert!(!change.coarse);
        assert!(!change.cut);
    }

    #[test]
    fn appended_lines_are_a_range_at_the_end() {
        let change = changed("a\nb\n", "a\nb\nfresh1\nfresh2\n").expect("changed");
        assert_eq!((change.added, change.removed), (2, 0));
        assert_eq!(change.ranges, vec![Range { from: 3, to: 4 }]);
    }

    #[test]
    fn deleted_lines_report_no_range_and_the_removed_count() {
        // Removing from the end: nothing in the new text is new.
        let change = changed("a\nb\nc\n", "a\nb\n").expect("changed");
        assert_eq!((change.added, change.removed), (0, 1));
        assert!(change.ranges.is_empty(), "no line in the new text changed");
    }

    #[test]
    fn a_new_file_is_all_new() {
        let change = changed("", "one\ntwo\n").expect("changed");
        assert_eq!((change.added, change.removed), (2, 0));
        assert_eq!(change.ranges, vec![Range { from: 1, to: 2 }]);
    }

    #[test]
    fn two_separate_edits_are_two_ranges() {
        let before = "1\n2\n3\n4\n5\n6\n";
        let after = "1\nTWO\n3\n4\nFIVE\n6\n";
        assert_eq!(ranges(before, after), vec![(2, 2), (5, 5)]);
    }

    #[test]
    fn contiguous_insertions_merge_into_one_range() {
        let before = "a\nd\n";
        let after = "a\nb\nc\nd\n";
        assert_eq!(ranges(before, after), vec![(2, 3)]);
    }

    #[test]
    fn a_line_that_only_moved_is_not_an_edit() {
        // LCS: the matching line is kept, so the two new lines are the change.
        let before = "keep\nold\n";
        let after = "keep\ninserted\n";
        let change = changed(before, after).expect("changed");
        assert_eq!((change.added, change.removed), (1, 1));
    }

    #[test]
    fn a_trailing_newline_is_not_an_extra_line() {
        assert_eq!(split_lines("a\nb\n").len(), 2);
        assert_eq!(split_lines("a\nb").len(), 2);
        assert_eq!(split_lines("").len(), 0);
        // And adding it is not an edit of any line.
        assert_eq!(changed("a\nb\n", "a\nb"), None);
    }

    #[test]
    fn a_text_past_the_line_cap_gets_no_answer() {
        let big = "x\n".repeat(MAX_LINES + 1);
        assert_eq!(changed(&big, &format!("{big}y\n")), None);
    }

    #[test]
    fn a_middle_too_large_to_align_is_reported_coarse() {
        // Unrelated bodies, both large enough that the trimmed middle exceeds
        // the cell budget: old lines 2.., new lines 2.., nothing matched.
        let old_body: String = (0..2_100).map(|i| format!("old{i}\n")).collect();
        let new_body: String = (0..2_100).map(|i| format!("new{i}\n")).collect();
        let before = format!("head\n{old_body}tail\n");
        let after = format!("head\n{new_body}tail\n");
        let change = changed(&before, &after).expect("changed");
        assert!(change.coarse, "2,100 × 2,100 cells is over the budget");
        assert!(!change.ranges.is_empty());
        assert_eq!(change.ranges[0].from, 2);
    }

    #[test]
    fn the_json_shape_is_the_one_the_interface_reads() {
        let change = changed("a\nb\nc\n", "a\nB\nc\n").expect("changed");
        let value = change.to_json();
        assert_eq!(value["added"], 1);
        assert_eq!(value["removed"], 1);
        assert_eq!(value["ranges"][0]["from"], 2);
        assert_eq!(value["ranges"][0]["to"], 2);
        assert!(
            value.get("coarse").is_none(),
            "coarse is only present when true"
        );
    }

    #[test]
    fn binary_text_is_not_compared() {
        assert_eq!(changed("a\0b", "a\0c"), None);
    }
}
