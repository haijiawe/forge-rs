//! A **closed subset** of YAML frontmatter, parsed from the raw bytes.
//!
//! Why not reuse `hearth-skill`'s parser: that parser is `pub(crate)`, scans
//! `root/*/SKILL.md` exactly one level deep, and *drops* what it cannot read —
//! it warns and keeps the skill. Neither the privacy nor the policy fits here.
//! A roster file is different in kind: forgetting a `tools:` line turns a
//! read-only reviewer into a persona that can run a shell, and forgetting a
//! `members:` entry turns a team into a team that silently lost a member. So
//! this reader **refuses** anything it does not understand instead of warning
//! and continuing; that is the one behavioural choice a second parser buys.
//!
//! The subset, in full:
//!
//! * a first line of `---`, a closing line of `---`, and nothing else;
//! * `key: scalar` at column 1, with `#` comments and blank lines allowed;
//! * `key: [a, b]` flow sequences of scalars;
//! * `key:` followed by a `- item` block sequence, either of scalars or of
//!   single-level `- key: value` maps (enough for `members:`);
//! * single- or double-quoted scalars, unquoted by removing one quote layer.
//!
//! Not supported, and refused with a message rather than guessed at: block
//! scalars (`|`, `>`), nested maps, nested sequences, anchors, tags, flow maps,
//! multi-document files. A body belongs *below* the fence, so there is nothing
//! a block scalar would buy.

use crate::error::{Error, ErrorKind, Result};

/// One parsed frontmatter value. Kept in file order, never sorted: a team's
/// member order is part of what the file says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// A bare or quoted scalar.
    Scalar(String),
    /// A sequence; items are scalars or single-level maps.
    List(Vec<Value>),
    /// A single-level map, in file order.
    Map(Vec<(String, Value)>),
}

impl Value {
    /// The scalar text, or `None` for a list or map.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::Scalar(text) => Some(text),
            _ => None,
        }
    }

    /// The sequence items, or `None` for a scalar or map.
    pub(crate) fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    /// The map entries, or `None` for a scalar or list.
    pub(crate) fn as_map(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// A short shape name, for "expected a list, found a scalar" messages.
    pub(crate) fn shape(&self) -> &'static str {
        match self {
            Value::Scalar(_) => "a scalar",
            Value::List(_) => "a list",
            Value::Map(_) => "a map",
        }
    }
}

/// One file's frontmatter plus its body, with the body sliced from the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frontmatter {
    /// Top-level keys in file order.
    pub fields: Vec<(String, Value)>,
    /// Everything after the closing `---` line and its line terminator, kept
    /// byte for byte: no newline translation, no trimming, no re-encoding.
    ///
    /// This is the prompt. A loader that normalised `\r\n` to `\n` would make
    /// the prompt differ from the file a reviewer approved, and no test that
    /// compares prompts can then say which one the model saw.
    pub body: String,
}

impl Frontmatter {
    /// The value of one top-level key.
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }
}

/// One source line, with the offset needed to slice the body out verbatim.
struct Line<'a> {
    /// 1-based line number, for messages.
    no: usize,
    /// Byte offset just past this line's terminator (the start of the next).
    end: usize,
    /// The line without its `\n` or `\r\n` terminator.
    content: &'a str,
}

