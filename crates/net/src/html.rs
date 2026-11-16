//! HTML to text, without a parser dependency.
//!
//! The tool that fetches a page hands the model text, not markup: a 300 KB
//! article is 30 KB of prose and 270 KB of attributes, and the model pays for
//! both. This is not an HTML parser and does not pretend to be one — it removes
//! what cannot be prose (`script`, `style`, comments), turns block boundaries
//! into newlines, drops the remaining tags, and decodes the entities that
//! actually appear in text.

/// Elements whose entire content is not prose.
const DROPPED_ELEMENTS: [&str; 6] = ["script", "style", "noscript", "template", "svg", "iframe"];

/// Tags that start a new line when they open or close.
const BLOCK_TAGS: [&str; 22] = [
    "p",
    "div",
    "br",
    "li",
    "tr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "section",
    "article",
    "header",
    "footer",
    "blockquote",
    "pre",
    "table",
    "ul",
    "ol",
    "nav",
    "form",
];

/// Convert a document to readable text.
pub fn to_text(html: &str) -> String {
    let without_dropped = drop_elements(html);
    let mut text = String::with_capacity(without_dropped.len());
    let mut chars = without_dropped.chars().peekable();
    let mut in_tag = false;
    let mut tag = String::new();

    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag_name(&tag);
                if BLOCK_TAGS.contains(&name.as_str()) {
                    text.push('\n');
                }
                if name == "td" || name == "th" {
                    text.push(' ');
                }
                // A `<a href>` is prose; the URL is not, unless the caller asks.
                if !tag.starts_with('/') && name == "li" {
                    text.push_str("- ");
                }
            }
            _ if in_tag => tag.push(ch),
            '&' => {
                // The decoder returns the literal `&` itself when it could not
                // read an entity, so there is nothing extra to push here.
                let (decoded, _consumed) = decode_entity(&mut chars);
                text.push_str(&decoded);
            }
            _ => text.push(ch),
        }
    }

    collapse(&text)
}

/// Remove the content of elements that never hold prose.
fn drop_elements(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut index = 0usize;
    while index < html.len() {
        // Find the next opening tag of a dropped element.
        let next = DROPPED_ELEMENTS
            .iter()
            .filter_map(|name| {
                let needle = format!("<{name}");
                lower[index..]
                    .find(&needle)
                    .map(|offset| (index + offset, *name))
            })
            .min_by_key(|(position, _)| *position);
        let Some((start, name)) = next else {
            out.push_str(&html[index..]);
            break;
        };
        out.push_str(&html[index..start]);
        let close = format!("</{name}");
        match lower[start..].find(&close) {
            Some(offset) => {
                let after = start + offset;
                match lower[after..].find('>') {
                    Some(end) => index = after + end + 1,
                    None => break,
                }
            }
            None => break,
        }
    }
    out
}

/// The local name of a tag, lowercased: `a href="x"` → `a`, `/p` → `p`.
fn tag_name(tag: &str) -> String {
    let trimmed = tag.trim_start_matches('/').trim_start();
    trimmed
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Decode one entity. Returns the replacement and how many input chars were
/// consumed (0 when the `&` was literal).
fn decode_entity(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> (String, usize) {
    let mut name = String::new();
    let mut consumed: usize = 0;
    // Entities are short; stop early rather than scanning the document.
    while consumed < 10 {
        let Some(&ch) = chars.peek() else { break };
        if ch == ';' {
            chars.next();
            consumed += 1;
            break;
        }
        if !ch.is_ascii_alphanumeric() && ch != '#' {
            break;
        }
        name.push(ch);
        chars.next();
        consumed += 1;
    }
    if consumed == 0 || !name.ends_with(|c: char| c.is_ascii_alphanumeric()) && !name.is_empty() {
        return ("&".to_string() + &name, consumed.saturating_sub(1));
    }
    let decoded = match name.as_str() {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "nbsp" => Some(" "),
        "mdash" => Some("—"),
        "ndash" => Some("–"),
        "hellip" => Some("…"),
        "copy" => Some("©"),
        "reg" => Some("®"),
        "trade" => Some("™"),
        "lsquo" => Some("'"),
        "rsquo" => Some("'"),
        "ldquo" => Some("\""),
        "rdquo" => Some("\""),
        "middot" => Some("·"),
        "laquo" => Some("«"),
        "raquo" => Some("»"),
        "deg" => Some("°"),
        "euro" => Some("€"),
        "pound" => Some("£"),
        "yen" => Some("¥"),
        "times" => Some("×"),
        "larr" => Some("←"),
        "rarr" => Some("→"),
        _ => numeric_entity(&name),
    };
    match decoded {
        Some(text) => (text.to_string(), consumed),
        None => ("&".to_string() + &name, consumed),
    }
}

/// `#233` / `#xE9` style entities.
fn numeric_entity(name: &str) -> Option<&'static str> {
    let digits = name.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    // Only the handful that appear in prose; everything else is dropped rather
    // than guessed, because a wrong character changes meaning.
    Some(match code {
        0x00A0 => " ",
        0x2013 => "–",
        0x2014 => "—",
        0x2018 | 0x2019 => "'",
        0x201C | 0x201D => "\"",
        0x2026 => "…",
        _ => return None,
    })
}

/// Collapse runs of blank space: spaces and tabs become one space, three or more
/// newlines become two, and every line is trimmed at the end.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    let mut newlines = 0usize;
    for ch in text.chars() {
        match ch {
            '\r' => {}
            '\n' => {
                space = false;
                newlines += 1;
                if newlines <= 2 {
                    out.push('\n');
                }
            }
            ' ' | '\t' | '\u{000C}' => {
                if newlines > 0 {
                    continue;
                }
                if !space && !out.ends_with('\n') && !out.is_empty() {
                    out.push(' ');
                    space = true;
                }
            }
            other => {
                space = false;
                newlines = 0;
                out.push(other);
            }
        }
    }
    // Trim each line's trailing spaces.
    out.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_and_keeps_prose() {
        let html = "<html><head><title>T</title><style>p{color:red}</style></head><body><h1>Hello</h1><p>World &amp; friends.</p></body></html>";
        let text = to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World & friends."));
        assert!(!text.contains("color:red"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn drops_script_bodies_that_contain_markup() {
        let html =
            "<p>before</p><script>if (a < b) { document.write('<p>no</p>') }</script><p>after</p>";
        let text = to_text(html);
        assert_eq!(text, "before\n\nafter");
    }

    #[test]
    fn turns_list_items_into_bullets_and_cells_into_spaces() {
        let html = "<ul><li>one</li><li>two</li></ul><table><tr><td>a</td><td>b</td></tr></table>";
        let text = to_text(html);
        assert!(text.contains("- one"));
        assert!(text.contains("- two"));
        assert!(text.contains("a b"));
    }

    #[test]
    fn decodes_the_entities_that_appear_in_prose() {
        assert_eq!(
            to_text("<p>a&nbsp;b &lt;c&gt; &#8212; d&hellip;</p>"),
            "a b <c> — d…"
        );
    }

    #[test]
    fn a_literal_ampersand_survives() {
        assert_eq!(to_text("<p>AT&T and R&D</p>"), "AT&T and R&D");
        assert_eq!(to_text("<p>100% & 50%</p>"), "100% & 50%");
    }

    #[test]
    fn collapses_blank_runs() {
        let text = to_text("<div>a</div>\n\n\n\n<div>b</div>");
        assert_eq!(text, "a\n\nb");
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(to_text(""), "");
        assert_eq!(to_text("<p></p>"), "");
    }
}
