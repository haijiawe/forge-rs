//! End-to-end tests build a synthetic workspace in the system temp directory
//! and run the real binary against it. A rule without a test that breaks it is
//! not a gate; each test below asserts both the finding and the exit code.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("archcheck-it-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fixture root");
        Fixture { root }
    }

    fn write_arch(&self, text: &str) {
        fs::write(self.root.join("architecture.yaml"), text).expect("write architecture");
    }

    fn write_crate(&self, dir: &str, package: &str, deps: &[&str]) {
        let crate_root = self.root.join("crates").join(dir);
        fs::create_dir_all(crate_root.join("src")).expect("create crate dir");
        let mut manifest =
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n\n[dependencies]\n");
        for dep in deps {
            manifest.push_str(&format!("{dep} = {{ path = \"../{dep}\" }}\n"));
        }
        fs::write(crate_root.join("Cargo.toml"), manifest).expect("write Cargo.toml");
        fs::write(crate_root.join("src/lib.rs"), "").expect("write lib.rs");
    }

    fn write_readme(&self, dir: &str, layer: &str) {
        fs::write(
            self.root.join("crates").join(dir).join("README.md"),
            readme(dir, layer),
        )
        .expect("write README");
    }

    fn remove_readme(&self, dir: &str) {
        fs::remove_file(self.root.join("crates").join(dir).join("README.md"))
            .expect("remove README");
    }

    /// The help text lives in the CLI crate; a fixture that wants the docs rule
    /// to have something to judge must write that file, exactly as the real
    /// tree has it.
    fn write_cli_usage(&self, docs_line: &str) {
        let src = self.root.join("crates/cli/src");
        fs::create_dir_all(&src).expect("create cli src");
        fs::write(
            src.join("lib.rs"),
            format!("pub const USAGE: &str = \"hearth — an agent harness\n\n{docs_line}\";\n"),
        )
        .expect("write cli lib.rs");
    }

    fn write_file(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, contents).expect("write fixture file");
    }

    fn run(&self, extra: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_archcheck"));
        command.arg("-root").arg(&self.root);
        command.args(extra);
        command.output().expect("run archcheck")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn readme(dir: &str, layer: &str) -> String {
    format!(
        "---\n\
         dir: crates/{dir}\n\
         layer: {layer}\n\
         role: {dir} crate\n\
         public: [Demo]\n\
         may_import: [hearth-core]\n\
         must_not: [hearth-server]\n\
         gate: make arch\n\
         ---\n\
         \n\
         ## 做什么\n\
         Does the {dir} job.\n\
         \n\
         ## 公开面\n\
         The public API.\n\
         \n\
         ## 依赖与边界\n\
         Only lower layers.\n\
         \n\
         ## 不要做\n\
         No upward imports.\n"
    )
}

fn two_layer_arch() -> &'static str {
    "version: 1\n\
     layers:\n\
     \x20 L0: [hearth-core]\n\
     \x20 L1: [hearth-exec]\n\
     ignore_dirs: [.git, target, node_modules]\n"
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("process exited with a code")
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("stdout is JSON")
}

fn clean_two_crates(name: &str) -> Fixture {
    let fixture = Fixture::new(name);
    fixture.write_arch(two_layer_arch());
    fixture.write_crate("core", "hearth-core", &[]);
    fixture.write_crate("exec", "hearth-exec", &["hearth-core"]);
    fixture.write_readme("core", "L0");
    fixture.write_readme("exec", "L1");
    fixture
}

