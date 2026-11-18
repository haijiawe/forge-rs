//! The P3 bridges: plugins, expert rosters and teams, as tools.
//!
//! These live in the CLI crate for the same reason the MCP bridge does
//! (`wiring.rs`): they need three things at once that no single library crate
//! owns — what the operator configured, the process that must own the children,
//! and the provider a team worker runs on. `forge-tool` deliberately does not
//! depend on `forge-plugin` or `forge-team`; it only ever sees `Arc<dyn Tool>`.
//!
//! Three rules shaped this module:
//!
//! 1. **A plugin tool's name is namespaced** (`<plugin>.<tool>`). Two plugins can
//!    both ship an `add`, and a registry with two `add`s panics on the second
//!    registration.
//! 2. **A failing call is data.** A plugin or worker that reports failure has
//!    still produced an answer the model should read; only a broken host, a dead
//!    child or a timeout is an error result.
//! 3. **Nothing here spawns at wiring time.** `Host::start` and `Board::open`
//!    are lazy; a team member is only started when a `team` call arrives.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_llm::ToolSpec;
use forge_tool::{
    decode_args, effective_limits, truncate, Call, Limits, Meta, Result as ToolResult, Tool,
    CODE_IO_ERROR, CODE_NOT_FOUND, ERR_BAD_ARGS,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Separator between a plugin name and its tool name.
pub const PLUGIN_NAME_SEPARATOR: char = '.';
/// Tool that summons one expert from the roster.
pub const EXPERT_TOOL_NAME: &str = "expert";
/// Tool that runs a team of workers over a shared board.
pub const TEAM_TOOL_NAME: &str = "team";
/// The plugin host refused the call itself (spawn, timeout, protocol) rather than
/// the plugin's tool reporting a failure.
pub const CODE_PLUGIN_CALL_FAILED: &str = "plugin_call_failed";
/// The dispatch finished but something in it did not.
pub const CODE_TEAM_INCOMPLETE: &str = "team_incomplete";

/// How many slugs a description lists. The description is sent on every request,
/// so a 300-expert roster must not cost 300 lines of prompt.
pub const MAX_EXPERTS_IN_DESCRIPTION: usize = 24;

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

/// One tool a plugin contributed, behind the `Tool` interface.
pub struct PluginTool {
    host: Arc<forge_plugin::Host>,
    plugin: String,
    /// The name the plugin knows.
    tool: String,
    /// `<plugin>.<tool>`.
    name: String,
    /// Precomputed: the loop asks for the spec on every request and a plugin
    /// must not be asked again for a description it already gave.
    spec: ToolSpec,
    limits: Limits,
}

impl PluginTool {
    pub fn new(
        host: Arc<forge_plugin::Host>,
        plugin: impl Into<String>,
        tool: impl Into<String>,
        spec: ToolSpec,
        limits: Limits,
    ) -> Self {
        let plugin = plugin.into();
        let tool = tool.into();
        let name = format!("{plugin}{}{tool}", PLUGIN_NAME_SEPARATOR);
        Self {
            host,
            plugin,
            tool,
            name,
            spec,
            limits,
        }
    }

    /// `<plugin>.<tool>`.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn call(&self, call: Call) -> ToolResult {
        // Verbatim, not decoded and re-encoded: the arguments belong to the
        // plugin tool's schema, and a round trip would drop fields this build
        // does not know about.
        let args = if call.args.is_null() {
            json!({})
        } else {
            call.args.clone()
        };

        let mut meta = Meta::new();
        meta.insert("plugin".into(), json!(self.plugin));
        meta.insert("tool".into(), json!(self.tool));

        match self.host.call_tool(&self.plugin, &self.tool, args) {
            Ok(output) => {
                let (text, cut) = truncate(&output.output, effective_limits(self.limits));
                let mut out = ToolResult::ok(text);
                out.truncated = cut;
                out.meta = meta;
                if output.is_error {
                    // A successful call that failed: pass the text through and
                    // let the model decide, exactly as the MCP bridge does.
                    out.is_error = true;
                    out.code = "plugin_tool_error".to_string();
                }
                out
            }
            Err(err) => {
                // The host's own failure codes are stable strings, but they are
                // not this crate's vocabulary: report the host code in `meta`
                // and use one code here, so a caller can tell "the plugin said
                // no" from "the plugin is gone".
                meta.insert("hostCode".into(), json!(err.code()));
                let mut out =
                    ToolResult::error(CODE_PLUGIN_CALL_FAILED, format!("{}: {err}", self.name));
                out.meta = meta;
                out
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Experts
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ExpertArgs {
    #[serde(default)]
    name: String,
}

/// The single tool for a scanned expert roster.
///
/// Skill-shaped on purpose: a roster is documentation the operator wrote, and the
/// useful action is "give me that document", not "run something". A team is the
/// other half (see [`TeamTool`]).
pub struct ExpertTool {
    set: forge_experts::Set,
    /// Sorted, so the description and every "available experts" list is stable.
    slugs: Vec<String>,
    description: String,
    limits: Limits,
}

impl ExpertTool {
    pub fn new(set: forge_experts::Set, limits: Limits) -> Self {
        let slugs: Vec<String> = {
            let mut names: Vec<String> = set
                .experts()
                .iter()
                .filter(|expert| expert.enabled)
                .map(|expert| expert.slug.clone())
                .collect();
            names.sort();
            names.dedup();
            names
        };
        let listed = &slugs[..slugs.len().min(MAX_EXPERTS_IN_DESCRIPTION)];
        let description = format!(
            "Summon one expert from the operator's roster: call with {{\"name\":\"<slug>\"}} and the brief comes back as text. \
             Available experts: {}{}.",
            if listed.is_empty() {
                "none".to_string()
            } else {
                listed.join(", ")
            },
            if slugs.len() > listed.len() {
                format!(" (and {} more)", slugs.len() - listed.len())
            } else {
                String::new()
            }
        );
        Self {
            set,
            slugs,
            description,
            limits,
        }
    }
}

impl Tool for ExpertTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: EXPERT_TOOL_NAME.to_string(),
            description: self.description.clone(),
            schema: serde_json::from_str(
                r#"{"type":"object","properties":{"name":{"type":"string","description":"Expert slug from the roster."}},"required":["name"]}"#,
            )
            .expect("the expert schema is a constant"),
        }
    }

    fn call(&self, call: Call) -> ToolResult {
        let args: ExpertArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(fail) => return fail,
        };
        let name = args.name.trim();
        if name.is_empty() {
            return ToolResult::error(
                ERR_BAD_ARGS,
                format!(
                    "{EXPERT_TOOL_NAME}: \"name\" is required; available experts: {}",
                    self.available()
                ),
            );
        }
        // A disabled expert is `not_found` rather than `bad_args`: the name was
        // well formed and the listing is the recovery hint, but summoning a
        // retired persona is the operator's decision, not the model's.
        let Some(brief) = forge_experts::prompt_for(&self.set, name) else {
            return ToolResult::error(
                CODE_NOT_FOUND,
                format!(
                    "{EXPERT_TOOL_NAME}: no enabled expert named {name:?}; available experts: {}",
                    self.available()
                ),
            );
        };
        let (text, cut) = truncate(&brief, effective_limits(self.limits));
        let mut out = ToolResult::ok(text);
        out.truncated = cut;
        out.meta.insert("expert".into(), json!(name));
        out
    }
}

