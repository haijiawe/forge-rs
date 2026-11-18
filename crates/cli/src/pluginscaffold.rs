//! Writes a plugin directory that this harness can start without edits.
//!
//! This module owns bytes, not flags. `plugincmd` owns the command vocabulary
//! (`--runtime`, `--force`, `--dir`) and the exit codes; everything here takes
//! already-decided values and returns a reason when it refuses. The split is
//! deliberate: an earlier draft of the scaffold carried its own `parse`,
//! `positional`, `root_of` and `USAGE`, which meant two flag tables for one
//! command — `new` did not accept `--json` while `start` did not accept
//! `--runtime`, and nothing would have noticed when they drifted further apart.
//!
//! The templates are `include_str!`ed rather than read at runtime, for the same
//! reason the web UI is embedded: one binary, no asset directory to lose. The
//! cost is that editing a template needs a rebuild, which is right for a file
//! nobody edits between releases.
//!
//! Three rules from `crates/plugin/PROTOCOL.md` are written into the templates
//! rather than left to the reader, because each one fails in a way that looks
//! like a broken harness rather than a broken plugin:
//!
//! 1. stdout carries protocol frames only — a stray `console.log` is a bad frame;
//! 2. every reply echoes the request `id` verbatim;
//! 3. a tool that ran and failed answers `isError: true`, which is a *result*;
//!    a JSON-RPC error means the call was never attempted.

use std::path::{Path, PathBuf};

const NODE_TEMPLATE: &str = include_str!("../templates/plugin/node.js.tmpl");
const PYTHON_TEMPLATE: &str = include_str!("../templates/plugin/python.py.tmpl");
const README_TEMPLATE: &str = include_str!("../templates/plugin/README.md.tmpl");

/// Which runtime the entry file and template come from.
pub struct Kind {
    pub runtime: &'static str,
    pub entry: &'static str,
    body: &'static str,
}

/// Why the scaffold declined. The caller maps these to exit codes; this module
/// does not know what an exit code is.
#[derive(Debug)]
pub enum Reject {
    /// The request cannot be honoured as asked (bad name, unknown runtime).
    Usage(String),
    /// The request was fine and the filesystem said no.
    Failed(String),
}

/// What was written, for the command to report.
pub struct Outcome {
    pub directory: PathBuf,
    pub runtime: &'static str,
    /// File names relative to `directory`, sorted for the operator.
    pub files: Vec<String>,
}

/// Resolve `--runtime`. An empty value means the default, which is `node`.
///
/// `exec` is accepted by the manifest format but has no template on purpose: an
/// `exec` plugin is a program the author already has, and emitting a stub with a
/// shebang for an interpreter nobody installed would write a manifest that
/// cannot start. Refusing is more useful than generating something broken.
pub fn kind_of(requested: &str) -> Result<Kind, Reject> {
    match requested.trim() {
        "" | "node" => Ok(Kind {
            runtime: "node",
            entry: "index.js",
            body: NODE_TEMPLATE,
        }),
        "python" => Ok(Kind {
            runtime: "python",
            entry: "main.py",
            body: PYTHON_TEMPLATE,
        }),
        "exec" => Err(Reject::Usage(
            "--runtime exec has no template; write the program yourself and set \
             \"runtime\":\"exec\" in plugin.json (see crates/plugin/PROTOCOL.md §1)"
                .to_string(),
        )),
        other => Err(Reject::Usage(format!(
            "unknown runtime {other:?} (want node, python or exec)"
        ))),
    }
}