#[test]
fn clean_tree_passes() {
    let fixture = clean_two_crates("clean");
    let output = fixture.run(&["-json"]);
    assert_eq!(
        code(&output),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    assert_eq!(report["ok"], true);
    assert_eq!(report["errors"], 0);
    assert_eq!(report["findings"].as_array().map(Vec::len), Some(0));
}

#[test]
fn upward_edge_is_caught_and_exits_one() {
    let fixture = Fixture::new("upward");
    fixture.write_arch(two_layer_arch());
    // L0 depending on L1 is the exact mistake the layer map exists to stop.
    fixture.write_crate("core", "hearth-core", &["hearth-exec"]);
    fixture.write_crate("exec", "hearth-exec", &[]);
    fixture.write_readme("core", "L0");
    fixture.write_readme("exec", "L1");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert_eq!(report["ok"], false);
    let findings = report["findings"].as_array().expect("findings array");
    let upward = findings
        .iter()
        .find(|finding| finding["rule"] == "edge.upward")
        .expect("upward finding");
    let message = upward["message"].as_str().unwrap_or("");
    assert!(message.contains("hearth-core (L0)"), "{message}");
    assert!(message.contains("hearth-exec (L1)"), "{message}");
}

#[test]
fn missing_readme_is_caught_and_exits_one() {
    let fixture = clean_two_crates("missing-readme");
    fixture.remove_readme("exec");
    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "readme.missing"));
}

#[test]
fn missing_readme_section_is_caught_and_exits_one() {
    let fixture = clean_two_crates("missing-section");
    let path = fixture.root.join("crates/core/README.md");
    let text = fs::read_to_string(&path).expect("read README");
    let without = text.replace("## 不要做\nNo upward imports.\n", "");
    fs::write(&path, without).expect("write README");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    let section = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["rule"] == "readme.section")
        .expect("section finding");
    assert!(section["message"].as_str().unwrap_or("").contains("不要做"));
}

#[test]
fn same_layer_forbidden_edge_is_caught() {
    let fixture = Fixture::new("forbidden");
    fixture.write_arch(
        "version: 1\n\
         layers:\n\
         \x20 L0: [hearth-core, hearth-config]\n\
         forbidden_edges:\n\
         \x20 - [hearth-core, hearth-config]\n\
         ignore_dirs: [target]\n",
    );
    fixture.write_crate("core", "hearth-core", &["hearth-config"]);
    fixture.write_crate("config", "hearth-config", &[]);
    fixture.write_readme("core", "L0");
    fixture.write_readme("config", "L0");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "edge.forbidden"));
}

#[test]
fn json_report_is_byte_stable() {
    let fixture = Fixture::new("byte-stable");
    fixture.write_arch(two_layer_arch());
    fixture.write_crate("core", "hearth-core", &["hearth-exec"]);
    fixture.write_crate("exec", "hearth-exec", &[]);
    fixture.write_readme("core", "L0");
    fixture.write_readme("exec", "L1");

    let first = fixture.run(&["-json"]);
    let second = fixture.run(&["-json"]);
    assert_eq!(first.stdout, second.stdout);
    assert!(String::from_utf8_lossy(&first.stdout).starts_with("{\"root\":"));
}

#[test]
fn a_root_without_architecture_is_a_checker_error() {
    let fixture = Fixture::new("no-arch");
    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("architecture.yaml"));
}

#[test]
fn readme_without_frontmatter_is_caught_and_exits_one() {
    let fixture = clean_two_crates("no-frontmatter");
    fs::write(fixture.root.join("crates/core/README.md"), "just prose\n").expect("write README");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "readme.frontmatter"));
}

#[test]
fn readme_missing_key_is_caught_and_exits_one() {
    let fixture = clean_two_crates("missing-key");
    let path = fixture.root.join("crates/core/README.md");
    let text = fs::read_to_string(&path).expect("read README");
    fs::write(&path, text.replace("role: core crate\n", "")).expect("write README");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "readme.key"));
}

#[test]
fn readme_list_key_shape_is_caught_and_exits_one() {
    let fixture = clean_two_crates("bad-list");
    let path = fixture.root.join("crates/core/README.md");
    let text = fs::read_to_string(&path).expect("read README");
    fs::write(&path, text.replace("public: [Demo]", "public: true")).expect("write README");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "readme.key"));
}

#[test]
fn readme_dir_mismatch_is_caught_and_exits_one() {
    let fixture = clean_two_crates("bad-dir");
    let path = fixture.root.join("crates/core/README.md");
    let text = fs::read_to_string(&path).expect("read README");
    fs::write(
        &path,
        text.replace("dir: crates/core", "dir: crates/elsewhere"),
    )
    .expect("write README");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "readme.dir"));
}