/// Parse one definition file.
///
/// `source` is only a label for messages ("crates/experts/x.md"); it is never
/// opened. The whole file is refused on any surprise; see the module doc for
/// why that differs from `hearth-skill`.
pub(crate) fn parse(data: &[u8], source: &str) -> Result<Frontmatter> {
    // A UTF-8 BOM is an encoding artefact, not content: stripping it keeps the
    // fence on line 1. It is stripped before the body is sliced, so the body is
    // still byte-for-byte what follows the closing fence.
    let bytes = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    let text = std::str::from_utf8(bytes).map_err(|err| {
        Error::new(
            ErrorKind::Unreadable,
            format!(
                "{source}: not valid UTF-8 at byte {}; a roster file must be UTF-8 Markdown",
                err.valid_up_to()
            ),
        )
    })?;

    let lines = split_lines(text);
    let Some(first) = lines.first() else {
        return Err(Error::new(
            ErrorKind::MissingFrontmatter,
            format!(
                "{source}: file is empty; an expert needs a --- frontmatter block with name, slug and description"
            ),
        ));
    };
    if first.content.trim() != "---" {
        return Err(Error::new(
            ErrorKind::MissingFrontmatter,
            format!(
                "{source}: line 1 must be --- opening a frontmatter block; found {:?}. An expert without frontmatter has no slug to summon",
                first.content
            ),
        ));
    }

    let close = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.content.trim() == "---")
        .map(|(index, _)| index)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::BadFrontmatter,
                format!("{source}: frontmatter is not closed by a second --- line; add --- after the last field"),
            )
        })?;

    let fields = parse_fields(&lines[1..close], source)?;
    // The body starts after the closing fence and its terminator. Nothing else
    // is touched: `lines[close].end` is exact, so the bytes after it are the
    // bytes the author typed.
    let body = text[lines[close].end..].to_string();
    Ok(Frontmatter { fields, body })
}

/// Split into lines while remembering where each one ended.
fn split_lines(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut offset = 0usize;
    for (index, chunk) in text.split_inclusive('\n').enumerate() {
        offset += chunk.len();
        let content = chunk.strip_suffix('\n').unwrap_or(chunk);
        let content = content.strip_suffix('\r').unwrap_or(content);
        lines.push(Line {
            no: index + 1,
            end: offset,
            content,
        });
    }
    lines
}

/// Parse the lines strictly between the two fences.
fn parse_fields(lines: &[Line<'_>], source: &str) -> Result<Vec<(String, Value)>> {
    let mut fields: Vec<(String, Value)> = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        let line = &lines[index];
        if is_skippable(line.content) {
            index += 1;
            continue;
        }
        let indent = indent_of(line, source)?;
        if indent != 0 {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: unexpected indented line {:?} outside a list; top-level fields start at column 1",
                    line.no,
                    line.content.trim()
                ),
            ));
        }

        let text = line.content.trim();
        let Some((key_raw, value_raw)) = text.split_once(':') else {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: expected \"key: value\", got {:?}; a frontmatter line is a field name, a colon and a value",
                    line.no, text
                ),
            ));
        };
        let key = key_raw.trim();
        if !valid_key(key) {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: {} is not a usable field name; use letters, digits, '-' and '_', starting with a letter",
                    line.no,
                    crate::util::quote(key)
                ),
            ));
        }
        if fields.iter().any(|(name, _)| name == key) {
            // Two values for one key is the ambiguity this crate exists to
            // refuse: "the last one wins" is a rule nobody remembers.
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: field {:?} appears more than once; keep one value",
                    line.no, key
                ),
            ));
        }

        let value_text = value_raw.trim();
        let (value, next) = if value_text.is_empty() {
            parse_block(lines, index + 1, source, key, line.no)?
        } else if value_text.starts_with('[') {
            (
                parse_flow_list(value_text, source, line.no, key)?,
                index + 1,
            )
        } else if value_text.starts_with('|') || value_text.starts_with('>') {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: field {:?} uses a block scalar, which is not supported; put free-form text below the closing --- as the body",
                    line.no, key
                ),
            ));
        } else {
            (Value::Scalar(unquote(value_text)), index + 1)
        };

        fields.push((key.to_string(), value));
        index = next;
    }

    Ok(fields)
}

