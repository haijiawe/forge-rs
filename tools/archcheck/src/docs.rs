//! Checks that the documentation index `forge help` points at exists.
//!
//! The help text is the first documentation a user reads, and its last line
//! sends them to this tree's index. A path that only appears inside a Rust
//! string constant is checked by nothing: `cargo test` never opens the file, so
//! a deleted index leaves every gate green while `forge help` points at a
//! path that is not there. That is the exact defect this rule was written for.
//!
//! Why the help text and not a list of expected paths: any second copy of that
//! list — in this checker, or in `architecture.yaml` — is one more thing that
//! can drift from the sentence a user is shown. The rule therefore reads the
//! text the binary prints, and has no list to forget. `archcheck` does not
//! parse Rust: the usage text prints its references on the one line starting
//! with `docs:`, and that line is read as text, exactly as a reader would.
//!
//! The boundary is deliberate. If the usage text names no path, this rule has
//! nothing to judge and says nothing: the frozen CLI contract
//! (`docs/contracts/cli.md` §2.2) and `crates/cli/tests/cli.rs` pin that line
//! byte for byte, so a missing `docs:` line fails there, not here.

use std::fs;
use std::path::Path;

use crate::report::Finding;

/// Repository-relative. The usage text is a string constant in this file.
const CLI_SOURCE: &str = "crates/cli/src/lib.rs";
/// The marker the help text prints before its references.
const MARKER: &str = "docs:";

pub fn check(root: &Path) -> Vec<Finding> {
    let path = root.join(CLI_SOURCE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // A tree without the CLI crate is the crate-graph rules' business: an
        // absent forge-cli is `crate.missing`, which says more than this rule
        // could.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            return vec![Finding::error(
                CLI_SOURCE,
                1,
                "docs.unreadable",
                format!("cannot read the CLI source: {err}"),
            )]
        }
    };

    let mut findings = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let Some(rest) = raw.trim().strip_prefix(MARKER) else {
            continue;
        };
        for token in rest.split_whitespace() {
            // The line is prose as often as it is a bare path: strip the
            // punctuation a sentence puts around it before deciding what it is.
            let token = token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '`' | '"' | '\'' | ',' | '.' | ';' | ':' | '(' | ')' | '[' | ']'
                )
            });
            if !token.starts_with("docs/") {
                continue;
            }
            // A path the text promises is a file, not a directory: a directory
            // is what the index is *about*, and pointing a reader at it is the
            // same broken promise in a smaller way.
            if !root.join(token).is_file() {
                findings.push(Finding::error(
                    CLI_SOURCE,
                    line_no,
                    "docs.missing",
                    format!("the help text points at {token}, which is not a file on disk"),
                ));
            }
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str, usage: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("archcheck-docs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cli = root.join("crates/cli/src");
        fs::create_dir_all(&cli).expect("create fixture");
        fs::write(
            cli.join("lib.rs"),
            format!("pub const USAGE: &str = \"forge — an agent harness\n\n{usage}\";\n"),
        )
        .expect("write lib.rs");
        root
    }

    #[test]
    fn a_reference_to_a_missing_index_is_reported() {
        let root = fixture("missing", "docs: docs/README.md\n");
        let findings = check(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(findings
            .iter()
            .any(|f| f.rule == "docs.missing" && f.message.contains("docs/README.md")));
    }

    #[test]
    fn an_index_that_exists_passes() {
        let root = fixture("present", "docs: docs/README.md\n");
        fs::create_dir_all(root.join("docs")).expect("create docs");
        fs::write(root.join("docs/README.md"), "# docs\n").expect("write index");
        let findings = check(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn a_tree_without_the_cli_crate_is_not_this_rules_business() {
        let root =
            std::env::temp_dir().join(format!("archcheck-docs-absent-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture");
        let findings = check(&root);
        let _ = fs::remove_dir_all(&root);
        assert!(findings.is_empty(), "{findings:?}");
    }
}
