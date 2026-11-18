//! Applies the judgements Cargo cannot make:
//!   1. every dependency edge points to a lower or equal layer;
//!   2. every crate has a README that states the boundary the edge crosses;
//!   3. every path the help text promises exists (see `docs`).

use std::collections::BTreeSet;
use std::path::Path;

use crate::arch::Architecture;
use crate::docs;
use crate::manifest::Crate;
use crate::readme;
use crate::report::Finding;

pub fn run(root: &Path, architecture: &Architecture, crates: &[Crate]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut seen_names: BTreeSet<&str> = BTreeSet::new();

    for krate in crates {
        if !seen_names.insert(krate.name.as_str()) {
            findings.push(Finding::error(
                format!("{}/Cargo.toml", krate.dir),
                1,
                "crate.duplicate",
                format!(
                    "package name {:?} is used by more than one crate",
                    krate.name
                ),
            ));
        }

        let placement = architecture.crates.get(&krate.name);
        if placement.is_none() {
            findings.push(Finding::error(
                format!("{}/Cargo.toml", krate.dir),
                1,
                "crate.undeclared",
                format!(
                    "crate {:?} is not declared in architecture.yaml; add it to a layer or it is outside the gate",
                    krate.name
                ),
            ));
        }

        let source_rank = architecture.rank_of(&krate.name);
        let source_layer = placement.map(|placement| placement.layer.as_str());
        for dependency in &krate.deps {
            let Some(target) = architecture.crates.get(&dependency.name) else {
                // Only internal-looking names are the gate's business. An
                // external crate called e.g. `serde_json` is not in the map
                // and must not be reported as an unknown layer.
                if dependency.name.starts_with("forge-") {
                    findings.push(Finding::error(
                        format!("{}/Cargo.toml", krate.dir),
                        dependency.line,
                        "edge.unknown",
                        format!(
                            "depends on {:?}, which architecture.yaml does not declare",
                            dependency.name
                        ),
                    ));
                }
                continue;
            };

            let target_rank = architecture.rank_of(&dependency.name);
            if let (Some(source_rank), Some(target_rank)) = (source_rank, target_rank) {
                if target_rank > source_rank {
                    findings.push(Finding::error(
                        format!("{}/Cargo.toml", krate.dir),
                        dependency.line,
                        "edge.upward",
                        format!(
                            "{} ({}) depends on {} ({}): an edge may point only to a lower or equal layer",
                            krate.name,
                            source_layer.unwrap_or("?"),
                            dependency.name,
                            target.layer
                        ),
                    ));
                }
            }

            if architecture.is_forbidden(&krate.name, &dependency.name) {
                findings.push(Finding::error(
                    format!("{}/Cargo.toml", krate.dir),
                    dependency.line,
                    "edge.forbidden",
                    format!(
                        "{} ({}) depends on {} ({}): architecture.yaml forbids this edge",
                        krate.name,
                        source_layer.unwrap_or("?"),
                        dependency.name,
                        target.layer
                    ),
                ));
            }
        }

        findings.extend(readme::check(root, &krate.dir, source_layer));
    }

    // A crate declared in architecture.yaml but absent from crates/ is a hole
    // in the gate: the layer map promises a boundary that does not exist yet.
    for (name, placement) in &architecture.crates {
        if !crates.iter().any(|krate| &krate.name == name) {
            findings.push(Finding::error(
                "architecture.yaml",
                placement.line,
                "crate.missing",
                format!(
                    "crate {name} is declared in {} but crates/ has no Cargo.toml for it",
                    placement.layer
                ),
            ));
        }
    }

    // The help text sends a user to the docs index; a path it names and the
    // tree does not have is the same broken promise as a README that does not
    // describe its crate.
    findings.extend(docs::check(root));

    findings
}
