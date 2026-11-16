//! Validates a crate README against the directory standard.
//!
//! Why a README is a gate and not documentation: a crate boundary that is only
//! in someone's head disappears the moment an import is added. The frontmatter
//! is the machine-readable half; the four sections are the half a reviewer (or
//! an agent about to edit the crate) reads before deciding an import is safe.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::report::Finding;

const REQUIRED_KEYS: [&str; 7] = [
    "dir",
    "layer",
    "role",
    "public",
    "may_import",
    "must_not",
    "gate",
];
const REQUIRED_SECTIONS: [&str; 4] = ["做什么", "公开面", "依赖与边界", "不要做"];

pub fn check(root: &Path, crate_dir: &str, expected_layer: Option<&str>) -> Vec<Finding> {
    let rel = format!("{crate_dir}/README.md");
    let path = root.join(crate_dir).join("README.md");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return vec![Finding::error(
                rel,
                1,
                "readme.missing",
                "crate has no README.md; every crate must state its boundary",
            )];
        }
        Err(err) => {
            return vec![Finding::error(
                rel,
                1,
                "readme.unreadable",
                format!("cannot read README.md: {err}"),
            )];
        }
    };

    let mut findings = Vec::new();
    let mut frontmatter: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut closing = None;

    let first_line = text
        .lines()
        .next()
        .unwrap_or("")
        .trim_start_matches('\u{feff}');
    if first_line.trim_end() != "---" {
        findings.push(Finding::error(
            &rel,
            1,
            "readme.frontmatter",
            "README must start with YAML frontmatter delimited by --- on line 1",
        ));
        return findings;
    }

    let mut frontmatter_lines = text.lines().enumerate().skip(1).peekable();
    while let Some((index, raw)) = frontmatter_lines.next() {
        let line_no = index + 1;
        let line = raw.trim();
        if line == "---" {
            closing = Some(line_no);
            break;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            findings.push(Finding::error(
                &rel,
                line_no,
                "readme.frontmatter",
                format!("frontmatter line is not \"key: value\": {line:?}"),
            ));
            continue;
        };
        let key = key.trim().to_string();
        let mut value = value.trim().to_string();
        if key.is_empty() {
            findings.push(Finding::error(
                &rel,
                line_no,
                "readme.frontmatter",
                "frontmatter key is empty",
            ));
            continue;
        }
        // YAML block sequences are the other natural way to write the three
        // list-valued keys. Accept them instead of forcing one house style on
        // README authors.
        if value.is_empty() {
            let mut items = Vec::new();
            while let Some((_, next_raw)) = frontmatter_lines.peek() {
                let next = next_raw.trim();
                let Some(item) = next.strip_prefix("- ") else {
                    break;
                };
                items.push(item.trim().to_string());
                frontmatter_lines.next();
            }
            if !items.is_empty() {
                value = format!("[{}]", items.join(", "));
            }
        }
        if frontmatter.contains_key(&key) {
            findings.push(Finding::error(
                &rel,
                line_no,
                "readme.key",
                format!("frontmatter key {key:?} appears more than once"),
            ));
            continue;
        }
        frontmatter.insert(key, (value, line_no));
    }

    let Some(closing_line) = closing else {
        findings.push(Finding::error(
            &rel,
            1,
            "readme.frontmatter",
            "frontmatter is not closed by a --- line",
        ));
        return findings;
    };

    for key in REQUIRED_KEYS {
        match frontmatter.get(key) {
            None => findings.push(Finding::error(
                &rel,
                1,
                "readme.key",
                format!("frontmatter is missing required key {key:?}"),
            )),
            Some((value, line_no)) => check_shape(key, value, *line_no, &rel, &mut findings),
        }
    }

    if let Some((value, line_no)) = frontmatter.get("dir") {
        let actual = unquote(value).replace('\\', "/");
        if actual != crate_dir {
            findings.push(Finding::error(
                &rel,
                *line_no,
                "readme.dir",
                format!("frontmatter dir is {actual:?}, expected {crate_dir:?}"),
            ));
        }
    }
    if let Some(expected) = expected_layer {
        if let Some((value, line_no)) = frontmatter.get("layer") {
            let actual = unquote(value);
            if actual != expected {
                findings.push(Finding::error(
                    &rel,
                    *line_no,
                    "readme.layer",
                    format!("frontmatter layer is {actual:?}, architecture.yaml says {expected:?}"),
                ));
            }
        }
    }

    // A section exists only when its `## title` appears after the frontmatter
    // and has at least one non-blank, non-heading line. A bare heading is the
    // documentation equivalent of a stub — it should not satisfy a gate.
    let mut seen = [false; 4];
    let mut has_content = [false; 4];
    let mut current: Option<usize> = None;
    for raw in text.lines().skip(closing_line) {
        let trimmed = raw.trim();
        let heading_level = trimmed.chars().take_while(|ch| *ch == '#').count();
        let is_heading = heading_level > 0 && trimmed.chars().nth(heading_level) == Some(' ');
        if is_heading && heading_level == 2 {
            let title = trimmed[2..].trim().trim_end_matches(['：', ':', '，', ',']);
            current = REQUIRED_SECTIONS
                .iter()
                .position(|section| *section == title);
            if let Some(index) = current {
                seen[index] = true;
            }
        } else if is_heading && heading_level < 2 {
            current = None;
        } else if let Some(index) = current {
            // A deeper heading does not end the section; its prose does count,
            // but a bare heading alone still must not satisfy the gate.
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                has_content[index] = true;
            }
        }
    }

    for (index, section) in REQUIRED_SECTIONS.iter().enumerate() {
        if !seen[index] {
            findings.push(Finding::error(
                &rel,
                1,
                "readme.section",
                format!("README is missing section \"## {section}\""),
            ));
        } else if !has_content[index] {
            findings.push(Finding::error(
                &rel,
                1,
                "readme.section",
                format!("section \"## {section}\" is empty"),
            ));
        }
    }

    findings
}

