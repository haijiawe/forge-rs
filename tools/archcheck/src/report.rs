//! Finding shape and the two renderings: human for a terminal, JSON for CI.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub rule: &'static str,
    pub message: String,
}

impl Finding {
    pub fn error(
        file: impl Into<String>,
        line: usize,
        rule: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Finding {
            file: file.into(),
            line,
            rule,
            message: message.into(),
        }
    }
}

pub fn json(root: &str, findings: &[Finding]) -> String {
    let mut out = String::new();
    // Field order is part of the CI contract: keep it fixed so two runs of the
    // same tree produce byte-identical reports. `warnings` is always zero for
    // now — every rule this gate enforces is an error; the field stays because
    // consumers of the format already expect it.
    out.push_str("{\"root\":");
    out.push_str(&quote(root));
    let _ = write!(
        out,
        ",\"ok\":{},\"errors\":{},\"warnings\":0,\"findings\":[",
        findings.is_empty(),
        findings.len()
    );
    for (index, finding) in findings.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"file\":");
        out.push_str(&quote(&finding.file));
        let _ = write!(out, ",\"line\":{},\"rule\":", finding.line);
        out.push_str(&quote(finding.rule));
        out.push_str(",\"severity\":");
        out.push_str(&quote("error"));
        out.push_str(",\"message\":");
        out.push_str(&quote(&finding.message));
        out.push('}');
    }
    out.push_str("]}\n");
    out
}

pub fn human(root: &str, findings: &[Finding]) -> String {
    let mut out = String::new();
    for finding in findings {
        let _ = writeln!(
            out,
            "{}:{}: {}: {}",
            finding.file, finding.line, finding.rule, finding.message
        );
    }
    if findings.is_empty() {
        let _ = writeln!(out, "archcheck: ok ({root})");
    } else {
        let _ = writeln!(
            out,
            "archcheck: {} error(s), 0 warning(s) ({root})",
            findings.len()
        );
    }
    out
}

// serde_json is already the crate's one dependency; using it for JSON string
// escaping avoids a second, subtly different escaper in this file.
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_byte_stable_and_ordered() {
        let findings = vec![
            Finding::error("b/Cargo.toml", 3, "edge.upward", "b (L2) -> a (L3)"),
            Finding::error("a/README.md", 1, "readme.missing", "no README"),
        ];
        let first = json(".", &findings);
        let second = json(".", &findings);
        assert_eq!(first, second);
        assert!(first.starts_with(
            "{\"root\":\".\",\"ok\":false,\"errors\":2,\"warnings\":0,\"findings\":["
        ));
        let parsed: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        assert_eq!(parsed["findings"][0]["rule"], "edge.upward");
    }

    #[test]
    fn clean_human_output_says_ok() {
        assert_eq!(human(".", &[]), "archcheck: ok (.)\n");
    }
}
