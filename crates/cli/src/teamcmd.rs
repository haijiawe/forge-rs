//! `hearth team`: run a team definition from the roster, or list the teams.
//!
//! A team in the roster is documentation — a coordinator, a list of members and
//! each member's duty — and this command turns it into a dispatch: one worker
//! per member, each on its own subagent session, all sharing one board. The
//! roster is the same scanner the `expert` and `team` tools use, so a team that
//! runs here is a team a turn could also run.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use crate::one_line;

const EXIT_OK: i32 = 0;
const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;

pub const USAGE: &str = "usage: hearth team <command> [flags]

Run a team of workers or list the teams the roster holds. A worker is one
subagent session; the members share a task board under <dataDir>/teams so their
results are one report rather than N transcripts.

commands:
  list                 every team in the roster, with its members
  run <slug>           dispatch the named team over a fresh board
  view <board>         describe an existing board directory and its journal

flags:
  --json               machine-readable output
  --data-dir <dir>     data directory (default: the hearth home's configured dataDir)
  --root <dir>         roster root; may be repeated (default: the configured
                       expert roots, then <dataDir>/experts)
  --max-parallel <n>   override the configured bound for this dispatch
  --model <name>       model for the workers (default: whatever the provider reads)
  -h, --help           this message

stdout: the listing or the report. Diagnostics go to stderr.

exit codes: 0 ok, 1 failure, 2 usage
";

const VALUE_FLAGS: &[&str] = &["data-dir", "root", "max-parallel", "model"];

#[derive(Debug, Default)]
struct Options {
    json: bool,
    data_dir: String,
    roots: Vec<String>,
    max_parallel: usize,
    model: String,
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
    // A board projection needs no roster: it describes files that already
    // exist. Handling it before the scan means a broken roster cannot stop an
    // operator from looking at a board.
    if command == "view" {
        return view(&opt, &reordered, stdout, stderr);
    }

    let roots = resolve_roots(&opt);
    let sets = match scan_roots(&roots, stderr) {
        Ok(sets) => sets,
        Err(code) => return code,
    };

    match command.as_str() {
        "list" => list(&sets, &roots, opt.json, stdout, stderr),
        "run" => run_team(&opt, &sets, &reordered, stdout, stderr),
        other => {
            let _ = writeln!(stderr, "hearth team: unknown command {other:?}\n\n{USAGE}");
            EXIT_USAGE
        }
    }
}

fn parse(args: &[String], stderr: &mut dyn Write) -> std::result::Result<Options, i32> {
    let mut opt = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // Positionals belong to the dispatch arm (`hearth team run <slug>`); reading
        // them as flags made every concrete invocation a usage error.
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
                            let _ = writeln!(stderr, "hearth team: --{name} needs a value");
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
            "model" => opt.model = take(&mut i)?,
            "max-parallel" => {
                let raw = take(&mut i)?;
                match raw.parse::<usize>() {
                    Ok(value) if value > 0 => opt.max_parallel = value,
                    _ => {
                        let _ = writeln!(
                            stderr,
                            "hearth team: --max-parallel wants a positive number, got {raw:?}"
                        );
                        return Err(EXIT_USAGE);
                    }
                }
            }
            "" => {}
            other => {
                let _ = writeln!(stderr, "hearth team: unknown flag --{other}");
                return Err(EXIT_USAGE);
            }
        }
        i += 1;
    }
    Ok(opt)
}

fn resolve_roots(opt: &Options) -> Vec<PathBuf> {
    if !opt.roots.is_empty() {
        return opt.roots.iter().map(PathBuf::from).collect();
    }
    let data_dir = if opt.data_dir.trim().is_empty() {
        crate::default_data_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(opt.data_dir.trim())
    };
    let cfg = config_for(opt);
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

fn config_for(opt: &Options) -> hearth_config::Config {
    let mut cfg = hearth_config::default_path()
        .ok()
        .and_then(|path| hearth_config::Config::load(&path).ok())
        .unwrap_or_default();
    // `--data-dir` overrides the config for this run, which is what makes
    // `hearth team run --data-dir X` land its board under X rather than under
    // whichever home the config names.
    if !opt.data_dir.trim().is_empty() {
        cfg.data_dir = opt.data_dir.trim().to_string();
    }
    cfg
}

fn data_dir(opt: &Options) -> PathBuf {
    let cfg = config_for(opt);
    if cfg.data_dir.trim().is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(cfg.data_dir.trim())
    }
}