#[test]
fn readme_layer_mismatch_is_caught_and_exits_one() {
    let fixture = clean_two_crates("bad-layer");
    let path = fixture.root.join("crates/core/README.md");
    let text = fs::read_to_string(&path).expect("read README");
    fs::write(&path, text.replace("layer: L0", "layer: L1")).expect("write README");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "readme.layer"));
}

#[test]
fn duplicate_package_name_is_caught_and_exits_one() {
    let fixture = Fixture::new("duplicate");
    fixture.write_arch(
        "version: 1\n\
         layers:\n\
         \x20 L0: [hearth-core]\n\
         ignore_dirs: [target]\n",
    );
    fixture.write_crate("core", "hearth-core", &[]);
    fixture.write_crate("other", "hearth-core", &[]);
    fixture.write_readme("core", "L0");
    fixture.write_readme("other", "L0");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "crate.duplicate"));
}

#[test]
fn undeclared_crate_is_caught_and_exits_one() {
    let fixture = Fixture::new("undeclared");
    fixture.write_arch(
        "version: 1\n\
         layers:\n\
         \x20 L0: [hearth-core]\n\
         ignore_dirs: [target]\n",
    );
    fixture.write_crate("core", "hearth-core", &[]);
    fixture.write_crate("rogue", "hearth-rogue", &[]);
    fixture.write_readme("core", "L0");
    fixture.write_readme("rogue", "L0");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "crate.undeclared"));
}

#[test]
fn declared_crate_with_no_directory_is_caught_and_exits_one() {
    let fixture = Fixture::new("missing-crate");
    fixture.write_arch(two_layer_arch());
    fixture.write_crate("core", "hearth-core", &[]);
    fixture.write_readme("core", "L0");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "crate.missing"));
}

#[test]
fn edge_to_undeclared_hearth_crate_is_caught_and_exits_one() {
    let fixture = clean_two_crates("unknown-edge");
    let path = fixture.root.join("crates/core/Cargo.toml");
    let mut text = fs::read_to_string(&path).expect("read Cargo.toml");
    text.push_str("hearth-ghost = { path = \"../ghost\" }\n");
    fs::write(&path, text).expect("write Cargo.toml");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "edge.unknown"));
}

#[test]
fn unreadable_readme_is_caught_and_exits_one() {
    let fixture = clean_two_crates("unreadable-readme");
    let path = fixture.root.join("crates/core/README.md");
    fs::remove_file(&path).expect("remove README");
    fs::create_dir(&path).expect("replace README with a directory");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "readme.unreadable"));
}

#[test]
fn a_docs_path_the_help_text_promises_but_the_tree_lacks_is_caught_and_exits_one() {
    // The defect: `hearth help` ends with `docs: docs/README.md`, and nothing
    // opened that file, so the index could be absent with every gate green.
    let fixture = clean_two_crates("missing-docs");
    fixture.write_cli_usage("docs: docs/README.md\n");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    let missing = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["rule"] == "docs.missing")
        .expect("docs.missing finding");
    let message = missing["message"].as_str().unwrap_or("");
    assert!(message.contains("docs/README.md"), "{message}");
}

#[test]
fn a_docs_path_the_help_text_promises_and_the_tree_has_passes() {
    let fixture = clean_two_crates("present-docs");
    fixture.write_cli_usage("docs: docs/README.md\n");
    fixture.write_file("docs/README.md", "# docs\n");

    let output = fixture.run(&["-json"]);
    assert_eq!(
        code(&output),
        0,
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    assert_eq!(report["ok"], true);
    assert_eq!(report["findings"].as_array().map(Vec::len), Some(0));
}

#[test]
fn a_docs_directory_is_not_a_docs_file() {
    // A path in the help text is a promise about a file a reader can open;
    // pointing at the directory instead is the same promise, kept wrongly.
    let fixture = clean_two_crates("docs-dir");
    fixture.write_cli_usage("docs: docs/README.md\n");
    fs::create_dir_all(fixture.root.join("docs/README.md")).expect("create docs/README.md dir");

    let output = fixture.run(&["-json"]);
    assert_eq!(code(&output), 1);
    let report = json(&output);
    assert!(report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .any(|finding| finding["rule"] == "docs.missing"));
}