fn check_shape(key: &str, value: &str, line: usize, file: &str, findings: &mut Vec<Finding>) {
    let value = value.trim();
    match key {
        "public" | "may_import" | "must_not" => {
            if !(value.starts_with('[') && value.ends_with(']')) {
                findings.push(Finding::error(
                    file,
                    line,
                    "readme.key",
                    format!("{key} must be a list like [forge-core], got {value:?}"),
                ));
            }
        }
        _ => {
            if value.is_empty() {
                findings.push(Finding::error(
                    file,
                    line,
                    "readme.key",
                    format!("{key} must not be empty"),
                ));
            }
        }
    }
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
    use std::path::PathBuf;

    fn write_fixture(name: &str, body: &str) -> (PathBuf, String) {
        let root =
            std::env::temp_dir().join(format!("archcheck-readme-{name}-{}", std::process::id()));
        let crate_dir = root.join("crates/demo");
        fs::create_dir_all(&crate_dir).expect("create fixture");
        fs::write(crate_dir.join("README.md"), body).expect("write README");
        (root, "crates/demo".to_string())
    }

    fn valid_readme() -> &'static str {
        "---\n\
         dir: crates/demo\n\
         layer: L1\n\
         role: demo\n\
         public: [Demo]\n\
         may_import: [forge-core]\n\
         must_not: [forge-server]\n\
         gate: make arch\n\
         ---\n\
         \n\
         ## 做什么\n\
         demo things.\n\
         \n\
         ## 公开面\n\
         Demo type.\n\
         \n\
         ## 依赖与边界\n\
         forge-core only.\n\
         \n\
         ## 不要做\n\
         no upward imports.\n"
    }

    #[test]
    fn a_complete_readme_passes() {
        let (root, dir) = write_fixture("ok", valid_readme());
        let findings = check(&root, &dir, Some("L1"));
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn a_missing_section_is_reported() {
        let without = valid_readme().replace("## 不要做\nno upward imports.\n", "");
        let (root, dir) = write_fixture("section", &without);
        let findings = check(&root, &dir, Some("L1"));
        assert!(findings
            .iter()
            .any(|f| f.rule == "readme.section" && f.message.contains("不要做")));
    }
}