fn scan_roots(
    roots: &[PathBuf],
    stderr: &mut dyn Write,
) -> std::result::Result<Vec<hearth_experts::Set>, i32> {
    let mut sets = Vec::new();
    for root in roots {
        match hearth_experts::scan(root) {
            Ok(set) => sets.push(set),
            Err(err) => {
                let _ = writeln!(stderr, "hearth team: {}: {err}", root.display());
                return Err(EXIT_FAILURE);
            }
        }
    }
    Ok(sets)
}

/// Describe an existing board directory: its tasks, which are ready, what each
/// is waiting on, and what the leads journalled. This is the reader that turns
/// the board and journal files into something a person can look at.
fn view(opt: &Options, args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let positionals = positional(args, "view");
    let Some(dir) = positionals.first().copied() else {
        let _ = writeln!(stderr, "hearth team: view needs a board directory");
        return EXIT_USAGE;
    };
    let path = std::path::Path::new(dir);
    if !path.is_dir() {
        let _ = writeln!(stderr, "hearth team: no such board directory: {dir}");
        return EXIT_FAILURE;
    }
    let view = match hearth_team::project(path) {
        Ok(view) => view,
        Err(err) => {
            let _ = writeln!(stderr, "hearth team: {}", err.message());
            return EXIT_FAILURE;
        }
    };
    if opt.json {
        let tasks: Vec<serde_json::Value> = view
            .tasks
            .iter()
            .map(|task| {
                serde_json::json!({
                    "id": task.id,
                    "subject": task.subject,
                    "status": task.status.as_str(),
                    "owner": task.owner,
                    "revision": task.revision,
                    "ready": task.is_ready(),
                    "waitingOn": task.waiting_on,
                    "missing": task.missing,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "board": view.board,
            "revision": view.revision,
            "counts": {
                "total": view.counts.total(),
                "pending": view.counts.pending,
                "inProgress": view.counts.in_progress,
                "completed": view.counts.completed,
            },
            "tasks": tasks,
            "leads": view.leads.iter().map(|(lead, count)| serde_json::json!({"lead": lead, "entries": count})).collect::<Vec<_>>(),
            "problems": view.problems,
        });
        let _ = writeln!(stdout, "{payload}");
        return EXIT_OK;
    }
    let _ = write!(stdout, "{}", view.render());
    EXIT_OK
}

fn list(
    sets: &[hearth_experts::Set],
    roots: &[PathBuf],
    json: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let teams: Vec<&hearth_experts::Team> = sets.iter().flat_map(|set| set.teams()).collect();
    if json {
        let entries: Vec<serde_json::Value> = teams
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
            "team list",
            &serde_json::json!({
                "roots": roots.iter().map(|root| root.to_string_lossy()).collect::<Vec<_>>(),
                "teams": entries,
            }),
        );
    }
    if teams.is_empty() {
        let _ = writeln!(
            stdout,
            "no teams under {}",
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return EXIT_OK;
    }
    let _ = writeln!(stdout, "{:<24} {:<10} members", "team", "coordinator");
    for team in &teams {
        let _ = writeln!(
            stdout,
            "{:<24} {:<10} {}",
            team.slug,
            team.coordinator,
            one_line(&format!("{} member(s)", team.members.len()))
        );
        for member in &team.members {
            let _ = writeln!(
                stdout,
                "  - {:<20} {}",
                member.expert,
                one_line(&member.duty)
            );
        }
    }
    EXIT_OK
}

fn run_team(
    opt: &Options,
    sets: &[hearth_experts::Set],
    args: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let positionals = positional(args, "run");
    let Some(slug) = positionals.first().copied() else {
        return usage(stderr, "run needs a team slug");
    };
    let Some(team) = sets.iter().find_map(|set| set.team(slug)) else {
        let _ = writeln!(
            stderr,
            "hearth team: no team named {slug:?}; known: {}",
            known(sets)
        );
        return EXIT_FAILURE;
    };

    // A member's prompt is its expert's brief, plus what this team expects of it.
    // The duty is the difference between "summon the reviewer" and "summon the
    // reviewer and check the concurrency story", and a worker that did not get it
    // would be doing a different job than the roster says.
    let mut prompts: Vec<(String, String)> = Vec::new();
    for member in &team.members {
        let mut brief = match sets
            .iter()
            .find_map(|set| hearth_experts::prompt_for(set, &member.expert))
        {
            Some(brief) => brief,
            None => {
                let _ = writeln!(
                    stderr,
                    "hearth team: member {:?} is not summonable (the roster names it, the scanner refused it)",
                    member.expert
                );
                return EXIT_FAILURE;
            }
        };
        brief.push_str(&format!(
            "\n\nYour duty in the team {:?}: {}",
            team.slug, member.duty
        ));
        let name = worker_name(&member.expert, prompts.len());
        prompts.push((name, brief));
    }

    // One provider and one runner for the whole dispatch: the workers share the
    // model and the data directory, and a per-worker provider would multiply the
    // TLS handshakes for no gain.
    let provider: Arc<dyn hearth_llm::Provider> =
        match hearth_llm::Client::new(hearth_llm::Options::default()) {
            Ok(client) => Arc::new(client),
            Err(err) => {
                let _ = writeln!(stderr, "hearth team: no LLM provider: {}", err.message());
                return EXIT_FAILURE;
            }
        };
    let cfg = config_for(opt);
    let workspace = workspace_dir();
    let model = if opt.model.trim().is_empty() {
        cfg.model.clone()
    } else {
        opt.model.trim().to_string()
    };
    if model.trim().is_empty() {
        // The subagent's own message ("Model is required") names the symptom but not
        // the fix, and it arrives with the crate prefix stacked on ours. Say what to
        // do while we still know we are the team command.
        let _ = writeln!(
            stderr,
            "hearth team: no model configured: pass --model <name> or set \"model\" in config.json"
        );
        return EXIT_FAILURE;
    }
    let mut sub = hearth_subagent::Options::new(
        Arc::clone(&provider),
        data_dir(opt),
        workspace.clone(),
        model,
    );
    sub.max_depth = cfg.subagent.max_depth.max(1) as usize;
    let runner = match hearth_subagent::Runner::new(sub) {
        Ok(runner) => Arc::new(runner),
        Err(err) => {
            let _ = writeln!(stderr, "hearth team: subagent: {}", err.message());
            return EXIT_FAILURE;
        }
    };
    let workers = crate::bridges::SubagentWorkers::new(Arc::clone(&runner), workspace);

    // A board per run, named after the team and the moment: two runs of the same
    // team are two dispatches, and sharing one board would let the second claim
    // the first's completed tasks.
    let stamp = crate::epoch_stamp(now_secs());
    let board_root = crate::bridges::team_root(&data_dir(opt)).join(format!("{slug}-{stamp}"));
    let board = match hearth_team::Board::open(&board_root) {
        Ok(board) => board,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "hearth team: open board {}: {err}",
                board_root.display()
            );
            return EXIT_FAILURE;
        }
    };

    let parallel = if opt.max_parallel > 0 {
        opt.max_parallel
    } else if cfg.team.max_parallel > 0 {
        cfg.team.max_parallel as usize
    } else {
        hearth_team::DEFAULT_MAX_PARALLEL
    };

    // A board task per member, named after the worker, so the report counts
    // claimed/completed rather than only reporting that a process exited zero.
    // The id is the worker name because that is how the dispatcher binds a claim
    // to a worker: one name, one task, no lookup table.
    for (index, (name, _)) in prompts.iter().enumerate() {
        let mut task = hearth_team::NewTask::new(team.members[index].duty.clone());
        task.description = format!("{} on team {}", team.members[index].expert, team.slug);
        let task = match board.add_task(task.id(name)) {
            Ok(task) => task,
            Err(err) => {
                let _ = writeln!(stderr, "hearth team: add task {name:?}: {err}");
                return EXIT_FAILURE;
            }
        };
        let _ = task;
    }
    let mut plan = hearth_team::Plan::new(&board_root).max_parallel(parallel);
    for (name, prompt) in &prompts {
        plan = plan.worker(hearth_team::WorkerSpec::new(name, prompt).task(name));
    }

    let report = hearth_team::run::run(&plan, &board, &workers);
    if opt.json {
        return crate::write_json(
            stdout,
            stderr,
            "team run",
            &serde_json::json!({
                "team": slug,
                "board": board_root.to_string_lossy(),
                "parallel": parallel,
                "report": report,
            }),
        );
    }
    let _ = writeln!(
        stdout,
        "{}: {} worker(s), {} task(s) completed, {} failed",
        slug,
        report.workers.len(),
        report.tasks.completed,
        report.tasks.failed
    );
    for worker in &report.workers {
        let _ = writeln!(
            stdout,
            "  - {:<20} {:<6} {}",
            worker.name,
            if worker.ok { "ok" } else { "failed" },
            one_line(&worker.note)
        );
    }
    if !report.problems.is_empty() {
        let _ = writeln!(stdout, "problems:");
        for problem in &report.problems {
            let _ = writeln!(stdout, "  - {problem}");
        }
    }
    let _ = writeln!(stdout, "board: {}", board_root.display());
    if report.ok() {
        EXIT_OK
    } else {
        EXIT_FAILURE
    }
}