/// Write `<root>/<name>/`.
///
/// Validation happens before anything is created: a refused name must leave no
/// directory behind, or the next attempt has to reckon with a half-made plugin.
pub fn generate(
    root: &Path,
    name: &str,
    kind: &Kind,
    description: &str,
    force: bool,
) -> Result<Outcome, Reject> {
    if !forge_plugin::is_valid_name(name) {
        return Err(Reject::Usage(format!(
            "{name:?} is not a legal plugin name; the name becomes a directory and a tool \
             prefix, so it must match ^[a-z0-9][a-z0-9._-]{{0,63}}$"
        )));
    }
    let directory = root.join(name);
    if directory.exists() {
        if !force {
            return Err(Reject::Failed(format!(
                "{} already exists; pass --force to replace it",
                directory.display()
            )));
        }
        std::fs::remove_dir_all(&directory).map_err(|err| {
            Reject::Failed(format!("cannot replace {}: {err}", directory.display()))
        })?;
    }
    std::fs::create_dir_all(&directory)
        .map_err(|err| Reject::Failed(format!("cannot create {}: {err}", directory.display())))?;

    let description = if description.trim().is_empty() {
        format!("a {name} plugin")
    } else {
        crate::one_line(description.trim())
    };
    let manifest = serde_json::json!({
        "name": name,
        "version": "0.1.0",
        "description": description,
        "runtime": kind.runtime,
        "entry": kind.entry,
    });
    let manifest = serde_json::to_string_pretty(&manifest)
        .map_err(|err| Reject::Failed(format!("cannot build the manifest: {err}")))?;

    let substitutions = [
        ("__NAME__", name),
        ("__TITLE__", name),
        ("__DESCRIPTION__", description.as_str()),
        ("__RUNTIME__", kind.runtime),
        ("__ENTRY__", kind.entry),
    ];
    let files = [
        ("plugin.json", manifest),
        (kind.entry, fill(kind.body, &substitutions)),
        ("README.md", fill(README_TEMPLATE, &substitutions)),
    ];
    for (file_name, body) in &files {
        let path = directory.join(file_name);
        std::fs::write(&path, body)
            .map_err(|err| Reject::Failed(format!("cannot write {}: {err}", path.display())))?;
    }

    let mut names: Vec<String> = files.iter().map(|(name, _)| (*name).to_string()).collect();
    names.sort();
    Ok(Outcome {
        directory,
        runtime: kind.runtime,
        files: names,
    })
}