impl ExpertTool {
    fn available(&self) -> String {
        if self.slugs.is_empty() {
            return "none".to_string();
        }
        self.slugs.join(", ")
    }
}

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TeamArgs {
    #[serde(default)]
    members: Vec<TeamMemberArg>,
    #[serde(default)]
    max_parallel: usize,
}

#[derive(Deserialize)]
struct TeamMemberArg {
    #[serde(default)]
    name: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    task: String,
}

/// One tool that dispatches workers over a shared board.
pub struct TeamTool {
    runner: Arc<dyn forge_team::WorkerRunner>,
    /// `<dataDir>/teams/<call-id>` per call: a board per dispatch, because a
    /// mutable board shared between unrelated calls would let one call's failure
    /// make the next call's report wrong.
    root: PathBuf,
    /// The configured bound, restated in the description so the model knows how
    /// much parallelism it is asking for.
    parallel: usize,
    description: String,
    limits: Limits,
}

impl TeamTool {
    pub fn new(
        runner: Arc<dyn forge_team::WorkerRunner>,
        root: impl Into<PathBuf>,
        max_parallel: usize,
        limits: Limits,
    ) -> Self {
        let parallel = max_parallel.max(1);
        let description = format!(
            "Dispatch parallel workers over a shared task board. Each member gets its own session, \
             prompt and transcript; the report comes back here and the board stays on disk. At most \
             {parallel} run at once. Use this when the work splits into independent pieces; for one \
             piece, use `subagent`."
        );
        Self {
            runner,
            root: root.into(),
            parallel,
            description,
            limits,
        }
    }
}

