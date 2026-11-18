//! Ordered policy rules, ported from `forge-go/internal/tool/policy.go`.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ApprovalRequest, Call, Decision, Policy};

pub const ACTION_ALLOW: &str = "allow";
pub const ACTION_ASK: &str = "ask";
pub const ACTION_DENY: &str = "deny";

const TOOL_ANY: &str = "*";
const PATH_ANY: &str = "**";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub action: String,
}

impl Rule {
    pub fn new(
        tool: impl Into<String>,
        path: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            tool: tool.into(),
            path: path.into(),
            action: action.into(),
        }
    }
}

/// An ordered allow/ask/deny list. The zero value denies everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    pub rules: Vec<Rule>,
    fallback: Decision,
    fallback_reason: String,
    note: String,
}

impl Rules {
    pub fn from_rules(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    /// Parse and validate an operator rules file.
    pub fn parse(input: &str) -> std::result::Result<Self, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(Self::default());
        }
        if trimmed.starts_with('[') {
            return Err(
                "rules: expected an object like {\"rules\":[...]}, got a JSON array".into(),
            );
        }
        if trimmed.starts_with('"') {
            return Err(
                "rules: expected an object like {\"rules\":[...]}, got a JSON string".into(),
            );
        }
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            rules: Vec<Rule>,
        }
        let raw: Raw =
            serde_json::from_str(trimmed).map_err(|err| format!("rules: parse JSON: {err}"))?;
        let mut rules = Self::from_rules(raw.rules);
        rules.validate()?;
        Ok(rules)
    }

    fn validate(&mut self) -> std::result::Result<(), String> {
        let mut seen = std::collections::BTreeMap::new();
        for (index, rule) in self.rules.iter_mut().enumerate() {
            rule.tool = rule.tool.trim().to_string();
            rule.path = rule.path.trim().to_string();
            rule.action = rule.action.trim().to_string();

            if rule.tool.is_empty() {
                return Err(format!(
                    "rule {index}: empty tool (want a tool name such as \"fs_read\" or \"*\" for every tool)"
                ));
            }
            if parse_action(&rule.action).is_none() {
                return Err(format!(
                    "rule {index}: unknown action {:?} (want allow|ask|deny)",
                    rule.action
                ));
            }
            if rule.tool != TOOL_ANY && rule.tool.chars().any(|c| matches!(c, '*' | '?' | '[')) {
                return Err(format!(
                    "rule {index}: tool {:?}: only an exact tool name or \"*\" is supported; a partial wildcard would silently never match, which is how a deny stops denying",
                    rule.tool
                ));
            }
            if !rule.path.is_empty() {
                let normalized = normalize_rule_path(&rule.path)
                    .map_err(|err| format!("rule {index}: path {:?}: {err}", rule.path))?;
                rule.path = normalized;
            }
            let key = format!("{}\0{}\0{}", rule.tool, rule.path, rule.action);
            if let Some(previous) = seen.insert(key, index) {
                return Err(format!(
                    "rule {index}: duplicate of rule {previous} ({}); first match wins, so the copy is dead weight — and pasted above the original it silently reverses it",
                    rule_label(rule)
                ));
            }
        }
        Ok(())
    }

    pub fn explain(&self, call: &Call) -> String {
        self.decide(call).1
    }

    fn note_prefix(&self) -> String {
        if self.note.is_empty() {
            String::new()
        } else {
            format!("{}; ", self.note)
        }
    }
}