fn worker_name(expert: &str, index: usize) -> String {
    // Worker names become file names, so they are constrained rather than
    // inherited from the roster's vocabulary.
    let mut name = expert
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while name.starts_with('.') || name.starts_with('-') {
        name.remove(0);
    }
    if name.is_empty() {
        name = format!("worker-{index}");
    }
    if !hearth_team::valid_name(&name) {
        name = format!("worker-{index}");
    }
    name
}

fn known(sets: &[hearth_experts::Set]) -> String {
    let mut names: Vec<String> = sets
        .iter()
        .flat_map(|set| set.teams().iter().map(|team| team.slug.clone()))
        .collect();
    names.sort();
    if names.is_empty() {
        return "none".to_string();
    }
    names.join(", ")
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

fn now_secs() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        // A clock before the epoch is not a reason to refuse a dispatch; the
        // board still gets a name.
        Err(_) => 0,
    }
}

/// The workspace the workers see: the directory `hearth team run` was typed in.
fn workspace_dir() -> String {
    match std::env::current_dir() {
        Ok(path) => path.to_string_lossy().to_string(),
        Err(_) => ".".to_string(),
    }
}

fn usage(stderr: &mut dyn Write, message: &str) -> i32 {
    let _ = writeln!(stderr, "hearth team: {message}\n\n{USAGE}");
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
        for command in ["list", "run"] {
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
    fn worker_names_survive_the_rosters_vocabulary() {
        // A slug the roster allows but a file name does not.
        assert_eq!(worker_name("ベース", 3), "worker-3");
        assert_eq!(worker_name(".hidden", 0), "hidden");
        assert_eq!(worker_name("rust-reviewer", 0), "rust-reviewer");
    }

    #[test]
    fn listing_no_teams_is_success() {
        let set = hearth_experts::Set::empty();
        let dir = std::env::temp_dir().join(format!(
            "hearth-team-cmd-{}-{}",
            std::process::id(),
            hearth_core::new_id().unwrap_or_default()
        ));
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = list(&[set], &[dir], false, &mut out, &mut err);
        assert_eq!(code, EXIT_OK);
        assert!(String::from_utf8_lossy(&out).contains("no teams under"));
    }
}

#[test]
fn view_needs_a_board_directory() {
    let opt = Options::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = view(&opt, &["view".to_string()], &mut out, &mut err);
    assert_eq!(code, EXIT_USAGE);
    assert!(String::from_utf8_lossy(&err).contains("board directory"));
}

#[test]
fn view_of_a_directory_that_does_not_exist_is_a_failure() {
    let opt = Options::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = view(
        &opt,
        &["view".to_string(), "no-such-board-dir-xyz".to_string()],
        &mut out,
        &mut err,
    );
    assert_eq!(code, EXIT_FAILURE);
    assert!(String::from_utf8_lossy(&err).contains("no such board directory"));
}

#[test]
fn view_describes_an_empty_board() {
    let dir = std::env::temp_dir().join(format!("hearth-team-view-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create");
    let opt = Options::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = view(
        &opt,
        &["view".to_string(), dir.to_string_lossy().into_owned()],
        &mut out,
        &mut err,
    );
    assert_eq!(code, EXIT_OK, "stderr: {}", String::from_utf8_lossy(&err));
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("0 task(s)"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_usage_names_view() {
    assert!(USAGE.contains("view <board>"), "usage must mention view");
}
