//! `forge skill`: the roster the scanner already reads, made visible.
//!
//! Skills are offered to the model, not to the operator: `forge_skill::scan`
//! feeds the `skill` tool, and until now nothing on the command line showed what
//! that scan found. The result was that a `SKILL.md` with broken frontmatter was
//! silently absent — the model was offered less than the operator believed — and
//! nobody could tell which root a skill came from.
//!
//! This command therefore prints two things and never mixes them: the accepted
//! roster on stdout, and the scanner's own diagnostics on stderr, verbatim. The
//! diagnostics are not paraphrased because they are the only record of *why* a
//! skill is missing; the strings are the scanner's contract, and this module adds
//! a `forge skill:` prefix and nothing else.
//!
//! Nothing here writes to disk. An empty or absent root is a successful listing
//! of nothing, which is why `run` never creates the directory it looked in: a
//! `forge skill list` that mkdir'd would turn a typo into a permanent home for
//! future confusion.

use std::io::Write;
use std::path::{Path, PathBuf};

use forge_skill::{Set as SkillSet, Skill};

use crate::one_line;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

/// How much of a description the text listing shows. The JSON output carries the
/// whole string; this limit exists so one long description cannot push the root
/// off the line, because the root is the column an operator is looking for.
const MAX_DESCRIPTION: usize = 64;

pub const USAGE: &str = "usage: forge skill <command> [flags]

Show the skills the model was actually offered. The same scanner the `skill` tool
uses runs here, so a SKILL.md with broken frontmatter is named on stderr instead
of disappearing from the roster.

commands:
  list                 every skill the scanner accepted, with the root it came from
  show <name>          print one skill's path and body

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory (default: the forge home's configured dataDir)
  --root <dir>         skill root; may be repeated (default: the configured skill
                       roots, then <dataDir>/skills)
  -h, --help           this message

stdout: the listing or the skill. Skipped entries and warnings go to stderr,
verbatim, because those are the reason a skill is missing from the listing.

exit codes: 0 ok, 1 failure, 2 usage
";

const VALUE_FLAGS: &[&str] = &["data-dir", "root"];

#[derive(Debug, Default)]
struct Options {
    json: bool,
    data_dir: String,
    roots: Vec<String>,
}

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if crate::wants_help(args) {
        let _ = stdout.write_all(USAGE.as_bytes());
        return EXIT_OK;
    }
    let reordered = crate::reorder_flags(args, VALUE_FLAGS);
    let Some(command) = crate::command_after_flags(&reordered, VALUE_FLAGS) else {
        let _ = stderr.write_all(USAGE.as_bytes());
        return EXIT_USAGE;
    };
    let opt = match parse(&reordered, stderr) {
        Ok(opt) => opt,
        Err(code) => return code,
    };
    let roots = resolve_roots(&opt);

    // One scan over every root, not one scan per root: `scan` already handles
    // several roots as a single set, and that is what makes a duplicate name
    // across roots resolvable (the first root wins, the second is reported). Per
    // root scans would each accept the duplicate and print the same skill twice.
    let set = match forge_skill::scan(&roots) {
        Ok(set) => set,
        // `scan` only fails on a broken internal invariant, but a CLI that
        // pretended otherwise would report an empty roster as an empty machine.
        Err(err) => {
            let _ = writeln!(stderr, "forge skill: scan: {err}");
            return EXIT_FAILURE;
        }
    };
    diagnostics(&set, stderr);

    match command.as_str() {
        "list" => list(&set, &roots, opt.json, stdout, stderr),
        "show" => show(&set, &reordered, opt.json, stdout, stderr),
        other => {
            let _ = writeln!(stderr, "forge skill: unknown command {other:?}\n\n{USAGE}");
            EXIT_USAGE
        }
    }
}

/// The scanner's diagnostics, verbatim, with only the command name prefixed.
///
/// `skipped` is the important one: a missing description, an unclosed fence or an
/// invalid name lands there, and it is exactly the case an operator cannot see
/// any other way.
fn diagnostics(set: &SkillSet, stderr: &mut dyn Write) {
    for line in set.skipped.iter().chain(set.warnings.iter()) {
        let _ = writeln!(stderr, "forge skill: {line}");
    }
}