impl Policy for Rules {
    fn decide(&self, call: &Call) -> (Decision, String) {
        let (path, state) = call_path_arg(call);
        let mut skipped = String::new();
        for (index, rule) in self.rules.iter().enumerate() {
            if !tool_matches(&rule.tool, &call.name) {
                continue;
            }
            if !rule.path.is_empty() {
                match state {
                    PathState::Missing => {
                        if skipped.is_empty() {
                            skipped = format!("rule {index} needs a path; call has none");
                        }
                        continue;
                    }
                    PathState::Outside => {
                        if skipped.is_empty() {
                            skipped = format!(
                                "rule {index} needs a workspace-relative path; call path {:?} is outside the workspace",
                                path
                            );
                        }
                        continue;
                    }
                    PathState::Ok => {}
                }
                if !match_path_glob(&rule.path, &path) {
                    continue;
                }
            }
            let decision = parse_action(&rule.action).unwrap_or(Decision::Deny);
            let reason = format!(
                "rule {index}: tool={} path={} → {decision}",
                rule.tool,
                if rule.path.is_empty() {
                    TOOL_ANY
                } else {
                    &rule.path
                }
            );
            return (decision, format!("{}{}", self.note_prefix(), reason));
        }
        let (mut decision, mut reason) =
            (Decision::Deny, "no rule matched; default deny".to_string());
        if !self.fallback_reason.is_empty() {
            decision = self.fallback;
            reason = self.fallback_reason.clone();
        }
        if !skipped.is_empty() {
            reason.push_str(" (");
            reason.push_str(&skipped);
            reason.push(')');
        }
        (decision, format!("{}{}", self.note_prefix(), reason))
    }
}

/// Built-in policy for a sandbox mode. Unknown modes become read-only.
pub fn default_rules(mode: &str) -> Rules {
    let mut rules = match mode {
        "read-only" => Rules {
            rules: vec![
                Rule::new("fs_read", "", ACTION_ALLOW),
                Rule::new("fs_glob", "", ACTION_ALLOW),
                Rule::new("fs_grep", "", ACTION_ALLOW),
                // Reading an image is reading a file: it changes nothing, and
                // the model can only act on it by asking for it explicitly.
                Rule::new("read_image", "", ACTION_ALLOW),
                Rule::new("present", "", ACTION_ALLOW),
                // A goal's state is bookkeeping the operator already asked
                // for by starting the goal; ending it cannot touch the
                // filesystem, so no mode has a reason to ask.
                Rule::new("goal_end", "", ACTION_ALLOW),
                // The network tools run inside the harness, not inside the
                // sandbox, so a confined mode cannot contain them by itself:
                // the approval prompt is the only gate there is.
                Rule::new("web_fetch", "", ACTION_ASK),
                Rule::new("web_search", "", ACTION_ASK),
            ],
            fallback: Decision::Deny,
            fallback_reason:
                "no rule matched; read-only mode denies everything not explicitly allowed".into(),
            note: String::new(),
        },
        "workspace-write" => Rules {
            rules: vec![
                Rule::new("fs_read", "", ACTION_ALLOW),
                Rule::new("fs_glob", "", ACTION_ALLOW),
                Rule::new("fs_grep", "", ACTION_ALLOW),
                Rule::new("read_image", "", ACTION_ALLOW),
                Rule::new("todo_write", "", ACTION_ALLOW),
                Rule::new("present", "", ACTION_ALLOW),
                // Ending a goal cannot touch anything outside the log.
                Rule::new("goal_end", "", ACTION_ALLOW),
                Rule::new("fs_write", PATH_ANY, ACTION_ALLOW),
                Rule::new("fs_edit", PATH_ANY, ACTION_ALLOW),
                Rule::new("shell", "", ACTION_ASK),
                Rule::new("worktree", "", ACTION_ASK),
                Rule::new("web_fetch", "", ACTION_ASK),
                Rule::new("web_search", "", ACTION_ASK),
            ],
            fallback: Decision::Deny,
            fallback_reason:
                "no rule matched; workspace-write mode denies tools with no matching rule".into(),
            note: String::new(),
        },
        "full-access" => Rules {
            rules: Vec::new(),
            fallback: Decision::Allow,
            fallback_reason: "no rule matched; full-access means no gate, every call is allowed"
                .into(),
            note: String::new(),
        },
        other => {
            let mut read_only = default_rules("read-only");
            read_only.note = format!("unknown mode {other:?}, assuming read-only");
            return read_only;
        }
    };
    rules
        .validate()
        .expect("built-in rules are validated constants");
    rules
}

