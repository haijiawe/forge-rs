//! `archcheck` — the crate-graph gate described in docs/08-rust-port.md §2.
//!
//! Cargo.toml already says which crates depend on which, so this tool does not
//! parse Rust source. It supplies the two judgements Cargo cannot make: an edge
//! may point only down a layer, and every crate must describe its boundary in a
//! README that a reviewer (or an agent) can read before touching the code.

mod arch;
mod check;
mod docs;
mod manifest;
mod readme;
mod report;

use std::fs;
use std::path::{Path, PathBuf};

const USAGE: &str = "\
archcheck — checks the crate graph against architecture.yaml

Usage:
  archcheck [flags]

Flags:
  -json         print findings as JSON for CI
  -root <path>  repository root (default: walk up from the working directory
                to the directory holding architecture.yaml)
  -h, --help    print this help

Exit codes: 0 clean, 1 violations found, 2 usage or checker error.
";

struct Options {
    json: bool,
    root: Option<PathBuf>,
}

pub fn run(args: Vec<String>, cwd: Option<PathBuf>) -> i32 {
    let opts = match parse_args(&args) {
        Ok(Some(opts)) => opts,
        Ok(None) => return 0, // help was printed
        Err(message) => {
            eprintln!("archcheck: {message}");
            eprint!("{USAGE}");
            return 2;
        }
    };

    let cwd = cwd.unwrap_or_else(|| PathBuf::from("."));
    let cwd = fs::canonicalize(&cwd).unwrap_or(cwd);

    let root = match resolve_root(opts.root, &cwd) {
        Ok(root) => root,
        Err(message) => {
            eprintln!("archcheck: {message}");
            return 2;
        }
    };

    let architecture = match read_architecture(&root) {
        Ok(architecture) => architecture,
        Err(message) => {
            eprintln!("archcheck: {message}");
            return 2;
        }
    };

    let crates = match manifest::discover(&root, &architecture.ignore_dirs) {
        Ok(crates) => crates,
        Err(message) => {
            eprintln!("archcheck: {message}");
            return 2;
        }
    };

    let mut findings = check::run(&root, &architecture, &crates);
    findings.sort_by(|a, b| {
        (&a.file, a.line, a.rule, &a.message).cmp(&(&b.file, b.line, b.rule, &b.message))
    });

    let display = display_root(&root, &cwd);
    if opts.json {
        print!("{}", report::json(&display, &findings));
    } else {
        print!("{}", report::human(&display, &findings));
    }

    if findings.is_empty() {
        0
    } else {
        1
    }
}

fn parse_args(args: &[String]) -> Result<Option<Options>, String> {
    let mut json = false;
    let mut root = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-json" | "--json" => json = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-root" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "flag -root needs a path".to_string())?;
                root = Some(PathBuf::from(value));
            }
            _ if arg.starts_with("-root=") => {
                root = Some(PathBuf::from(&arg["-root=".len()..]));
            }
            _ if arg.starts_with('-') => return Err(format!("unknown flag {arg:?}")),
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
        i += 1;
    }
    Ok(Some(Options { json, root }))
}

// A checker that silently checks the wrong tree is worse than one that refuses
// to run, so a missing architecture.yaml is an error, never an empty pass.
fn resolve_root(flag: Option<PathBuf>, cwd: &Path) -> Result<PathBuf, String> {
    let candidate = match flag {
        Some(path) => {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }
        None => return find_root(cwd),
    };
    fs::canonicalize(&candidate).map_err(|err| format!("root {}: {err}", candidate.display()))
}

fn find_root(cwd: &Path) -> Result<PathBuf, String> {
    let mut dir = cwd.to_path_buf();
    loop {
        if dir.join("architecture.yaml").is_file() {
            return Ok(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => {
                return Err(format!(
                    "no architecture.yaml found in {} or any parent directory; pass -root",
                    cwd.display()
                ))
            }
        }
    }
}

fn read_architecture(root: &Path) -> Result<arch::Architecture, String> {
    let path = root.join("architecture.yaml");
    let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    arch::parse(&text).map_err(|err| format!("architecture.yaml: {err}"))
}

// Report paths relative to the working directory when they are below it: CI
// logs from different checkout locations stay diffable, and the common case
// (run from the repo root) prints ".".
fn display_root(root: &Path, cwd: &Path) -> String {
    match root.strip_prefix(cwd) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_string(),
        Ok(relative) => slash_path(relative),
        Err(_) => slash_path(root),
    }
}

// std::fs::canonicalize returns a verbatim path on Windows (\\?\D:\...).
// Strip that prefix before printing so CI logs and reports show a normal path.
fn slash_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let normalized = match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!("//{rest}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_string(),
    };
    normalized.replace('\\', "/")
}