impl Tool for TeamTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TEAM_TOOL_NAME.to_string(),
            description: self.description.clone(),
            schema: serde_json::from_str(
                r#"{"type":"object","properties":{"members":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"prompt":{"type":"string"},"task":{"type":"string"}},"required":["name","prompt"]}},"max_parallel":{"type":"integer","description":"Workers allowed to run at once; cannot exceed the configured bound."}},"required":["members"]}"#,
            )
            .expect("the team schema is a constant"),
        }
    }

    fn call(&self, call: Call) -> ToolResult {
        let args: TeamArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(fail) => return fail,
        };
        if args.members.is_empty() {
            return ToolResult::error(
                ERR_BAD_ARGS,
                format!("{TEAM_TOOL_NAME}: \"members\" is empty; a team needs at least one worker"),
            );
        }

        // The board root is per call and named after the call id, which is unique
        // per tool invocation. The id is model-visible in the log and therefore
        // not a file name, so it is sanitised before it reaches the filesystem.
        let board_root = self.root.join(sanitise(&call.id));
        let board = match forge_team::Board::open(&board_root) {
            Ok(board) => board,
            Err(err) => {
                return ToolResult::error(
                    CODE_IO_ERROR,
                    format!(
                        "{TEAM_TOOL_NAME}: open board {}: {err}",
                        board_root.display()
                    ),
                )
            }
        };

        // A caller asking for more parallelism than configured gets the
        // configured bound: the config is the operator's ceiling, and a model
        // that ignored it would be spending money the operator did not agree to.
        let asked = if args.max_parallel == 0 {
            self.parallel
        } else {
            args.max_parallel.min(self.parallel)
        };
        let mut plan = forge_team::Plan::new(&board_root).max_parallel(asked);
        let mut specs: Vec<(String, String)> = Vec::new();
        for member in &args.members {
            let name = member.name.trim().to_string();
            if name.is_empty() {
                return ToolResult::error(
                    ERR_BAD_ARGS,
                    format!("{TEAM_TOOL_NAME}: every member needs a name"),
                );
            }
            let prompt = member.prompt.trim();
            if prompt.is_empty() {
                return ToolResult::error(
                    ERR_BAD_ARGS,
                    format!("{TEAM_TOOL_NAME}: member {name:?} has an empty prompt"),
                );
            }
            plan = plan.worker(forge_team::WorkerSpec::new(&name, prompt));
            specs.push((name, member.task.trim().to_string()));
        }

        // Tasks go on the board before the dispatch so the claim inside `run`
        // has something to claim; a rejected task is a board fact reported here
        // rather than a worker that mysteriously found nothing to do.
        for (name, subject) in &specs {
            let mut new_task = forge_team::NewTask::new(subject.as_str());
            new_task.description = format!("assigned to {name}");
            if let Err(err) = board.add_task(new_task) {
                return ToolResult::error(
                    CODE_IO_ERROR,
                    format!("{TEAM_TOOL_NAME}: add task for {name:?}: {err}"),
                );
            }
        }

        // `run` is the dispatcher function, but `run` is also the module that
        // holds it, so the path has to be spelled out.
        let report = forge_team::run::run(&plan, &board, self.runner.as_ref());
        let (text, cut) = truncate(&render(&report, &board_root), effective_limits(self.limits));
        let mut out = ToolResult::ok(text);
        out.truncated = cut;
        out.meta
            .insert("workers".into(), json!(report.workers.len()));
        out.meta.insert("failed".into(), json!(report.tasks.failed));
        if !report.problems.is_empty() {
            out.meta.insert("problems".into(), json!(report.problems));
        }
        if !report.ok() {
            // Somebody did not do their part. The text is still the answer: the
            // caller needs to read which worker said what.
            out.is_error = true;
            out.code = CODE_TEAM_INCOMPLETE.to_string();
        }
        out
    }
}

/// Renders a report as the model should read it: a header, one section per
/// worker, then the board problems. The JSON board stays on disk for anyone who
/// wants more.
///
/// The worker's own words are not in the report — the dispatcher keeps the
/// report small and writes each worker's answer to
/// `<board root>/workers/<name>.jsonl` — so they are read back here. A report
/// without the answers would tell the model which workers failed and nothing
/// about what they said.
fn render(report: &forge_team::Report, board_root: &Path) -> String {
    let ok = report.workers.iter().filter(|worker| worker.ok).count();
    let mut out = format!(
        "{} worker(s): {ok} ok, {} failed\n",
        report.workers.len(),
        report.workers.len() - ok
    );
    for worker in &report.workers {
        let body =
            worker_text(board_root, &worker.name).unwrap_or_else(|| worker.note.trim().to_string());
        out.push_str(&format!(
            "\n## {} ({})\n{body}\n",
            worker.name,
            if worker.ok { "ok" } else { "failed" }
        ));
    }
    if !report.problems.is_empty() {
        out.push_str("\nproblems:\n");
        for problem in &report.problems {
            out.push_str(&format!("- {problem}\n"));
        }
    }
    out
}