/// A blank line or a whole-line comment.
fn is_skippable(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// Leading-space count, refusing a tab because a tab is not indentation here.
fn indent_of(line: &Line<'_>, source: &str) -> Result<usize> {
    let trimmed = line.content.trim_start_matches(' ');
    if trimmed.starts_with('\t') {
        return Err(Error::new(
            ErrorKind::BadFrontmatter,
            format!(
                "{source}: line {}: indented with a tab; this reader counts spaces, so use spaces",
                line.no
            ),
        ));
    }
    Ok(line.content.len() - trimmed.len())
}

/// Parse `key:` followed by a `- item` block sequence.
///
/// Returns the value and the index of the first line after the block. Only
/// single-level maps are accepted inside a sequence; anything deeper is refused
/// rather than silently flattened.
fn parse_block(
    lines: &[Line<'_>],
    mut index: usize,
    source: &str,
    key: &str,
    key_line: usize,
) -> Result<(Value, usize)> {
    let mut items: Vec<Value> = Vec::new();

    loop {
        // Blank lines and comments inside a block are skipped, not interpreted.
        while index < lines.len() && is_skippable(lines[index].content) {
            index += 1;
        }
        if index >= lines.len() {
            break;
        }
        let line = &lines[index];
        let indent = indent_of(line, source)?;
        if indent == 0 {
            break;
        }
        let text = line.content.trim();
        let Some(rest) = text.strip_prefix('-') else {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: expected a \"- item\" entry under {:?}, got {:?}",
                    line.no, key, text
                ),
            ));
        };
        // A dash must be exactly `-` or `- `: `-foo` is a scalar, not an item.
        if !rest.is_empty() && !rest.starts_with(' ') {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: expected \"- item\" under {:?}, got {:?}; put a space after the dash",
                    line.no, key, text
                ),
            ));
        }
        let dash_indent = indent;
        let item_text = rest.trim();
        if item_text.is_empty() {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {}: list item under {:?} is empty; write the value after the dash",
                    line.no, key
                ),
            ));
        }

        index += 1;
        if let Some((item_key, item_value)) = split_mapping(item_text) {
            if !valid_key(&item_key) {
                return Err(Error::new(
                    ErrorKind::BadFrontmatter,
                    format!(
                        "{source}: line {}: {} is not a usable field name inside a {:?} entry",
                        line.no,
                        crate::util::quote(&item_key),
                        key
                    ),
                ));
            }
            let mut entries: Vec<(String, Value)> = Vec::new();
            if item_value.is_empty() {
                return Err(Error::new(
                    ErrorKind::BadFrontmatter,
                    format!(
                        "{source}: line {}: {:?} has no value inside a {:?} entry; nested blocks are not supported",
                        line.no, item_key, key
                    ),
                ));
            }
            entries.push((item_key, Value::Scalar(unquote(&item_value))));

            // Continuation lines of this item's map are indented deeper than
            // its dash; a line at or before the dash's indent belongs to the
            // next item or to the parent block.
            while index < lines.len() {
                if is_skippable(lines[index].content) {
                    index += 1;
                    continue;
                }
                let inner = &lines[index];
                let inner_indent = indent_of(inner, source)?;
                if inner_indent <= dash_indent {
                    break;
                }
                let inner_text = inner.content.trim();
                if inner_text.starts_with('-') {
                    return Err(Error::new(
                        ErrorKind::BadFrontmatter,
                        format!(
                            "{source}: line {}: nested list under {:?} is not supported; a {:?} entry is a flat map of key: value",
                            inner.no, key, key
                        ),
                    ));
                }
                let Some((inner_key, inner_value)) = split_mapping(inner_text) else {
                    return Err(Error::new(
                        ErrorKind::BadFrontmatter,
                        format!(
                            "{source}: line {}: expected \"key: value\" in a {:?} entry, got {:?}",
                            inner.no, key, inner_text
                        ),
                    ));
                };
                if !valid_key(&inner_key) {
                    return Err(Error::new(
                        ErrorKind::BadFrontmatter,
                        format!(
                            "{source}: line {}: {} is not a usable field name inside a {:?} entry",
                            inner.no,
                            crate::util::quote(&inner_key),
                            key
                        ),
                    ));
                }
                if inner_value.is_empty() {
                    return Err(Error::new(
                        ErrorKind::BadFrontmatter,
                        format!(
                            "{source}: line {}: {:?} has no value inside a {:?} entry; nested blocks are not supported",
                            inner.no, inner_key, key
                        ),
                    ));
                }
                if entries.iter().any(|(name, _)| name == &inner_key) {
                    return Err(Error::new(
                        ErrorKind::BadFrontmatter,
                        format!(
                            "{source}: line {}: field {:?} appears more than once in one {:?} entry",
                            inner.no, inner_key, key
                        ),
                    ));
                }
                entries.push((inner_key, Value::Scalar(unquote(&inner_value))));
                index += 1;
            }
            items.push(Value::Map(entries));
        } else {
            items.push(Value::Scalar(unquote(item_text)));
        }
    }

    if items.is_empty() {
        return Err(Error::new(
            ErrorKind::BadFrontmatter,
            format!(
                "{source}: line {key_line}: field {:?} has no value; write an inline list like [a, b] or a \"- item\" block",
                key
            ),
        ));
    }
    Ok((Value::List(items), index))
}

