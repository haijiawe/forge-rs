//! Frontmatter parsing for `SKILL.md`.
//!
//! This is deliberately a scalar subset, not a YAML parser: a `---` fence,
//! `key: value` lines, and free-form body text. Anything structured is
//! reported as a warning and the key is dropped, never guessed at.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, ErrorKind, Result};
use crate::util::quote;

/// The result of parsing one `SKILL.md` byte buffer.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct ParsedFile {
    /// Scalar fields that were accepted.
    pub fields: BTreeMap<String, String>,
    /// False when the file had no leading `---` fence.
    pub has_frontmatter: bool,
    /// Text after the closing fence (or the whole file when there is none).
    pub body: String,
    /// Non-fatal findings that must reach `Set::warnings`.
    pub warnings: Vec<String>,
}

/// Parse one file without touching the filesystem.
pub(crate) fn parse_frontmatter(data: &[u8]) -> Result<ParsedFile> {
    let text = normalise_newlines(data);
    let lines: Vec<&str> = text.split('\n').collect();
    if !is_fence(lines[0]) {
        return Ok(ParsedFile {
            has_frontmatter: false,
            body: text,
            warnings: vec!["no frontmatter; name falls back to the directory name".to_string()],
            ..ParsedFile::default()
        });
    }

    let mut close = None;
    for (index, line) in lines.iter().enumerate().skip(1) {
        if is_fence(line) {
            close = Some(index);
            break;
        }
    }
    let Some(close) = close else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "frontmatter is not closed by a second --- line",
        ));
    };

    let (fields, warnings) = parse_scalars(&lines[1..close]);
    let body = lines[(close + 1)..].join("\n");
    Ok(ParsedFile {
        fields,
        has_frontmatter: true,
        body: body.trim_start_matches('\n').to_string(),
        warnings,
    })
}

/// Parse only the scalar lines between the two fences.
fn parse_scalars(lines: &[&str]) -> (BTreeMap<String, String>, Vec<String>) {
    let mut fields = BTreeMap::new();
    let mut dropped = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut last_key = String::new();

    for (index, raw) in lines.iter().enumerate() {
        let line_no = index + 2;
        let line = raw.trim_end_matches([' ', '\t']);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if line != trimmed || trimmed.starts_with('-') {
            if !last_key.is_empty() && !dropped.contains(&last_key) {
                dropped.insert(last_key.clone());
                fields.remove(&last_key);
                warnings.push(format!(
                    "line {line_no}: {} has a nested or multi-line value, which is not supported; key dropped",
                    quote(&last_key)
                ));
            } else if last_key.is_empty() {
                warnings.push(format!(
                    "line {line_no}: unexpected indented text in frontmatter; ignored"
                ));
            }
            continue;
        }

        let Some((key_raw, value_raw)) = line.split_once(':') else {
            warnings.push(format!("line {line_no}: not a key: value pair; ignored"));
            last_key.clear();
            continue;
        };

        let key = key_raw.trim();
        if !valid_key(key) {
            warnings.push(format!(
                "line {line_no}: {} is not a valid frontmatter key; ignored",
                quote(key)
            ));
            last_key.clear();
            continue;
        }
        last_key = key.to_string();

        let value = value_raw.trim();
        if is_block_scalar(value) {
            dropped.insert(key.to_string());
            warnings.push(format!(
                "line {line_no}: {} uses a block scalar (multi-line value), which is not supported; key dropped",
                quote(key)
            ));
            continue;
        }
        if fields.contains_key(key) {
            warnings.push(format!(
                "line {line_no}: duplicate key {}; the first value wins",
                quote(key)
            ));
            continue;
        }
        fields.insert(key.to_string(), unquote(value));
    }

    (fields, warnings)
}

/// Strip a UTF-8 BOM and normalise CRLF to LF.
pub(crate) fn normalise_newlines(data: &[u8]) -> String {
    let bytes = match data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => rest,
        None => data,
    };
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn is_fence(line: &str) -> bool {
    line.trim() == "---"
}

fn is_block_scalar(value: &str) -> bool {
    value.starts_with('|') || value.starts_with('>')
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
}