/// The last `worker/end` line's `text` for one worker, if the transcript is
/// readable.
///
/// Best effort on purpose: a transcript that cannot be read is a missing section
/// in the answer, not a failed dispatch, so the caller falls back to the report's
/// note. The file is the dispatcher's own writing, under `<board root>/workers/`.
fn worker_text(board_root: &Path, name: &str) -> Option<String> {
    if !forge_team::valid_name(name) {
        return None;
    }
    let path = board_root
        .join(forge_team::WORKERS_DIR)
        .join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(path).ok()?;
    let mut answer = None;
    for line in text.lines() {
        let value: Value = serde_json::from_str(line).ok()?;
        if value.get("kind").and_then(Value::as_str) == Some("worker/end") {
            answer = value
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|body| !body.is_empty())
                .map(str::to_string);
        }
    }
    answer
}

/// A call id is model-visible in the log, so it is not a file name. Keep the
/// characters that survive a round trip and replace the rest.
fn sanitise(id: &str) -> String {
    let mut out: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.chars().all(|c| c == '_') {
        out = "call".to_string();
    }
    out
}

// ---------------------------------------------------------------------------
// The subagent-backed worker runner
// ---------------------------------------------------------------------------

/// Runs one team worker as a subagent session.
///
/// The worker's transcript is a `forge-team` file, not a session log; the session
/// log belongs to the child subagent. Law 1 ("one writer per log") holds because
/// the team writes the transcript and the child's `Store` writes the session, and
/// neither writes the other's file.
pub struct SubagentWorkers {
    runner: Arc<forge_subagent::Runner>,
    workspace: String,
}

impl SubagentWorkers {
    pub fn new(runner: Arc<forge_subagent::Runner>, workspace: impl Into<String>) -> Self {
        Self {
            runner,
            workspace: workspace.into(),
        }
    }
}

impl forge_team::WorkerRunner for SubagentWorkers {
    fn run(&self, call: &forge_team::Call<'_>) -> forge_core::Result<forge_team::WorkerResult> {
        // Mail that arrived before the run is part of the prompt, which is what
        // makes a worker's starting state reproducible from its transcript.
        let mut prompt = call.prompt().to_string();
        if let Some(task) = &call.task {
            prompt.push_str(&format!(
                "\n\nYour task on the board ({id}): {subject}",
                id = task.id,
                subject = task.subject
            ));
        }
        if !call.mail.is_empty() {
            prompt.push_str("\n\nMail from the team:\n");
            for message in &call.mail {
                prompt.push_str(&format!("- {}: {}\n", message.from, message.text));
            }
        }

        let ctx = forge_subagent::CallContext {
            cancel: forge_agent::Cancel::default(),
            parent_session_id: String::new(),
            workspace: self.workspace.clone(),
        };
        match self.runner.run(&ctx, &prompt, "", "") {
            Ok(run) => {
                let mut result = if let Some(err) = &run.error {
                    forge_team::WorkerResult::failed(err.message().to_string())
                } else if run.cancelled {
                    forge_team::WorkerResult::failed("the child was cancelled")
                } else {
                    forge_team::WorkerResult::ok(format!(
                        "{} turn(s), session {}",
                        run.turns, run.id
                    ))
                };
                result.text = run.text;
                Ok(result)
            }
            // A setup failure is the runner's problem, not this worker's: the
            // dispatcher records it as a failed worker either way, but the note
            // names who to blame.
            Err(err) => Ok(forge_team::WorkerResult::failed(err.message().to_string())),
        }
    }
}

/// Path of the board root for a data directory, so `forge team` and the tool
/// agree on one place.
pub fn team_root(data_dir: &Path) -> PathBuf {
    data_dir.join("teams")
}

/// Slug sorted expert names, for the CLI listing and the tests. Not a tool
/// concern, but it keeps the sort rule in one place.
pub fn expert_slugs(set: &forge_experts::Set) -> Vec<String> {
    let mut names: Vec<String> = set
        .experts()
        .iter()
        .filter(|expert| expert.enabled)
        .map(|expert| expert.slug.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// A `BTreeMap` of plugin → its tool names, for `forge plugin list`.
pub fn plugin_tool_names(host: &forge_plugin::Host) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (plugin, spec) in host.all_tools() {
        out.entry(plugin).or_default().push(spec.name);
    }
    out
}

/// The empty schema, for callers that would otherwise repeat it.
pub fn empty_schema() -> Value {
    json!({"type":"object","properties":{}})
}