/// Extra rules win over base rules; the fallback comes from the base set.
pub fn merge(base: Option<&Rules>, extra: Option<&Rules>) -> Rules {
    let mut out = Rules::default();
    if let Some(extra) = extra {
        out.rules.extend(extra.rules.iter().cloned());
    }
    if let Some(base) = base {
        out.rules.extend(base.rules.iter().cloned());
    }
    let source = base
        .filter(|rules| !rules.fallback_reason.is_empty())
        .or_else(|| extra.filter(|rules| !rules.fallback_reason.is_empty()));
    if let Some(source) = source {
        out.fallback = source.fallback;
        out.fallback_reason = source.fallback_reason.clone();
        out.note = source.note.clone();
    }
    if out.note.is_empty() {
        if let Some(extra) = extra {
            out.note = extra.note.clone();
        }
    }
    out
}

fn parse_action(action: &str) -> Option<Decision> {
    match action {
        ACTION_ALLOW => Some(Decision::Allow),
        ACTION_ASK => Some(Decision::Ask),
        ACTION_DENY => Some(Decision::Deny),
        _ => None,
    }
}

fn tool_matches(rule: &str, call: &str) -> bool {
    rule == TOOL_ANY || rule == call
}

fn rule_label(rule: &Rule) -> String {
    format!(
        "tool={} path={} action={}",
        rule.tool,
        if rule.path.is_empty() {
            TOOL_ANY
        } else {
            &rule.path
        },
        rule.action
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathState {
    Ok,
    Missing,
    Outside,
}

fn call_path_arg(call: &Call) -> (String, PathState) {
    let Value::Object(args) = &call.args else {
        return (String::new(), PathState::Missing);
    };
    let Some(Value::String(raw)) = args.get("path") else {
        return (String::new(), PathState::Missing);
    };
    let normalized = raw.trim().replace('\\', "/");
    if normalized.is_empty() {
        return (String::new(), PathState::Missing);
    }
    if !is_slash_abs(&normalized) && escapes_workspace(&normalized) {
        return (normalized, PathState::Outside);
    }
    let clean = clean_slash(&normalized);
    if !is_slash_abs(&clean) {
        if escapes_workspace(&clean) {
            return (clean, PathState::Outside);
        }
        return (clean, PathState::Ok);
    }
    if call.workspace.is_empty() {
        return (clean, PathState::Outside);
    }
    match relative_inside(&call.workspace, &clean) {
        Some(rel) => {
            if escapes_workspace(&rel) {
                (rel, PathState::Outside)
            } else {
                (rel, PathState::Ok)
            }
        }
        None => (clean, PathState::Outside),
    }
}

fn relative_inside(workspace: &str, absolute: &str) -> Option<String> {
    let workspace = workspace.trim().replace('\\', "/");
    let workspace = workspace.trim_end_matches('/');
    let absolute = absolute.trim_end_matches('/');
    let equal = if cfg!(windows) {
        workspace.to_lowercase() == absolute.to_lowercase()
    } else {
        workspace == absolute
    };
    if equal {
        return Some(".".to_string());
    }
    let prefix = format!("{workspace}/");
    let starts = if cfg!(windows) {
        absolute.to_lowercase().starts_with(&prefix.to_lowercase())
    } else {
        absolute.starts_with(&prefix)
    };
    if starts {
        Some(absolute[prefix.len()..].to_string())
    } else {
        None
    }
}

fn escapes_workspace(path: &str) -> bool {
    path == ".." || path.starts_with("../") || is_slash_abs(path)
}

fn is_slash_abs(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/' && bytes[0].is_ascii_alphabetic()
}

fn clean_slash(path: &str) -> String {
    let absolute = is_slash_abs(path);
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                let keep = parts.last().map(|last| *last == "..").unwrap_or(false);
                if !keep {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

fn normalize_rule_path(path: &str) -> std::result::Result<String, String> {
    let normalized = path.trim().replace('\\', "/");
    if normalized.is_empty() {
        return Ok(String::new());
    }
    if is_slash_abs(&normalized) {
        return Err(
            "must be workspace-relative; an absolute glob can never match a path inside the workspace"
                .into(),
        );
    }
    for segment in normalized.split('/') {
        if segment == ".." {
            return Err(
                "must not contain \"..\"; a call's path is already workspace-relative, so \"..\" can never match"
                    .into(),
            );
        }
    }
    let clean = clean_slash(&normalized);
    if clean == "." {
        return Err("must name something inside the workspace (use \"**\" for everything)".into());
    }
    for segment in clean.split('/') {
        if segment == PATH_ANY {
            continue;
        }
        if segment.matches('[').count() != segment.matches(']').count() {
            return Err("unbalanced \"[\" in glob".into());
        }
        validate_glob_segment(segment)
            .map_err(|err| format!("invalid glob segment {segment:?}: {err}"))?;
    }
    Ok(clean)
}

fn validate_glob_segment(pattern: &str) -> std::result::Result<(), String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '\\' => {
                index += 1;
                if index >= chars.len() {
                    return Err("trailing backslash".into());
                }
                index += 1;
            }
            '[' => match parse_class(&chars, index, '\0') {
                Ok((_, next)) => index = next,
                Err(err) => return Err(err),
            },
            _ => index += 1,
        }
    }
    Ok(())
}

fn parse_class(
    chars: &[char],
    start: usize,
    probe: char,
) -> std::result::Result<(bool, usize), String> {
    let mut index = start + 1;
    let mut negate = false;
    if chars.get(index) == Some(&'^') {
        negate = true;
        index += 1;
    }
    let mut matched = false;
    let mut first = true;
    while index < chars.len() {
        if chars[index] == ']' && !first {
            return Ok((matched != negate, index + 1));
        }
        let mut low = chars[index];
        if low == '\\' {
            index += 1;
            if index >= chars.len() {
                return Err("trailing backslash in bracket class".into());
            }
            low = chars[index];
        }
        index += 1;
        if index + 1 < chars.len() && chars[index] == '-' && chars[index + 1] != ']' {
            index += 1;
            let mut high = chars[index];
            if high == '\\' {
                index += 1;
                if index >= chars.len() {
                    return Err("trailing backslash in bracket class".into());
                }
                high = chars[index];
            }
            index += 1;
            if low > high {
                return Err(format!("invalid range {low:?}-{high:?}"));
            }
            if low <= probe && probe <= high {
                matched = true;
            }
        } else if low == probe {
            matched = true;
        }
        first = false;
    }
    Err("unclosed bracket class".into())
}

fn match_path_glob(pattern: &str, name: &str) -> bool {
    match_segments(
        &pattern.split('/').collect::<Vec<_>>(),
        &name.split('/').collect::<Vec<_>>(),
    )
}

fn match_segments(pattern: &[&str], name: &[&str]) -> bool {
    if pattern.is_empty() {
        return name.is_empty();
    }
    if pattern[0] == PATH_ANY {
        return (0..=name.len()).any(|index| match_segments(&pattern[1..], &name[index..]));
    }
    if name.is_empty() {
        return false;
    }
    match_segment(pattern[0], name[0]) && match_segments(&pattern[1..], &name[1..])
}

fn match_segment(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    match_chars(&pattern, &name)
}

fn match_chars(pattern: &[char], name: &[char]) -> bool {
    if pattern.is_empty() {
        return name.is_empty();
    }
    match pattern[0] {
        '*' => (0..=name.len()).any(|index| match_chars(&pattern[1..], &name[index..])),
        '?' => !name.is_empty() && match_chars(&pattern[1..], &name[1..]),
        '[' => {
            if name.is_empty() {
                return false;
            }
            match parse_class(pattern, 0, name[0]) {
                Ok((matched, next)) => matched && match_chars(&pattern[next..], &name[1..]),
                Err(_) => false,
            }
        }
        '\\' => {
            pattern.len() >= 2
                && !name.is_empty()
                && pattern[1] == name[0]
                && match_chars(&pattern[2..], &name[1..])
        }
        literal => !name.is_empty() && literal == name[0] && match_chars(&pattern[1..], &name[1..]),
    }
}

impl fmt::Display for Rules {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rules")
    }
}