/// Split `key: value` for a list entry, where the colon must be followed by a
/// space or end the text. `http://x` inside a value must not read as a key.
fn split_mapping(text: &str) -> Option<(String, String)> {
    let (key, rest) = text.split_once(':')?;
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((key.trim().to_string(), rest.trim().to_string()))
}

/// Parse `[a, b, "c"]`. An empty `[]` is a legal empty list.
fn parse_flow_list(text: &str, source: &str, line: usize, key: &str) -> Result<Value> {
    let inner = text
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {line}: field {key:?} starts a list that does not close on the same line; write it as [a, b] or use a \"- item\" block"
                ),
            )
        })?;
    if inner.trim().is_empty() {
        return Ok(Value::List(Vec::new()));
    }
    let mut items = Vec::new();
    for part in split_flow(inner) {
        let item = unquote(part.trim());
        if item.is_empty() {
            return Err(Error::new(
                ErrorKind::BadFrontmatter,
                format!(
                    "{source}: line {line}: field {key:?} has an empty item; remove the stray comma"
                ),
            ));
        }
        items.push(Value::Scalar(item));
    }
    Ok(Value::List(items))
}

/// Split a flow sequence body on commas that are not inside quotes.
fn split_flow(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_double => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ',' if !in_single && !in_double => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// Remove one matching layer of ASCII quotes; no escapes are interpreted.
fn unquote(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[value.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(data: &[u8]) -> Frontmatter {
        parse(data, "expert.md").expect("must parse")
    }

    fn parse_err(data: &[u8]) -> Error {
        parse(data, "expert.md").expect_err("must refuse")
    }

    /// The scalar text of one key, the way the roster reads it.
    fn scalar<'a>(fm: &'a Frontmatter, key: &str) -> Option<&'a str> {
        fm.get(key).and_then(Value::as_str)
    }

    #[test]
    fn scalars_inline_lists_and_a_verbatim_body() {
        let file = "---\nname: Rust 并发审查\nslug: rust-concurrency-review\ndescription: Reviews lock ordering\ndivision: engineering\nenabled: true\ntools: [fs_read, fs_grep]\n---\nThe system prompt, as the body of the file.\n";
        let parsed = parse_ok(file.as_bytes());
        assert_eq!(scalar(&parsed, "name"), Some("Rust 并发审查"));
        assert_eq!(scalar(&parsed, "slug"), Some("rust-concurrency-review"));
        assert_eq!(scalar(&parsed, "enabled"), Some("true"));
        assert_eq!(
            parsed
                .get("tools")
                .and_then(Value::as_list)
                .map(|items| items.iter().filter_map(Value::as_str).collect::<Vec<_>>()),
            Some(vec!["fs_read", "fs_grep"])
        );
        assert_eq!(parsed.body, "The system prompt, as the body of the file.\n");
    }

    #[test]
    fn body_is_byte_identical_for_crlf_tabs_dashes_and_non_ascii() {
        // The body contains everything that a "normalising" loader would eat:
        // CRLF, a tab, a line that looks like a closing fence, and non-ASCII.
        let mut file = Vec::new();
        file.extend_from_slice(b"---\r\nname: x\r\nslug: x\r\ndescription: d\r\n---\r\n");
        let body_start = file.len();
        // A byte string literal may not carry non-ASCII, so the body is built
        // from a normal string literal and its bytes.
        file.extend_from_slice(
            "line one\r\n\tindented with a tab\r\n---\r\n中文提示\r\n\r\n".as_bytes(),
        );
        let parsed = parse(&file, "expert.md");
        let parsed = parsed.expect("must parse");
        let expected = std::str::from_utf8(&file[body_start..]).expect("utf8");
        assert_eq!(parsed.body, expected);
        assert!(parsed.body.contains("\r\n"));
        assert!(parsed.body.contains('\t'));
        assert!(parsed.body.contains("\n---\r\n"));
        assert!(parsed.body.ends_with("\r\n\r\n"));
    }

    #[test]
    fn block_sequence_of_maps_keeps_file_order() {
        let file = "---\nname: Release squad\nslug: release-squad\ncoordinator: release-lead\nmembers:\n  - expert: bench-owner\n    duty: re-records the baselines\n  - expert: rust-concurrency-review\n    duty: reviews the lock ordering\n---\ntext\n";
        let parsed = parse_ok(file.as_bytes());
        let members = parsed
            .get("members")
            .and_then(Value::as_list)
            .expect("members list");
        assert_eq!(members.len(), 2);
        let first = members[0].as_map().expect("map item");
        assert_eq!(first[0].0, "expert");
        assert_eq!(first[0].1.as_str(), Some("bench-owner"));
        assert_eq!(first[1].1.as_str(), Some("re-records the baselines"));
        let second = members[1].as_map().expect("map item");
        assert_eq!(second[0].1.as_str(), Some("rust-concurrency-review"));
        // File order, not sorted: bench-owner sorts after rust-... alphabetically.
        assert_eq!(first[0].1.as_str().unwrap(), "bench-owner");
    }

    #[test]
    fn block_sequence_of_scalars() {
        let file = "---\nname: x\nslug: x\ndescription: d\ntools:\n  - fs_read\n  - fs_grep\n---\n";
        let parsed = parse_ok(file.as_bytes());
        let tools = parsed.get("tools").and_then(Value::as_list).expect("list");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].as_str(), Some("fs_read"));
        assert_eq!(tools[1].as_str(), Some("fs_grep"));
    }

    #[test]
    fn empty_body_is_empty_not_a_newline() {
        assert_eq!(
            parse_ok(b"---\nname: x\nslug: x\ndescription: d\n---\n").body,
            ""
        );
        assert_eq!(
            parse_ok(b"---\nname: x\nslug: x\ndescription: d\n---").body,
            ""
        );
        assert_eq!(
            parse_ok(b"---\nname: x\nslug: x\ndescription: d\n---\n\n").body,
            "\n"
        );
    }

    #[test]
    fn comments_blank_lines_and_quotes_are_accepted() {
        let file = "---\n\n# a comment\nname: 'quoted name'\ndescription: \"quoted description\"\n\n---\nbody";
        let parsed = parse_ok(file.as_bytes());
        assert_eq!(scalar(&parsed, "name"), Some("quoted name"));
        assert_eq!(scalar(&parsed, "description"), Some("quoted description"));
        assert_eq!(parsed.fields.len(), 2);
    }

    #[test]
    fn a_description_may_contain_a_colon() {
        let parsed = parse_ok(
            b"---\nname: x\nslug: x\ndescription: Reviews ordering: Send/Sync and locks\n---\n",
        );
        assert_eq!(
            scalar(&parsed, "description"),
            Some("Reviews ordering: Send/Sync and locks")
        );
    }

    #[test]
    fn bom_is_stripped_before_the_fence() {
        let mut file = vec![0xEF, 0xBB, 0xBF];
        file.extend_from_slice(b"---\nname: x\nslug: x\ndescription: d\n---\nbody");
        let parsed = parse(&file, "expert.md").expect("BOM must be tolerated");
        assert_eq!(parsed.body, "body");
    }

    #[test]
    fn missing_frontmatter_is_refused() {
        let err = parse_err(b"Just a prompt.\n");
        assert_eq!(err.kind(), ErrorKind::MissingFrontmatter);
        assert!(
            err.message().contains("line 1 must be ---"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn empty_file_is_refused() {
        let err = parse_err(b"");
        assert_eq!(err.kind(), ErrorKind::MissingFrontmatter);
        assert!(err.message().contains("file is empty"));
    }

    #[test]
    fn unclosed_frontmatter_is_refused() {
        let err = parse_err(b"---\nname: x\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("not closed by a second --- line"));
    }

    #[test]
    fn duplicate_key_is_refused() {
        let err = parse_err(b"---\nname: a\nslug: x\nslug: y\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("\"slug\" appears more than once"));
    }

    #[test]
    fn a_line_without_a_colon_is_refused() {
        let err = parse_err(b"---\nname x\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("expected \"key: value\""));
    }

    #[test]
    fn a_stray_indented_line_is_refused() {
        let err = parse_err(b"---\nname: x\n  orphan\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("unexpected indented line"));
    }

    #[test]
    fn a_key_without_a_value_is_refused() {
        let err = parse_err(b"---\nname: x\ntools:\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("\"tools\" has no value"));
    }

    #[test]
    fn a_block_scalar_is_refused() {
        let err = parse_err(b"---\nname: x\ndescription: >-\n  long\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("block scalar"));
    }

    #[test]
    fn an_unclosed_flow_list_is_refused() {
        let err = parse_err(b"---\nname: x\ntools: [fs_read\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("does not close on the same line"));
    }

    #[test]
    fn an_empty_flow_list_is_legal() {
        let parsed = parse_ok(b"---\nname: x\nslug: x\ndescription: d\ntools: []\n---\n");
        assert_eq!(parsed.get("tools").and_then(Value::as_list), Some(&[][..]));
    }

    #[test]
    fn a_tab_indent_is_refused() {
        let err = parse_err(b"---\nname: x\ntools:\n\t- fs_read\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("indented with a tab"));
    }

    #[test]
    fn a_missing_space_after_the_dash_is_refused() {
        let err = parse_err(b"---\nname: x\ntools:\n  -fs_read\n---\n");
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("put a space after the dash"));
    }

    #[test]
    fn a_nested_list_inside_a_member_is_refused() {
        let err = parse_err(
            b"---\nname: x\nslug: x\nmembers:\n  - expert: a\n    duty:\n      - x\n---\n",
        );
        assert_eq!(err.kind(), ErrorKind::BadFrontmatter);
        assert!(err.message().contains("has no value"));
    }

    #[test]
    fn invalid_utf8_is_unreadable() {
        let err = parse_err(&[b'/', b'/', 0xFF, 0xFE]);
        assert_eq!(err.kind(), ErrorKind::Unreadable);
        assert!(err.message().contains("not valid UTF-8"));
    }

    #[test]
    fn split_mapping_requires_a_space_after_the_colon() {
        assert_eq!(
            split_mapping("expert: bench-owner"),
            Some(("expert".to_string(), "bench-owner".to_string()))
        );
        assert_eq!(
            split_mapping("expert:"),
            Some(("expert".to_string(), String::new()))
        );
        assert_eq!(split_mapping("http://example"), None);
        assert_eq!(split_mapping("no colon"), None);
    }

    #[test]
    fn unquote_removes_one_layer_only() {
        assert_eq!(unquote("'a'"), "a");
        assert_eq!(unquote("\"a\""), "a");
        assert_eq!(unquote("'a\""), "'a\"");
        assert_eq!(unquote("a"), "a");
        assert_eq!(unquote("'"), "'");
    }

    #[test]
    fn valid_key_accepts_the_roster_vocabulary() {
        for key in [
            "name",
            "slug",
            "description",
            "division",
            "enabled",
            "tools",
            "members",
        ] {
            assert!(valid_key(key), "{key} should be legal");
        }
        for key in ["", "1bad", "no spaces", "dot.key"] {
            assert!(!valid_key(key), "{key} should be refused");
        }
    }
}