/// Remove one matching layer of ASCII quotes; no escapes are interpreted.
fn unquote(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    struct Case {
        name: &'static str,
        input: &'static str,
        fields: &'static [(&'static str, &'static str)],
        has_frontmatter: bool,
        body: &'static str,
        warnings: &'static [&'static str],
    }

    #[test]
    fn parse_frontmatter_cases() {
        let cases = vec![
            Case {
                name: "empty file",
                input: "",
                fields: &[],
                has_frontmatter: false,
                body: "",
                warnings: &["no frontmatter; name falls back to the directory name"],
            },
            Case {
                name: "scalar fields",
                input: "---\nname: alpha\ndescription:  A thing  \n---\nbody",
                fields: &[("name", "alpha"), ("description", "A thing")],
                has_frontmatter: true,
                body: "body",
                warnings: &[],
            },
            Case {
                name: "BOM and CRLF",
                input: "\u{feff}---\r\nname: bom\r\ndescription: windows\r\n---\r\nline 1\r\n\r\nline 2",
                fields: &[("name", "bom"), ("description", "windows")],
                has_frontmatter: true,
                body: "line 1\n\nline 2",
                warnings: &[],
            },
            Case {
                name: "empty frontmatter",
                input: "---\n---\nbody",
                fields: &[],
                has_frontmatter: true,
                body: "body",
                warnings: &[],
            },
            Case {
                name: "leading blank lines in body",
                input: "---\nname: a\ndescription: b\n---\n\n\nbody\n",
                fields: &[("name", "a"), ("description", "b")],
                has_frontmatter: true,
                body: "body\n",
                warnings: &[],
            },
            Case {
                name: "horizontal rule in body",
                input: "---\nname: a\ndescription: b\n---\nfirst\n---\nlast\n",
                fields: &[("name", "a"), ("description", "b")],
                has_frontmatter: true,
                body: "first\n---\nlast\n",
                warnings: &[],
            },
            Case {
                name: "single quoted value",
                input: "---\nname: 'alpha'\ndescription: 'A thing'\n---\n",
                fields: &[("name", "alpha"), ("description", "A thing")],
                has_frontmatter: true,
                body: "",
                warnings: &[],
            },
            Case {
                name: "double quoted value",
                input: "---\nname: \"alpha\"\ndescription: \"A thing\"\n---\n",
                fields: &[("name", "alpha"), ("description", "A thing")],
                has_frontmatter: true,
                body: "",
                warnings: &[],
            },
            Case {
                name: "unmatched quote is literal",
                input: "---\nname: 'alpha\ndescription: alpha'\n---\n",
                fields: &[("name", "'alpha"), ("description", "alpha'")],
                has_frontmatter: true,
                body: "",
                warnings: &[],
            },
            Case {
                name: "empty quoted value",
                input: "---\nname: \"\"\ndescription: ''\n---\n",
                fields: &[("name", ""), ("description", "")],
                has_frontmatter: true,
                body: "",
                warnings: &[],
            },
            Case {
                name: "comments and blank lines ignored",
                input: "---\n\n# a comment\nname: alpha\n\n# another\ndescription: beta\n---\n",
                fields: &[("name", "alpha"), ("description", "beta")],
                has_frontmatter: true,
                body: "",
                warnings: &[],
            },
            Case {
                name: "duplicate key keeps first",
                input: "---\nname: first\ndescription: good\nname: second\n---\n",
                fields: &[("name", "first"), ("description", "good")],
                has_frontmatter: true,
                body: "",
                warnings: &["line 4: duplicate key \"name\"; the first value wins"],
            },
            Case {
                name: "block scalar key dropped",
                input: "---\nname: alpha\ndescription: >-\n  long\n  text\n---\n",
                fields: &[("name", "alpha")],
                has_frontmatter: true,
                body: "",
                warnings: &[
                    "line 3: \"description\" uses a block scalar (multi-line value), which is not supported; key dropped",
                ],
            },
            Case {
                name: "nested list key dropped once",
                input: "---\nname: alpha\ndescription: beta\ntags:\n  - one\n  - two\n---\n",
                fields: &[("name", "alpha"), ("description", "beta")],
                has_frontmatter: true,
                body: "",
                warnings: &[
                    "line 5: \"tags\" has a nested or multi-line value, which is not supported; key dropped",
                ],
            },
            Case {
                name: "indented text with no key",
                input: "---\n  orphan\nname: alpha\ndescription: beta\n---\n",
                fields: &[("name", "alpha"), ("description", "beta")],
                has_frontmatter: true,
                body: "",
                warnings: &["line 2: unexpected indented text in frontmatter; ignored"],
            },
            Case {
                name: "line without colon ignored",
                input: "---\nname: alpha\ndescription: beta\nnonsense\n---\n",
                fields: &[("name", "alpha"), ("description", "beta")],
                has_frontmatter: true,
                body: "",
                warnings: &["line 4: not a key: value pair; ignored"],
            },
            Case {
                name: "invalid keys ignored",
                input: "---\n1bad: x\nno spaces: y\nname: alpha\ndescription: beta\n---\n",
                fields: &[("name", "alpha"), ("description", "beta")],
                has_frontmatter: true,
                body: "",
                warnings: &[
                    "line 2: \"1bad\" is not a valid frontmatter key; ignored",
                    "line 3: \"no spaces\" is not a valid frontmatter key; ignored",
                ],
            },
            Case {
                name: "valid unusual key",
                input: "---\nmy-key_2.name: yes\nname: alpha\ndescription: beta\n---\n",
                fields: &[
                    ("my-key_2.name", "yes"),
                    ("name", "alpha"),
                    ("description", "beta"),
                ],
                has_frontmatter: true,
                body: "",
                warnings: &[],
            },
            Case {
                name: "indented comment ignored",
                input: "---\n  # comment\nname: alpha\ndescription: beta\n---\n",
                fields: &[("name", "alpha"), ("description", "beta")],
                has_frontmatter: true,
                body: "",
                warnings: &[],
            },
            Case {
                name: "no frontmatter body is whole file",
                input: "just a body\nwith two lines\n",
                fields: &[],
                has_frontmatter: false,
                body: "just a body\nwith two lines\n",
                warnings: &["no frontmatter; name falls back to the directory name"],
            },
        ];

        for case in cases {
            let got = parse_frontmatter(case.input.as_bytes()).unwrap_or_else(|err| {
                panic!("{}: unexpected parse error: {}", case.name, err.message())
            });
            assert_eq!(got.fields, map(case.fields), "case: {}", case.name);
            assert_eq!(
                got.has_frontmatter, case.has_frontmatter,
                "case: {}",
                case.name
            );
            assert_eq!(got.body, case.body, "case: {}", case.name);
            assert_eq!(
                got.warnings,
                case.warnings
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn parse_frontmatter_rejects_unclosed_fence() {
        let err = parse_frontmatter(b"---\nname: alpha\n").expect_err("must fail");
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        assert_eq!(
            err.message(),
            "frontmatter is not closed by a second --- line"
        );
    }

    #[test]
    fn parse_scalars_ignores_second_continuation_line() {
        let lines = vec!["tags:", "  - one", "  - two", "name: alpha"];
        let (fields, warnings) = parse_scalars(&lines);
        assert_eq!(fields, map(&[("name", "alpha")]));
        assert_eq!(warnings.len(), 1, "warnings: {warnings:?}");
    }

    #[test]
    fn scalar_helpers_match_go() {
        assert!(is_fence("---"));
        assert!(is_fence("  ---  "));
        assert!(!is_fence("----"));
        assert!(is_block_scalar("|"));
        assert!(is_block_scalar(">-"));
        assert!(is_block_scalar(">- text"));
        assert!(valid_key("name"));
        assert!(valid_key("_x-2.3"));
        assert!(valid_key("no.dot"));
        assert!(!valid_key(""));
        assert!(!valid_key("9lives"));
        assert!(!valid_key("two words"));
        assert_eq!(unquote("'abc'"), "abc");
        assert_eq!(unquote("\"abc\""), "abc");
        assert_eq!(unquote("'abc\""), "'abc\"");
        assert_eq!(unquote("abc"), "abc");
    }

    #[test]
    fn normalise_newlines_strips_bom_and_crlf() {
        assert_eq!(
            normalise_newlines(&[0xEF, 0xBB, 0xBF, b'a', b'\r', b'\n', b'b']),
            "a\nb"
        );
        assert_eq!(normalise_newlines(b"a\rb"), "a\rb");
    }
}