/// Keep `ApprovalRequest` reachable from this module for callers wiring policy.
pub fn approval_request(call: Call, reason: impl Into<String>) -> ApprovalRequest {
    ApprovalRequest {
        call,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_match_wins_and_reason_names_rule() {
        let rules = Rules::parse(
            r#"{"rules":[{"tool":"shell","action":"ask"},{"tool":"*","action":"deny"}]}"#,
        )
        .unwrap();
        let call = Call::new("c", "shell", Value::Null, "/ws");
        assert_eq!(
            rules.decide(&call),
            (Decision::Ask, "rule 0: tool=shell path=* → ask".into())
        );
    }

    #[test]
    fn recursive_glob_crosses_directories() {
        let rules = Rules::parse(
            r#"{"rules":[{"tool":"fs_write","path":"internal/**","action":"allow"}]}"#,
        )
        .unwrap();
        let call = Call::new(
            "c",
            "fs_write",
            serde_json::json!({"path": "internal/a/b.go"}),
            "/ws",
        );
        assert_eq!(rules.decide(&call).0, Decision::Allow);
    }

    #[test]
    fn outside_path_does_not_match_rule() {
        let rules = Rules::parse(r#"{"rules":[{"tool":"fs_write","path":"**","action":"allow"}]}"#)
            .unwrap();
        let call = Call::new(
            "c",
            "fs_write",
            serde_json::json!({"path": "../outside"}),
            "/ws",
        );
        let (decision, reason) = rules.decide(&call);
        assert_eq!(decision, Decision::Deny);
        assert!(reason.contains("outside the workspace"), "{reason}");
    }

    /// The platform tools are gated by the mode, not by the sandbox: they run
    /// inside the harness, so a confined mode can only put a person in front of
    /// them. This asserts the built-in table, because a tool that no rule
    /// mentions is denied by the fallback and would silently disappear.
    #[test]
    fn platform_tools_are_gated_by_the_mode() {
        let call = |tool: &str| Call::new("c", tool, serde_json::json!({}), "/ws");

        let read_only = default_rules("read-only");
        assert_eq!(read_only.decide(&call("present")).0, Decision::Allow);
        assert_eq!(read_only.decide(&call("web_fetch")).0, Decision::Ask);
        assert_eq!(read_only.decide(&call("web_search")).0, Decision::Ask);
        assert_eq!(read_only.decide(&call("worktree")).0, Decision::Deny);
        assert_eq!(read_only.decide(&call("shell")).0, Decision::Deny);
        // Ending a goal touches nothing on disk; gating it would mean a goal
        // that cannot be stopped in the mode that can change nothing.
        assert_eq!(read_only.decide(&call("goal_end")).0, Decision::Allow);
        // Reading an image opens a file and writes nothing; if the mode allows
        // fs_read it has no reason to interrogate read_image.
        assert_eq!(read_only.decide(&call("read_image")).0, Decision::Allow);

        let workspace = default_rules("workspace-write");
        assert_eq!(workspace.decide(&call("present")).0, Decision::Allow);
        assert_eq!(workspace.decide(&call("web_fetch")).0, Decision::Ask);
        assert_eq!(workspace.decide(&call("web_search")).0, Decision::Ask);
        assert_eq!(workspace.decide(&call("worktree")).0, Decision::Ask);
        assert_eq!(workspace.decide(&call("shell")).0, Decision::Ask);
        assert_eq!(workspace.decide(&call("fs_read")).0, Decision::Allow);
        assert_eq!(workspace.decide(&call("read_image")).0, Decision::Allow);

        let full = default_rules("full-access");
        for tool in ["present", "web_fetch", "web_search", "worktree", "shell"] {
            assert_eq!(full.decide(&call(tool)).0, Decision::Allow, "{tool}");
        }
    }
}
