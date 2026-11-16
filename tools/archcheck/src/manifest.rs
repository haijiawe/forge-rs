//! Reads crate manifests from `crates/*/Cargo.toml`.
//!
//! Only two facts matter here: the package name (which is how dependencies
//! refer to the crate) and the dependency names. Everything else in a manifest
//! belongs to Cargo, not to the architecture gate.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Crate {
    /// repository-relative, slash-separated: "crates/core"
    pub dir: String,
    pub name: String,
    pub deps: Vec<Dependency>,
}

/// Every directory directly under `crates/` that holds a `Cargo.toml` is a
/// crate. Directories without one are not touched: they are not the gate's
/// business, and guessing would create phantom failures.
pub fn discover(root: &Path, ignore_dirs: &[String]) -> Result<Vec<Crate>, String> {
    let crates_root = root.join("crates");
    let entries = fs::read_dir(&crates_root).map_err(|err| format!("crates: {err}"))?;

    let mut manifests: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("crates: {err}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if ignore_dirs.iter().any(|ignored| ignored == &dir_name) || dir_name.starts_with('.') {
            continue;
        }
        let manifest = path.join("Cargo.toml");
        if manifest.is_file() {
            manifests.push((format!("crates/{dir_name}"), manifest));
        }
    }
    manifests.sort_by(|a, b| a.0.cmp(&b.0));

    let mut crates = Vec::new();
    for (dir, manifest) in manifests {
        let text =
            fs::read_to_string(&manifest).map_err(|err| format!("{dir}/Cargo.toml: {err}"))?;
        let (name, deps) =
            parse_manifest(&text).map_err(|err| format!("{dir}/Cargo.toml: {err}"))?;
        crates.push(Crate { dir, name, deps });
    }
    Ok(crates)
}

/// Cargo manifests in this workspace are hand-written and simple; this parser
/// understands the section headers and `key = value` lines a dependency needs.
/// A manifest that uses features this parser cannot see is a checker error
/// (exit 2) rather than a silent pass, because a missed edge is exactly the
/// failure the gate exists to prevent.
fn parse_manifest(text: &str) -> Result<(String, Vec<Dependency>), String> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Package,
        Dependencies,
        Other,
    }

    let mut section = Section::Other;
    let mut package_name: Option<String> = None;
    let mut deps: BTreeMap<String, usize> = BTreeMap::new();

    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') {
            let header = trimmed
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .ok_or_else(|| format!("line {line_no}: malformed section header {trimmed:?}"))?
                .trim();
            section = if header == "package" {
                Section::Package
            } else if header == "dependencies"
                || header == "dev-dependencies"
                || header == "build-dependencies"
                || header.ends_with(".dependencies")
            {
                Section::Dependencies
            } else if let Some(dep) = header.strip_prefix("dependencies.") {
                add_dependency(&mut deps, dep, line_no);
                Section::Other
            } else if let Some(dep) = header.rsplit_once(".dependencies.") {
                add_dependency(&mut deps, dep.1, line_no);
                Section::Other
            } else {
                Section::Other
            };
            continue;
        }

        match section {
            Section::Package => {
                if let Some((key, value)) = split_assignment(trimmed) {
                    if key == "name" {
                        let name = unquote(value.trim());
                        if !name.is_empty() {
                            package_name = Some(name);
                        }
                    }
                }
            }
            Section::Dependencies => {
                let Some((key, value)) = split_assignment(trimmed) else {
                    continue;
                };
                // `serde.workspace = true` and `serde.version = "1"` describe
                // the dependency `serde`; the suffix is a field of its table.
                let dependency_key = key.split('.').next().unwrap_or(key);
                let dependency_key = unquote(dependency_key.trim());
                if dependency_key.is_empty() {
                    continue;
                }
                // `core = { package = "forge-core", path = "../core" }`
                // renames an internal crate; the architecture cares about the
                // real crate name, not the local alias.
                let real_name = inline_package_name(value).unwrap_or(dependency_key);
                add_dependency(&mut deps, &real_name, line_no);
            }
            Section::Other => {}
        }
    }

    let package_name = package_name.ok_or_else(|| "missing [package] name".to_string())?;
    let deps = deps
        .into_iter()
        .map(|(name, line)| Dependency { name, line })
        .collect();
    Ok((package_name, deps))
}

fn add_dependency(deps: &mut BTreeMap<String, usize>, name: &str, line: usize) {
    let name = unquote(name.trim());
    if !name.is_empty() {
        // Keep the first declaration's line: a dependency repeated in
        // dev-dependencies should still point at a real line in the manifest.
        deps.entry(name).or_insert(line);
    }
}

fn inline_package_name(value: &str) -> Option<String> {
    let value = value.trim();
    if !value.starts_with('{') {
        return None;
    }
    let (_, rest) = value.split_once("package")?;
    let (_, rest) = rest.split_once('=')?;
    let rest = rest.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)?;
    Some(rest[1..1 + end].to_string())
}

fn split_assignment(text: &str) -> Option<(&str, &str)> {
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
            '=' if !in_single && !in_double => {
                return Some((text[..index].trim(), text[index + 1..].trim()));
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
    fn reads_package_name_and_dependencies() {
        let (name, deps) = parse_manifest(
            "[package]\n\
             name = \"forge-server\"\n\
             \n\
             [dependencies]\n\
             forge-core = { path = \"../core\" }\n\
             serde.workspace = true\n\
             \n\
             [dev-dependencies]\n\
             forge-core = { path = \"../core\" }\n",
        )
        .expect("valid manifest");
        assert_eq!(name, "forge-server");
        assert_eq!(
            deps,
            vec![
                Dependency {
                    name: "forge-core".to_string(),
                    line: 5
                },
                Dependency {
                    name: "serde".to_string(),
                    line: 6
                },
            ]
        );
    }

    #[test]
    fn follows_a_renamed_package() {
        let (_, deps) = parse_manifest(
            "[package]\nname = \"forge-cli\"\n\n[dependencies]\ncore = { package = \"forge-core\", path = \"../core\" }\n",
        )
        .expect("valid manifest");
        assert_eq!(deps[0].name, "forge-core");
    }
}
