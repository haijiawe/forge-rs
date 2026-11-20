//! `hearth expert`: list the operators' expert roster and print one brief.
//!
//! A roster is documentation, not code: `scan` reads Markdown with frontmatter
//! and validates the whole tree together. This command lists what the same
//! scanner the turn will use can see, and prints the brief the `expert` tool will
//! hand to a subagent. It never starts anything.

use std::io::Write;
use std::path::PathBuf;

use crate::one_line;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "usage: hearth expert <command> [flags]

Inspect the expert roster: Markdown files with frontmatter that describe a persona
(an expert), or a team of personas. The same scanner the `expert` tool uses runs
here, so the listing is what a turn can actually summon.

commands:
  list                 every expert and team the roster holds
  show <slug>          print one expert's or team's brief

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory (default: the hearth home's configured dataDir)
  --root <dir>         roster root; may be repeated (default: the configured
                       expert roots, then <dataDir>/experts)
  -h, --help           this message

stdout: the listing or the brief. Diagnostics go to stderr.

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

    // Each root is scanned on its own: `scan` validates one tree, and a roster
    // with a defect stops the listing rather than silently dropping the file it
    // could not read. Several roots are therefore several validated sets, and a
    // lookup walks them in the order the operator listed them.
    let mut sets = Vec::new();
    for root in &roots {
        match hearth_experts::scan(root) {
            Ok(set) => sets.push(set),
            Err(err) => {
                let _ = writeln!(stderr, "hearth expert: {}: {err}", root.display());
                return EXIT_FAILURE;
            }
        }
    }

    match command.as_str() {
        "list" => list(&sets, &roots, opt.json, stdout, stderr),
        "show" => show(&sets, &reordered, stdout, stderr),
        other => {
            let _ = writeln!(
                stderr,
                "hearth expert: unknown command {other:?}\n\n{USAGE}"
            );
            EXIT_USAGE
        }
    }
}

