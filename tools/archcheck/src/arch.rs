//! Reads `architecture.yaml`.
//!
//! This is deliberately not a YAML parser. The file has four keys, and a YAML
//! crate would be a dependency that has to answer "why can the standard
//! library not do this" — for a document this tool both writes and reads, the
//! honest answer is that it can, with about a page of code. Anything outside
//! the supported subset is an error with a line number, never a guess.

use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct CratePlacement {
    pub layer: String,
    pub line: usize,
}

#[derive(Debug)]
pub struct Architecture {
    /// crate name -> layer it is pinned to
    pub crates: BTreeMap<String, CratePlacement>,
    /// layer name -> rank, L0 = 0
    pub ranks: HashMap<String, usize>,
    /// same-layer edges that are forbidden even though the layer rule allows them
    pub forbidden_edges: Vec<(String, String)>,
    pub ignore_dirs: Vec<String>,
}

impl Architecture {
    pub fn rank_of(&self, crate_name: &str) -> Option<usize> {
        self.crates
            .get(crate_name)
            .and_then(|placement| self.ranks.get(&placement.layer).copied())
    }

    pub fn is_forbidden(&self, from: &str, to: &str) -> bool {
        self.forbidden_edges
            .iter()
            .any(|(f, t)| f == from && t == to)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Layers,
    ForbiddenEdges,
    IgnoreDirs,
}

pub fn parse(text: &str) -> Result<Architecture, String> {
    let mut version: Option<u32> = None;
    let mut layers: Vec<(usize, String, Vec<String>, usize)> = Vec::new();
    let mut forbidden_edges = Vec::new();
    let mut ignore_dirs = vec![
        ".git".to_string(),
        "target".to_string(),
        "node_modules".to_string(),
    ];
    let mut section = Section::None;

    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        if indent == 0 {
            let (key, value) = split_key_value(trimmed).ok_or_else(|| {
                format!("line {line_no}: expected \"key: value\", got {trimmed:?}")
            })?;
            match key.as_str() {
                "version" => {
                    let parsed = value.parse::<u32>().map_err(|_| {
                        format!("line {line_no}: version must be a positive integer")
                    })?;
                    if parsed < 1 {
                        return Err(format!(
                            "line {line_no}: version must be a positive integer"
                        ));
                    }
                    version = Some(parsed);
                    section = Section::None;
                }
                "layers" => {
                    if !value.is_empty() {
                        return Err(format!("line {line_no}: layers must be a mapping"));
                    }
                    section = Section::Layers;
                }
                "forbidden_edges" => {
                    if value.is_empty() {
                        section = Section::ForbiddenEdges;
                    } else {
                        let pair =
                            parse_list(&value).map_err(|err| format!("line {line_no}: {err}"))?;
                        if pair.is_empty() {
                            section = Section::None;
                        } else {
                            if pair.len() != 2 || pair.iter().any(|item| item.is_empty()) {
                                return Err(format!(
                                    "line {line_no}: forbidden_edges entries must be [from_crate, to_crate]"
                                ));
                            }
                            forbidden_edges.push((pair[0].clone(), pair[1].clone()));
                            section = Section::None;
                        }
                    }
                }
                "ignore_dirs" => {
                    ignore_dirs =
                        parse_list(&value).map_err(|err| format!("line {line_no}: {err}"))?;
                    section = Section::IgnoreDirs;
                }
                other => return Err(format!("line {line_no}: unknown key {other:?}")),
            }
            continue;
        }

        match section {
            Section::Layers => {
                let (name, value) = split_key_value(trimmed).ok_or_else(|| {
                    format!("line {line_no}: expected \"L<n>: [crate, ...]\", got {trimmed:?}")
                })?;
                let rank = parse_layer(&name).ok_or_else(|| {
                    format!("line {line_no}: layer {name:?} must look like L0, L1, ...")
                })?;
                let crates = parse_list(&value)
                    .map_err(|err| format!("line {line_no}: layer {name}: {err}"))?;
                if crates.is_empty() {
                    return Err(format!("line {line_no}: layer {name} declares no crates"));
                }
                layers.push((rank, name, crates, line_no));
            }
            Section::ForbiddenEdges => {
                let rest = trimmed.strip_prefix("- ").ok_or_else(|| {
                    format!(
                        "line {line_no}: expected a \"- [from, to]\" entry under forbidden_edges"
                    )
                })?;
                let pair = parse_list(rest).map_err(|err| format!("line {line_no}: {err}"))?;
                if pair.len() != 2 || pair.iter().any(|item| item.is_empty()) {
                    return Err(format!(
                        "line {line_no}: forbidden_edges entries must be [from_crate, to_crate]"
                    ));
                }
                forbidden_edges.push((pair[0].clone(), pair[1].clone()));
            }
            Section::IgnoreDirs => {
                let rest = trimmed.strip_prefix("- ").ok_or_else(|| {
                    format!("line {line_no}: expected a \"- name\" entry under ignore_dirs")
                })?;
                ignore_dirs.push(unquote(rest));
            }
            Section::None => {
                return Err(format!(
                    "line {line_no}: unexpected indented line {trimmed:?}"
                ));
            }
        }
    }

