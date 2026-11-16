//! Byte-for-byte golden conformance against the Go scanner's `scan.golden`.
//!
//! The fixture tree is copied into `tests/fixtures` and never references the
//! Go source tree at test time. The working directory is changed to the
//! fixture root so the relative paths rendered by `Scan` match the golden.

use std::env;
use std::fs;
use std::path::PathBuf;

use forge_skill::{scan, Set};

fn quote(value: &str) -> String {
    format!("{value:?}")
}

fn render_set(set: &Set) -> String {
    let mut out = String::new();
    out.push_str(&format!("skills={}\n", set.skills.len()));
    for (index, skill) in set.skills.iter().enumerate() {
        out.push_str(&format!("skill {index}:\n"));
        out.push_str(&format!("  name:        {}\n", quote(&skill.name)));
        out.push_str(&format!("  description: {}\n", quote(&skill.description)));
        out.push_str(&format!("  dir:         {}\n", quote(&skill.dir)));
        out.push_str(&format!("  path:        {}\n", quote(&skill.path)));
        out.push_str("  body:        0 bytes (not loaded)\n");
        let files = skill
            .files
            .iter()
            .map(|file| quote(file))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("  files:       [{files}]\n"));
    }
    out.push_str(&format!("skipped={}\n", set.skipped.len()));
    for item in &set.skipped {
        out.push_str(&format!("  - {}\n", quote(item)));
    }
    out.push_str(&format!("warnings={}\n", set.warnings.len()));
    for item in &set.warnings {
        out.push_str(&format!("  - {}\n", quote(item)));
    }
    out
}

fn assert_clone_debug_eq<T: Clone + std::fmt::Debug + PartialEq>(_value: &T) {}

#[test]
fn scan_matches_go_golden_byte_for_byte() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let golden_path = manifest.join("tests/fixtures/testdata/scan.golden");
    let fixture_root = manifest.join("tests/fixtures");

    env::set_current_dir(&fixture_root).expect("enter fixture directory");
    let set = scan(&[PathBuf::from("testdata/a"), PathBuf::from("testdata/b")])
        .expect("scan must succeed");
    let expected = fs::read_to_string(&golden_path).expect("read scan.golden");
    let rendered = render_set(&set);
    assert_eq!(rendered, expected, "rendered Set differs from scan.golden");
    assert_clone_debug_eq(&set);
    assert_clone_debug_eq(&set.skills);
}