fn parse(args: &[String], stderr: &mut dyn Write) -> std::result::Result<Options, i32> {
    let mut opt = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // Positionals belong to the dispatch arm, not here: `hearth expert show
        // <slug>` passes a bare word, and reading it as a flag produced "unknown
        // flag --<slug>" for the most ordinary invocation.
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
                            let _ = writeln!(stderr, "hearth expert: --{name} needs a value");
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
                let _ = writeln!(stderr, "hearth expert: unknown flag --{other}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opt)
}

/// Explicit `--root`s win; otherwise the config's roots, then `<dataDir>/experts`.
///
/// The last fallback exists so a first-run install with no config still has one
/// predictable place to put a roster.
fn resolve_roots(opt: &Options) -> Vec<PathBuf> {
    if !opt.roots.is_empty() {
        return opt.roots.iter().map(PathBuf::from).collect();
    }
    let data_dir = if opt.data_dir.trim().is_empty() {
        crate::default_data_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(opt.data_dir.trim())
    };
    let cfg = hearth_config::default_path()
        .ok()
        .and_then(|path| hearth_config::Config::load(&path).ok())
        .unwrap_or_default();
    let configured: Vec<PathBuf> = cfg
        .expert_roots()
        .into_iter()
        .filter(|root| !root.trim().is_empty())
        .map(PathBuf::from)
        .collect();
    if !configured.is_empty() {
        return configured;
    }
    vec![data_dir.join("experts")]
}

fn list(
    sets: &[hearth_experts::Set],
    roots: &[PathBuf],
    json: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let experts: Vec<&hearth_experts::Expert> = sets.iter().flat_map(|set| set.experts()).collect();
    let teams: Vec<&hearth_experts::Team> = sets.iter().flat_map(|set| set.teams()).collect();

    if json {
        let expert_entries: Vec<serde_json::Value> = experts
            .iter()
            .map(|expert| {
                serde_json::json!({
                    "slug": expert.slug,
                    "name": expert.name,
                    "description": expert.description,
                    "division": expert.division,
                    "enabled": expert.enabled,
                    "tools": expert.tools,
                    "path": expert.path.to_string_lossy(),
                })
            })
            .collect();
        let team_entries: Vec<serde_json::Value> = teams
            .iter()
            .map(|team| {
                serde_json::json!({
                    "slug": team.slug,
                    "name": team.name,
                    "description": team.description,
                    "coordinator": team.coordinator,
                    "members": team.members.iter().map(|member| serde_json::json!({
                        "expert": member.expert,
                        "duty": member.duty,
                    })).collect::<Vec<_>>(),
                    "path": team.path.to_string_lossy(),
                })
            })
            .collect();
        return crate::write_json(
            stdout,
            stderr,
            "expert list",
            &serde_json::json!({
                "roots": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
                "experts": expert_entries,
                "teams": team_entries,
            }),
        );
    }
    if experts.is_empty() && teams.is_empty() {
        let _ = writeln!(
            stdout,
            "no experts or teams under {}",
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return EXIT_OK;
    }
    if !experts.is_empty() {
        let _ = writeln!(stdout, "{:<24} {:<10} description", "expert", "state");
        for expert in &experts {
            let _ = writeln!(
                stdout,
                "{:<24} {:<10} {}",
                expert.slug,
                if expert.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                one_line(&expert.description)
            );
        }
    }
    if !teams.is_empty() {
        let _ = writeln!(stdout, "\n{:<24} {:<10} members", "team", "state");
        for team in &teams {
            let _ = writeln!(
                stdout,
                "{:<24} {:<10} {}",
                team.slug,
                "team",
                one_line(&format!(
                    "{} member(s), coordinator {}",
                    team.members.len(),
                    team.coordinator
                ))
            );
        }
    }
    EXIT_OK
}

fn show(
    sets: &[hearth_experts::Set],
    args: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let positionals: Vec<&str> = positional(args, "show");
    let Some(slug) = positionals.first().copied() else {
        return usage(stderr, "show needs an expert or team slug");
    };
    // Experts first: a roster where a slug names both is a defect the scanner
    // refuses, so the order only decides which lookup reports a miss.
    for set in sets {
        if let Some(brief) = hearth_experts::prompt_for(set, slug) {
            let _ = writeln!(stdout, "{brief}");
            return EXIT_OK;
        }
    }
    for set in sets {
        if let Some(brief) = hearth_experts::team_prompt(set, slug) {
            let _ = writeln!(stdout, "{brief}");
            return EXIT_OK;
        }
    }
    // A disabled expert is a different answer from an unknown slug, and it names
    // the file that says so.
    for set in sets {
        if let Some(expert) = set.expert(slug) {
            let _ = writeln!(
                stderr,
                "hearth expert: {slug:?} is disabled (enabled: false in {})",
                expert.path.display()
            );
            return EXIT_FAILURE;
        }
    }
    let _ = writeln!(
        stderr,
        "hearth expert: no expert or team named {slug:?}; known: {}",
        known(sets)
    );
    EXIT_FAILURE
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

fn known(sets: &[hearth_experts::Set]) -> String {
    let mut names: Vec<String> = sets
        .iter()
        .flat_map(|set| {
            set.experts()
                .iter()
                .map(|expert| expert.slug.clone())
                .chain(set.teams().iter().map(|team| team.slug.clone()))
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return "none".to_string();
    }
    names.join(", ")
}

fn usage(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "hearth expert: {message}\n\n{USAGE}");
    EXIT_USAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn the_usage_names_every_command() {
        for command in ["list", "show"] {
            assert!(USAGE.contains(command), "usage must name {command}");
        }
    }

    #[test]
    fn an_unknown_command_is_a_usage_error() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&args(&["nope"]), &mut out, &mut err);
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("unknown command"));
    }

    #[test]
    fn a_slug_is_not_a_flag() {
        // `hearth expert show <slug>` is the ordinary invocation; the slug is a bare
        // word, and reading it as a flag turned it into "unknown flag --<slug>".
        let mut err = Vec::new();
        let outcome = parse(&args(&["show", "rust-reviewer"]), &mut err);
        assert!(outcome.is_ok(), "stderr: {}", String::from_utf8_lossy(&err));
    }

    #[test]
    fn an_explicit_root_that_does_not_exist_lists_nothing() {
        let dir = std::env::temp_dir().join("hearth-expert-cmd-empty");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            &args(&["list", "--root", &dir.to_string_lossy()]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_OK);
        assert!(String::from_utf8_lossy(&out).contains("no experts or teams"));
    }

    #[test]
    fn show_without_a_slug_is_a_usage_error() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let dir = std::env::temp_dir().join("hearth-expert-cmd-none");
        let code = run(
            &args(&["show", "--root", &dir.to_string_lossy(), "--json"]),
            &mut out,
            &mut err,
        );
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&err).contains("needs an expert"));
    }
}