    if version.is_none() {
        return Err("missing key \"version\"".to_string());
    }
    if layers.is_empty() {
        return Err("missing or empty \"layers\" mapping".to_string());
    }

    layers.sort_by_key(|(rank, _, _, _)| *rank);
    let mut ranks = HashMap::new();
    let mut seen_crates: HashSet<String> = HashSet::new();
    let mut crates = BTreeMap::new();
    for (rank, name, names, line_no) in layers {
        if ranks.contains_key(&name) {
            return Err(format!("line {line_no}: duplicate layer {name:?}"));
        }
        ranks.insert(name.clone(), rank);
        for crate_name in names {
            if crate_name.is_empty() {
                return Err(format!(
                    "line {line_no}: layer {name} contains an empty crate name"
                ));
            }
            if !seen_crates.insert(crate_name.clone()) {
                return Err(format!(
                    "line {line_no}: crate {crate_name:?} is declared in more than one layer"
                ));
            }
            crates.insert(
                crate_name,
                CratePlacement {
                    layer: name.clone(),
                    line: line_no,
                },
            );
        }
    }

    Ok(Architecture {
        crates,
        ranks,
        forbidden_edges,
        ignore_dirs,
    })
}

fn parse_layer(name: &str) -> Option<usize> {
    let digits = name.strip_prefix('L')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

// "L2: [hearth-core, hearth-config]" -> ("L2", "[hearth-core, hearth-config]").
// The colon must be followed by whitespace or end-of-line so a Windows path in
// a value (C:\...) is not mistaken for a key.
fn split_key_value(text: &str) -> Option<(String, String)> {
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
            ':' if !in_single && !in_double => {
                let rest = &text[index + 1..];
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    return Some((text[..index].trim().to_string(), rest.trim().to_string()));
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_comment(text: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut previous_whitespace = true;
    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            previous_whitespace = ch.is_whitespace();
            continue;
        }
        match ch {
            '\\' if in_double => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double && previous_whitespace => {
                return &text[..index];
            }
            _ => {}
        }
        previous_whitespace = ch.is_whitespace();
    }
    text
}

fn parse_list(text: &str) -> Result<Vec<String>, String> {
    let text = text.trim();
    if !text.starts_with('[') || !text.ends_with(']') {
        return Err(format!("expected a list like [a, b], got {text:?}"));
    }
    let inner = &text[1..text.len() - 1];
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for part in split_flow(inner) {
        let item = unquote(part.trim());
        if item.is_empty() {
            return Err(format!("empty item in list {text:?}"));
        }
        items.push(item);
    }
    Ok(items)
}

fn split_flow(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
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

fn unquote(text: &str) -> String {
    let text = text.trim();
    if text.len() >= 2 {
        let first = text.as_bytes()[0];
        let last = text.as_bytes()[text.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return text[1..text.len() - 1].to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_checked_in_shape() {
        let architecture = parse(
            "version: 1\n\
             layers:\n\
             \x20 L0: [hearth-core, hearth-config]\n\
             \x20 L1: [hearth-exec]\n\
             forbidden_edges:\n\
             \x20 - [hearth-core, hearth-config]\n\
             ignore_dirs: [target]\n",
        )
        .expect("valid architecture");
        assert_eq!(architecture.rank_of("hearth-core"), Some(0));
        assert_eq!(architecture.rank_of("hearth-exec"), Some(1));
        assert!(architecture.is_forbidden("hearth-core", "hearth-config"));
        assert_eq!(architecture.ignore_dirs, vec!["target".to_string()]);
    }

    #[test]
    fn rejects_a_duplicate_crate() {
        let err = parse("version: 1\nlayers:\n  L0: [a]\n  L1: [a]\n")
            .expect_err("duplicate crate must be rejected");
        assert!(err.contains("more than one layer"), "{err}");
    }

    #[test]
    fn rejects_an_unknown_top_level_key() {
        let err = parse("version: 1\nlayers:\n  L0: [a]\nwat: true\n")
            .expect_err("unknown key must be rejected");
        assert!(err.contains("unknown key"), "{err}");
    }

    #[test]
    fn accepts_an_empty_forbidden_edges_list() {
        let architecture = parse("version: 1\nlayers:\n  L0: [hearth-core]\nforbidden_edges: []\n")
            .expect("empty forbidden_edges is valid");
        assert!(architecture.forbidden_edges.is_empty());
    }
}