fn parse(args: &[String], stderr: &mut dyn Write) -> std::result::Result<Options, i32> {
    let mut opt = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // Positionals belong to the dispatch arm: `forge skill show <name>` passes
        // a bare word, and reading it as a flag produced "unknown flag --<name>"
        // for the most ordinary invocation.
        if !arg.starts_with('-') {
            i += 1;
            continue;
        }
        let name = arg.trim_start_matches('-');
        let (name, inline) = match name.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (name.to_string(), None),
        };
        let mut take = |index: &mut usize| -> std::result::Result<String, i32> {
            match inline.clone() {
                Some(value) => Ok(value),
                None => {
                    *index += 1;
                    match args.get(*index) {
                        Some(value) => Ok(value.clone()),
                        None => {
                            let _ = writeln!(stderr, "forge skill: --{name} needs a value");
                            Err(EXIT_USAGE)
                        }
                    }
                }
            }
        };
        match name.as_str() {
            "json" if inline.is_none() => opt.json = true,
            "data-dir" => opt.data_dir = take(&mut i)?,
            "root" => opt.roots.push(take(&mut i)?),
            "" => {}
            other => {
                let _ = writeln!(stderr, "forge skill: unknown flag --{other}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opt)
}

/// Explicit `--root`s win; otherwise the config's skill roots, then
/// `<dataDir>/skills` — the same default the turn uses, so `forge skill list`
/// without flags lists exactly what the model was offered.
fn resolve_roots(opt: &Options) -> Vec<PathBuf> {
    if !opt.roots.is_empty() {
        return opt.roots.iter().map(PathBuf::from).collect();
    }
    let data_dir = if opt.data_dir.trim().is_empty() {
        crate::default_data_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(opt.data_dir.trim())
    };
    let cfg = forge_config::default_path()
        .ok()
        .and_then(|path| forge_config::Config::load(&path).ok())
        .unwrap_or_default();
    let configured: Vec<PathBuf> = cfg
        .skill_roots()
        .into_iter()
        .filter(|root| !root.trim().is_empty())
        .map(PathBuf::from)
        .collect();
    if !configured.is_empty() {
        return configured;
    }
    vec![data_dir.join("skills")]
}

fn list(
    set: &SkillSet,
    roots: &[PathBuf],
    json: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if json {
        let skills: Vec<serde_json::Value> = set
            .skills
            .iter()
            .map(|skill| {
                serde_json::json!({
                    "name": skill.name,
                    "description": skill.description,
                    "root": root_of(skill, roots),
                    "dir": skill.dir,
                    "path": skill.path,
                    "files": skill.files,
                })
            })
            .collect();
        let code = crate::write_json(
            stdout,
            stderr,
            "skill list",
            &serde_json::json!({
                "roots": roots.iter().map(|root| root_text(root)).collect::<Vec<_>>(),
                "skills": skills,
                "skipped": set.skipped,
                "warnings": set.warnings,
            }),
        );
        if code != EXIT_OK {
            return code;
        }
    } else if set.skills.is_empty() {
        // Success, and deliberately not a mkdir: an absent root is an empty
        // roster, not a reason to create a directory the operator did not ask for.
        let _ = writeln!(
            stdout,
            "no skills under {}",
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    } else {
        let _ = writeln!(
            stdout,
            "{:<24} {:<width$} root",
            "skill",
            "description",
            width = MAX_DESCRIPTION
        );
        for skill in &set.skills {
            let _ = writeln!(
                stdout,
                "{:<24} {:<width$} {}",
                skill.name,
                truncated(&skill.description, MAX_DESCRIPTION),
                root_of(skill, roots),
                width = MAX_DESCRIPTION
            );
        }
    }
    // Fail closed on the one case that looks like success and is not: files were
    // there and none parsed, so the model was offered nothing while the operator
    // believes otherwise. An absent root stays `0`; an empty roster *with*
    // skipped entries is a failure, in both output modes, and the diagnostics
    // already on stderr say why.
    if set.skills.is_empty() && !set.skipped.is_empty() {
        let count = set.skipped.len();
        let entries = if count == 1 {
            "1 entry was skipped".to_string()
        } else {
            format!("{count} entries were skipped")
        };
        let _ = writeln!(stderr, "forge skill: nothing parsed; {entries} (see above)");
        return EXIT_FAILURE;
    }
    EXIT_OK
}

fn show(
    set: &SkillSet,
    args: &[String],
    json: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let positionals: Vec<&str> = positional(args, "show");
    let Some(name) = positionals.first().copied() else {
        return usage(stderr, "show needs a skill name");
    };
    let Some(skill) = set.skills.iter().find(|skill| skill.name == name) else {
        let _ = writeln!(
            stderr,
            "forge skill: no skill named {name:?}; known: {}",
            known(set)
        );
        return EXIT_FAILURE;
    };
    // Scanning never reads a body; only asking for one skill does.
    let loaded = match skill.load() {
        Ok(loaded) => loaded,
        // The message already carries the path, so it is printed as it comes.
        Err(err) => {
            let _ = writeln!(stderr, "forge skill: {err}");
            return EXIT_FAILURE;
        }
    };
    if json {
        return crate::write_json(
            stdout,
            stderr,
            "skill show",
            &serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "dir": skill.dir,
                "path": skill.path,
                "files": skill.files,
                "body": loaded.body,
            }),
        );
    }
    let _ = writeln!(stdout, "skill: {}\npath:  {}\n", loaded.name, loaded.path);
    let _ = stdout.write_all(loaded.body.as_bytes());
    if !loaded.body.ends_with('\n') {
        let _ = stdout.write_all(b"\n");
    }
    EXIT_OK
}

fn positional<'a>(args: &'a [String], command: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut seen_command = false;
    let mut stop_flags = false;
    while i < args.len() {
        let arg = args[i].as_str();
        if stop_flags {
            out.push(arg);
            i += 1;
            continue;
        }
        if arg == command && !seen_command {
            seen_command = true;
            i += 1;
            continue;
        }
        if arg == "--" {
            stop_flags = true;
            i += 1;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            if !name.contains('=') && VALUE_FLAGS.contains(&name) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        out.push(arg);
        i += 1;
    }
    out
}

/// The root a skill came from.
///
/// The scanner does not record it, but it builds `dir` as `<root>/<name>`, so the
/// root is the prefix. Normalising both sides the same way the scanner does
/// (forward slashes) keeps this correct on Windows without touching the string
/// `Skill` reports.
fn root_of(skill: &Skill, roots: &[PathBuf]) -> String {
    for root in roots {
        let text = root_text(root);
        if text.is_empty() {
            continue;
        }
        if skill.dir == text || skill.dir.starts_with(&format!("{text}/")) {
            return text;
        }
    }
    // Unreachable through `scan`; reported as such rather than as a path that
    // would look like a real answer.
    "(unknown root)".to_string()
}

fn root_text(root: &Path) -> String {
    let text = root.to_string_lossy().into_owned();
    let text = if std::path::MAIN_SEPARATOR == '\\' {
        text.replace('\\', "/")
    } else {
        text
    };
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() && text.starts_with('/') {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn truncated(text: &str, max: usize) -> String {
    let text = one_line(text);
    if text.chars().count() <= max {
        return text;
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

fn known(set: &SkillSet) -> String {
    let mut names: Vec<&str> = set.skills.iter().map(|skill| skill.name.as_str()).collect();
    names.sort_unstable();
    if names.is_empty() {
        return "none".to_string();
    }
    names.join(", ")
}

fn usage(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "forge skill: {message}\n\n{USAGE}");
    EXIT_USAGE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn run_bytes(items: &[&str]) -> (i32, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&args(items), &mut out, &mut err);
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    }

    /// A temporary skill root. Removed on drop, unlike the shared temp paths the
    /// expert tests use, because these cases write files.
    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "forge-skill-cmd-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("create temp root");
            Self { path }
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent");
            }
            std::fs::write(path, text).expect("write file");
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn skill_file(name: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}")
    }

    #[test]
    fn the_usage_names_every_command() {
        for command in ["list", "show"] {
            assert!(USAGE.contains(command), "usage must name {command}");
        }
    }

    #[test]
    fn an_unknown_command_is_a_usage_error() {
        let (code, _, err) = run_bytes(&["nope"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.contains("unknown command"), "stderr: {err}");
    }

    #[test]
    fn a_skill_name_is_not_a_flag() {
        let mut err = Vec::new();
        let outcome = parse(&args(&["show", "office"]), &mut err);
        assert!(outcome.is_ok(), "stderr: {}", String::from_utf8_lossy(&err));
    }

    #[test]
    fn an_explicit_root_that_does_not_exist_lists_nothing_and_creates_nothing() {
        let absent = std::env::temp_dir().join(format!(
            "forge-skill-cmd-absent-{}-{}",
            std::process::id(),
            "root"
        ));
        let _ = std::fs::remove_dir_all(&absent);
        let root = absent.to_string_lossy().into_owned();
        let (code, out, _) = run_bytes(&["list", "--root", &root]);
        assert_eq!(code, EXIT_OK);
        assert!(out.contains("no skills under"), "stdout: {out}");
        assert!(
            !absent.exists(),
            "a listing must not create the root it looked in"
        );
    }

    #[test]
    fn a_broken_frontmatter_is_reported_with_the_scanners_own_message() {
        let root = TempRoot::new("broken");
        root.write(
            "bad/SKILL.md",
            "---\nname: bad\ndescription: Broken\nno closing fence\n",
        );
        let (code, out, err) = run_bytes(&["list", "--root", &root.path.to_string_lossy()]);
        // Nothing parsed while a directory was there: the model was offered an
        // empty roster, so the listing that "succeeded" is a failure.
        assert_eq!(code, EXIT_FAILURE, "stderr: {err}");
        assert!(out.contains("no skills under"), "stdout: {out}");
        // The scanner normalises paths to forward slashes, so the expectation
        // is built with the same helper rather than with `display()`, which on
        // Windows would produce a backslash the scanner never prints.
        let expected = format!(
            "{}: frontmatter is not closed by a second --- line",
            root_text(&root.path.join("bad"))
        );
        assert!(err.contains(&expected), "stderr: {err}");
        assert!(err.starts_with("forge skill: "), "stderr: {err}");
        assert!(err.contains("nothing parsed"), "stderr: {err}");
    }

    #[test]
    fn a_mixed_roster_lists_the_good_skill_and_only_warns_about_the_broken_one() {
        let root = TempRoot::new("mixed");
        root.write(
            "office/SKILL.md",
            &skill_file("office", "Documents", "body\n"),
        );
        root.write(
            "bad/SKILL.md",
            "---\nname: bad\ndescription: Broken\nno closing fence\n",
        );
        let root_path = root.path.to_string_lossy().into_owned();
        let (code, out, err) = run_bytes(&["list", "--root", &root_path]);
        // One skill parsed, so the listing did what it says and the broken
        // sibling is a diagnostic rather than a failure.
        assert_eq!(code, EXIT_OK, "stderr: {err}");
        assert!(out.contains("office"), "stdout: {out}");
        assert!(!out.contains("bad"), "stdout: {out}");
        assert!(err.contains("frontmatter is not closed"), "stderr: {err}");
        assert!(!err.contains("nothing parsed"), "stderr: {err}");
    }

    #[test]
    fn list_shows_the_name_the_description_and_the_root() {
        let root = TempRoot::new("list");
        root.write(
            "office/SKILL.md",
            &skill_file("office", "Create and read documents", "body\n"),
        );
        let root_text = root.path.to_string_lossy().into_owned();
        let (code, out, err) = run_bytes(&["list", "--root", &root_text]);
        assert_eq!(code, EXIT_OK, "stderr: {err}");
        assert!(out.contains("skill"), "stdout: {out}");
        assert!(out.contains("office"), "stdout: {out}");
        assert!(out.contains("Create and read documents"), "stdout: {out}");
        assert!(
            out.contains(&root_text) || out.contains(&root_text.replace('\\', "/")),
            "stdout must name the root: {out}"
        );
    }

    #[test]
    fn a_long_description_is_truncated_in_text_but_not_in_json() {
        let root = TempRoot::new("long");
        let long = "d".repeat(MAX_DESCRIPTION * 2);
        root.write("long/SKILL.md", &skill_file("long", &long, "body\n"));
        let root_text = root.path.to_string_lossy().into_owned();
        let (code, out, _) = run_bytes(&["list", "--root", &root_text]);
        assert_eq!(code, EXIT_OK);
        assert!(out.contains('…'), "text listing must truncate: {out}");
        let (code, out, _) = run_bytes(&["list", "--json", "--root", &root_text]);
        assert_eq!(code, EXIT_OK);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
        assert_eq!(
            parsed["skills"][0]["description"],
            serde_json::Value::String(long)
        );
    }

    #[test]
    fn list_json_has_the_stable_shape() {
        let root = TempRoot::new("json");
        root.write(
            "office/SKILL.md",
            &skill_file("office", "Documents", "body\n"),
        );
        let root_text = root.path.to_string_lossy().into_owned();
        let (code, out, err) = run_bytes(&["list", "--json", "--root", &root_text]);
        assert_eq!(code, EXIT_OK, "stderr: {err}");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
        let mut keys: Vec<&str> = parsed
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["roots", "skills", "skipped", "warnings"]);
        let skill = &parsed["skills"][0];
        let mut fields: Vec<&str> = skill
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            vec!["description", "dir", "files", "name", "path", "root"]
        );
        assert_eq!(skill["name"], "office");
    }

    #[test]
    fn show_prints_the_path_and_the_body() {
        let root = TempRoot::new("show");
        root.write(
            "office/SKILL.md",
            &skill_file("office", "Documents", "the body\n"),
        );
        let root_text = root.path.to_string_lossy().into_owned();
        let (code, out, err) = run_bytes(&["show", "office", "--root", &root_text]);
        assert_eq!(code, EXIT_OK, "stderr: {err}");
        assert!(out.contains("skill: office"), "stdout: {out}");
        assert!(out.contains("SKILL.md"), "stdout: {out}");
        assert!(out.contains("the body"), "stdout: {out}");
    }

    #[test]
    fn an_unknown_name_exits_one_and_names_the_known_ones() {
        let root = TempRoot::new("unknown");
        root.write(
            "office/SKILL.md",
            &skill_file("office", "Documents", "body\n"),
        );
        let root_text = root.path.to_string_lossy().into_owned();
        let (code, out, err) = run_bytes(&["show", "nope", "--root", &root_text]);
        assert_eq!(code, EXIT_FAILURE);
        assert!(out.is_empty(), "stdout: {out}");
        assert!(err.contains("no skill named \"nope\""), "stderr: {err}");
        assert!(
            err.contains("office"),
            "stderr must name the known ones: {err}"
        );
    }

    #[test]
    fn show_without_a_name_is_a_usage_error() {
        let root = TempRoot::new("noname");
        let root_text = root.path.to_string_lossy().into_owned();
        let (code, _, err) = run_bytes(&["show", "--root", &root_text, "--json"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(err.contains("needs a skill name"), "stderr: {err}");
    }

    #[test]
    fn a_skill_root_that_is_a_plain_file_lists_nothing() {
        let root = TempRoot::new("file");
        root.write("plain.txt", "not a root");
        let (code, out, err) = run_bytes(&[
            "list",
            "--root",
            &root.path.join("plain.txt").to_string_lossy(),
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(out.contains("no skills under"), "stdout: {out}");
        assert!(
            err.contains("root is not a directory; skipped"),
            "the scanner's own words: {err}"
        );
    }
}