/// Replace the `__TOKEN__` placeholders in one pass, so a value that happens to
/// look like a token is not substituted a second time.
fn fill(template: &str, substitutions: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    'outer: while !rest.is_empty() {
        for (token, value) in substitutions {
            if let Some(after) = rest.strip_prefix(token) {
                out.push_str(value);
                rest = after;
                continue 'outer;
            }
        }
        let mut chars = rest.chars();
        match chars.next() {
            Some(ch) => {
                out.push(ch);
                rest = chars.as_str();
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("forge-scaffold-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn node() -> Kind {
        kind_of("node").expect("node has a template")
    }

    #[test]
    fn the_manifest_the_scaffold_writes_is_one_the_host_accepts() {
        // The claim worth proving: the output is not merely well-shaped JSON, it
        // is a manifest `Host::discover` reads without a complaint.
        let root = scratch("discover");
        let outcome = generate(&root, "good", &node(), "", false).expect("generate");
        let host = forge_plugin::Host::start(&root);
        let (found, broken) = host.discover();
        assert!(
            broken.is_empty(),
            "the scaffold wrote a broken manifest: {broken:?}"
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "good");
        assert_eq!(outcome.files, vec!["README.md", "index.js", "plugin.json"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_placeholder_survives_in_any_generated_file() {
        let root = scratch("fill");
        generate(&root, "myslug", &node(), "", false).expect("generate");
        for name in ["index.js", "README.md", "plugin.json"] {
            let body = std::fs::read_to_string(root.join("myslug").join(name)).expect("read");
            for token in [
                "__NAME__",
                "__TITLE__",
                "__DESCRIPTION__",
                "__RUNTIME__",
                "__ENTRY__",
            ] {
                assert!(!body.contains(token), "{name} still contains {token}");
            }
        }
        let entry = std::fs::read_to_string(root.join("myslug/index.js")).expect("entry");
        assert!(entry.contains("name: 'myslug'"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_python_plugin_gets_a_python_entry_and_a_discoverable_manifest() {
        let root = scratch("python");
        let kind = kind_of("python").expect("python has a template");
        let outcome = generate(&root, "pyslug", &kind, "", false).expect("generate");
        assert_eq!(outcome.runtime, "python");
        assert!(outcome.files.contains(&"main.py".to_string()));
        let host = forge_plugin::Host::start(&root);
        let (found, broken) = host.discover();
        assert!(broken.is_empty(), "{broken:?}");
        assert_eq!(found[0].runtime.as_str(), "python");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_illegal_name_is_refused_before_anything_is_created() {
        let root = scratch("illegal");
        for bad in ["Hello", "-x", "with space", "../escape", ""] {
            let outcome = generate(&root, bad, &node(), "", false);
            assert!(
                matches!(outcome, Err(Reject::Usage(_))),
                "{bad:?} was not a usage rejection"
            );
        }
        assert!(!root.exists(), "a refused name created {}", root.display());
    }

    #[test]
    fn exec_and_an_unknown_runtime_are_usage_rejections_that_say_why() {
        match kind_of("exec") {
            Err(Reject::Usage(message)) => assert!(message.contains("has no template")),
            _ => panic!("exec must be refused with a reason"),
        }
        match kind_of("ruby") {
            Err(Reject::Usage(message)) => {
                assert!(message.contains("want node, python or exec"), "{message}")
            }
            _ => panic!("an unknown runtime must be refused"),
        }
        assert_eq!(kind_of("").expect("empty means node").runtime, "node");
        assert_eq!(kind_of("  node  ").expect("trimmed").runtime, "node");
    }

    #[test]
    fn an_existing_directory_is_refused_without_force_and_survives_the_refusal() {
        let root = scratch("exists");
        generate(&root, "twice", &node(), "", false).expect("first");
        let second = generate(&root, "twice", &node(), "", false);
        assert!(matches!(second, Err(Reject::Failed(_))));
        assert!(root.join("twice/index.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn force_replaces_the_whole_directory() {
        let root = scratch("force");
        generate(&root, "again", &node(), "", false).expect("first");
        std::fs::write(root.join("again/leftover.txt"), "should be gone").expect("write");
        generate(&root, "again", &node(), "", true).expect("forced");
        assert!(!root.join("again/leftover.txt").exists());
        assert!(root.join("again/index.js").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_description_reaches_the_manifest_on_one_line() {
        // `one_line` flattens newlines and tabs and trims the ends; it does not
        // collapse runs of spaces, because it is the same helper `list` uses to
        // print an operator's own words in a table row. The property that matters
        // here is that no line break survives into a single-line field.
        let root = scratch("description");
        generate(
            &root,
            "described",
            &node(),
            "  greets\npeople\ttwice  ",
            false,
        )
        .expect("generate");
        let manifest =
            std::fs::read_to_string(root.join("described/plugin.json")).expect("manifest");
        let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("json");
        let description = parsed["description"].as_str().expect("a string");
        assert_eq!(description, "greets people twice");
        assert!(!description.contains('\n') && !description.contains('\t'));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_description_becomes_a_named_default_not_an_empty_string() {
        let root = scratch("default-description");
        generate(&root, "plain", &node(), "   ", false).expect("generate");
        let manifest = std::fs::read_to_string(root.join("plain/plugin.json")).expect("manifest");
        let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("json");
        assert_eq!(parsed["description"], "a plain plugin");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fill_substitutes_once_and_leaves_unknown_tokens_alone() {
        let out = fill("__NAME__ and __OTHER__ and __NAME__", &[("__NAME__", "x")]);
        assert_eq!(out, "x and __OTHER__ and x");
        // A value that looks like a token must not be expanded again.
        let out = fill("__NAME__", &[("__NAME__", "__OTHER__"), ("__OTHER__", "y")]);
        assert_eq!(out, "__OTHER__");
    }
}
