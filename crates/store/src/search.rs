//! Full-text search over boundary messages only.
//!
//! Query words are always quoted and made prefix terms. That is deliberate:
//! user input must never be interpreted as FTS5 syntax (`" OR 1=1 --` is a
//! search string, not an expression).

use hearth_core::Result;
use rusqlite::params;

use crate::store::{store_err, Hit, Query, Store};

const MAX_SNIPPET_BYTES: usize = 200;
const SNIPPET_LEAD_BYTES: usize = 60;
const MAX_QUERY_TOKENS: usize = 16;
const MAX_QUERY_TOKEN_BYTES: usize = 64;
const ELLIPSIS_LEN: usize = "…".len();

/// Keep only word characters. Go used unicode letter/digit/mark; Rust's
/// `is_alphanumeric` covers letters and digits, and combining marks are kept
/// explicitly so CJK text and accents survive tokenization.
fn is_word_char(ch: char) -> bool {
    let code = ch as u32;
    ch.is_alphanumeric() || (0x0300..=0x036f).contains(&code)
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if is_word_char(ch) {
            current.push(ch);
        } else if !current.is_empty() {
            if current.len() > MAX_QUERY_TOKEN_BYTES {
                current.truncate(clip_len(&current, MAX_QUERY_TOKEN_BYTES));
            }
            if tokens.len() < MAX_QUERY_TOKENS {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if !current.is_empty() && tokens.len() < MAX_QUERY_TOKENS {
        if current.len() > MAX_QUERY_TOKEN_BYTES {
            current.truncate(clip_len(&current, MAX_QUERY_TOKEN_BYTES));
        }
        tokens.push(current);
    }
    tokens
}

/// Largest index <= `max` that is a UTF-8 character boundary.
fn clip_len(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn match_expression(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| {
            let escaped = token.replace('"', "\"\"");
            format!("\"{escaped}\"*")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Case-insensitive rune-folded index of `needle` in `haystack`, byte based.
/// Go used `unicode.ToLower` rune by rune; `to_lowercase().next()` is the
/// closest standard-library equivalent without adding a Unicode dependency.
fn index_fold(haystack: &str, needle: &str) -> Option<usize> {
    let folded_haystack: Vec<char> = haystack
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let folded_needle: Vec<char> = needle
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    if folded_needle.is_empty() || folded_needle.len() > folded_haystack.len() {
        return None;
    }
    let mut offsets = Vec::with_capacity(folded_haystack.len() + 1);
    let mut byte = 0;
    for ch in haystack.chars() {
        offsets.push(byte);
        byte += ch.len_utf8();
    }
    offsets.push(byte);
    (0..=folded_haystack.len() - folded_needle.len()).find_map(|start| {
        if folded_haystack[start..start + folded_needle.len()] == folded_needle[..] {
            Some(offsets[start])
        } else {
            None
        }
    })
}

fn make_snippet(text: &str, tokens: &[String]) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= MAX_SNIPPET_BYTES {
        return trimmed.to_owned();
    }
    let earliest = tokens
        .iter()
        .filter_map(|token| index_fold(trimmed, token))
        .min()
        .unwrap_or(0);
    let start = clip_len(trimmed, earliest.saturating_sub(SNIPPET_LEAD_BYTES));
    // Two ellipses must fit inside the 200-byte budget.
    let budget = MAX_SNIPPET_BYTES - 2 * ELLIPSIS_LEN;
    let wanted_end = start.saturating_add(budget).min(trimmed.len());
    let end = clip_len(trimmed, wanted_end);
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(&trimmed[start..end]);
    if end < trimmed.len() {
        snippet.push('…');
    }
    snippet
}

impl Store {
    /// Search indexed boundary messages. An empty (or punctuation-only) query
    /// returns an empty vector without touching FTS5.
    pub fn search(&self, text: &str, query: Query) -> Result<Vec<Hit>> {
        let _gate = self.read_gate()?;
        self.ensure_open()?;
        let tokens = tokenize(text);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let limit = if query.limit <= 0 {
            50
        } else {
            query.limit.min(200)
        };
        let expression = match_expression(&tokens);
        let guard = self.read_conn()?;
        let conn = guard.as_ref().ok_or_else(crate::store::err_closed)?;
        let mut statement = conn
            .prepare(
                "SELECT m.session_id, m.seq, m.turn, m.type, m.text
                 FROM messages_fts
                 JOIN messages m ON m.id = messages_fts.rowid
                 WHERE messages_fts MATCH ?1
                 ORDER BY bm25(messages_fts), m.session_id ASC, m.seq ASC
                 LIMIT ?2",
            )
            .map_err(|e| store_err("search prepare", e))?;
        let rows = statement
            .query_map(params![expression, limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| store_err("search", e))?;
        let mut hits = Vec::new();
        for row in rows {
            let (session_id, seq, turn, event_type, message) =
                row.map_err(|e| store_err("search row", e))?;
            hits.push(Hit {
                session_id,
                seq: u64::try_from(seq).unwrap_or(0),
                turn: u32::try_from(turn).unwrap_or(0),
                event_type,
                snippet: make_snippet(&message, &tokens),
            });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_drops_fts_syntax() {
        assert!(tokenize("   ").is_empty());
        assert_eq!(tokenize("forg*"), vec!["forg"]);
        assert_eq!(
            tokenize("forgotten\" OR 1=1 --"),
            vec!["forgotten", "OR", "1", "1"]
        );
        assert_eq!(tokenize("a\"b"), vec!["a", "b"]);
        assert_eq!(match_expression(&["a\"b".to_owned()]), "\"a\"\"b\"*");
    }

    #[test]
    fn snippet_stays_within_budget_on_a_rune_boundary() {
        let text = format!("{} needleword {}", "界".repeat(100), "x".repeat(200));
        let snippet = make_snippet(&text, &["needleword".to_owned()]);
        assert!(snippet.len() <= MAX_SNIPPET_BYTES);
        assert!(snippet.contains("needleword"));
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
    }

    #[test]
    fn short_text_is_returned_trimmed() {
        assert_eq!(make_snippet("  hello  ", &[]), "hello");
    }
}
