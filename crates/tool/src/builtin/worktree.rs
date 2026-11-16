//! `worktree`: give one turn its own isolated copy of the workspace.
//!
//! The copy is a git worktree, so nothing is copied: `git worktree add` checks
//! the same repository out again at `<workspace>/.forge/worktrees/<name>` on a
//! new branch. That is what makes "edit and build without dirtying the main
//! workspace" cheap enough to do per turn.
//!
//! Every process goes through `forge-exec`, exactly like `shell`: the harness
//! never builds a shell command line, so a worktree name is an argv element and
//! never a fragment of a command string.
//!
//! One convention differs from `shell` on purpose. There, a non-zero exit code is
//! data ("the test suite failed"). Here an action either happened or it did not,
//! so a non-zero exit from git is an error: the model must not read "fatal: not a
//! git repository" as a successful listing with empty output.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use forge_exec::{Command, Pipe, Result as ExecResult, Runner, Sandbox, ERR_SANDBOX, ERR_TIMEOUT};
use forge_llm::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::contract::{decode_args, Call, Result, CODE_IO_ERROR, CODE_TIMEOUT, ERR_BAD_ARGS};
use crate::truncate::{truncate, Limits};
use crate::Tool;

/// git is not installed, or not on the `PATH` the child inherits.
pub const WORKTREE_CODE_GIT_MISSING: &str = "git_missing";
/// git ran and refused the action (a branch that already exists, a path that is
/// not a registered worktree, a lock file, ...).
pub const WORKTREE_CODE_GIT_FAILED: &str = "git_failed";
/// The workspace is not inside a git repository, so no worktree can exist here.
pub const WORKTREE_CODE_NOT_A_REPO: &str = "not_a_git_repo";
/// `git worktree list --porcelain` printed something this tool cannot read.
pub const WORKTREE_CODE_BAD_CONTENT: &str = "bad_content";
/// The target path is already taken.
pub const WORKTREE_CODE_NAME_EXISTS: &str = "already_exists";
/// The sandbox enforcer refused to start git.
pub const WORKTREE_CODE_SANDBOX: &str = "sandbox_refused";

/// The executable is looked up on `PATH`; the minimal child environment keeps
/// `PATH`, so this is the same git a terminal would run.
const GIT_PROGRAM: &str = "git";
/// Fixed location: `<workspace>/.forge/worktrees/<name>`.
const WORKTREE_DIR: [&str; 2] = [".forge", "worktrees"];
/// The name becomes one path component and one branch name. ASCII-only, and
/// short enough for every filesystem this runs on to keep the full path usable.
const MAX_NAME_LEN: usize = 64;
/// `git worktree add` on a large repository is a checkout, not a copy, but a
/// hung git (network filesystem, credential prompt) must still end.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// How much unparseable output an error message quotes back.
const OUTPUT_HEAD_CHARS: usize = 400;
const STDERR_MARKER: &str = "[stderr]";

/// The `worktree` tool.
pub struct WorktreeTool {
    limits: Limits,
    runner: Option<Arc<dyn Runner + Send + Sync>>,
    sandbox: Option<Arc<dyn Sandbox>>,
}

#[derive(Deserialize)]
struct WorktreeArgs {
    action: String,
    #[serde(default)]
    name: String,
}

/// One entry of `git worktree list --porcelain`.
struct WorktreeEntry {
    path: String,
    branch: Option<String>,
    detached: bool,
}

impl WorktreeTool {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            runner: None,
            sandbox: None,
        }
    }

    pub fn with_runner(mut self, runner: Arc<dyn Runner + Send + Sync>) -> Self {
        self.runner = Some(runner);
        self
    }

    pub fn with_sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Zero means "use the default cap" in [`truncate`], so an unconfigured
    /// `Limits` would silently return unbounded output. Resolve it once here.
    fn effective_limits(&self) -> Limits {
        if self.limits.max_bytes <= 0 && self.limits.max_lines <= 0 {
            Limits::default()
        } else {
            self.limits
        }
    }

    /// Shape one result: bounded output, `truncated` set in the field *and* in
    /// meta (the field is what the loop records; the meta key is what the model
    /// reads).
    fn bounded(
        &self,
        is_error: bool,
        code: &str,
        output: String,
        meta: Vec<(&'static str, Value)>,
    ) -> Result {
        let (text, truncated) = truncate(&output, self.effective_limits());
        let mut result = if is_error {
            Result::error(code, text)
        } else {
            Result::ok(text)
        };
        result.truncated = truncated;
        if truncated {
            result = result.with_meta("truncated", json!(true));
        }
        for (key, value) in meta {
            result = result.with_meta(key, value);
        }
        result
    }

    /// Run one git command in the workspace and return its captured result.
    ///
    /// A runner that cannot start the process is a tool error, not data: nothing
    /// was attempted, and the model must be told which of the two it is.
    fn run_git(&self, workspace: &Path, argv: &[&str]) -> std::result::Result<ExecResult, Result> {
        let mut command = Command::new(GIT_PROGRAM)
            .args(argv.iter().copied())
            .dir(workspace.to_string_lossy().into_owned())
            .timeout(GIT_TIMEOUT);
        if let Some(sandbox) = &self.sandbox {
            command = command.sandbox(Arc::clone(sandbox));
        }
        let outcome = match &self.runner {
            Some(runner) => runner.run(command),
            None => Pipe.run_with_cancel(command, &AtomicBool::new(false)),
        };
        outcome.map_err(|error| self.exec_failure(error.message()))
    }

    /// git could not be started at all: no action happened, so this is an error
    /// with the exec layer's own words and nothing else.
    fn exec_failure(&self, message: &str) -> Result {
        if message.contains(ERR_TIMEOUT) {
            return self.bounded(
                true,
                CODE_TIMEOUT,
                format!(
                    "worktree: git did not finish within {} seconds and was stopped, so the action did not complete.\n[hint] a worktree add is a checkout; on a huge repository raise the workspace or run git yourself with the shell tool\n",
                    GIT_TIMEOUT.as_secs()
                ),
                Vec::new(),
            );
        }
        if message.contains(ERR_SANDBOX) {
            return self.bounded(
                true,
                WORKTREE_CODE_SANDBOX,
                format!(
                    "worktree: git was not started: {message}\n[hint] the sandbox enforcer refused the process, so no worktree action happened. Do not retry expecting a different result; fix the sandbox mode or the workspace path\n"
                ),
                Vec::new(),
            );
        }
        let code = if missing_program(message) {
            WORKTREE_CODE_GIT_MISSING
        } else {
            CODE_IO_ERROR
        };
        let hint = if code == WORKTREE_CODE_GIT_MISSING {
            "git is not installed, or not on the PATH this child sees. Nothing changed; install git or do the work another way"
        } else {
            "git could not be started, so nothing changed. Read the message above"
        };
        self.bounded(
            true,
            code,
            format!("worktree: could not run git: {message}\n[hint] {hint}\n"),
            Vec::new(),
        )
    }

    /// One non-zero git exit, reported with git's own output so the model can
    /// tell "not a repository" from "branch already exists" without guessing.
    fn git_failure(
        &self,
        action: &str,
        argv: &[&str],
        executed: &ExecResult,
        extra: &str,
    ) -> Result {
        let code = classify_git_failure(&executed.stdout, &executed.stderr);
        let mut output = format!(
            "worktree: {action} failed: git exited with code {}\ncommand: git {}\n",
            executed.exit_code,
            argv.join(" ")
        );
        let details = git_output_text(executed);
        if details.is_empty() {
            output.push_str("[git] git printed nothing\n");
        } else {
            output.push_str("[git]\n");
            output.push_str(&details);
            output.push('\n');
        }
        if !extra.is_empty() {
            output.push_str(extra);
            output.push('\n');
        }
        output.push_str(failure_hint(action, code));
        self.bounded(
            true,
            code,
            output,
            vec![("exitCode", json!(executed.exit_code))],
        )
    }

    /// Reject output this tool cannot read instead of pretending the list is
    /// empty: "no worktrees" and "I could not parse git" are different answers.
    fn bad_content(&self, message: &str, text: &str) -> Result {
        let head: String = text.chars().take(OUTPUT_HEAD_CHARS).collect();
        self.bounded(
            true,
            WORKTREE_CODE_BAD_CONTENT,
            format!(
                "worktree: cannot read the output of `git worktree list --porcelain`: {message}\nfirst {OUTPUT_HEAD_CHARS} characters:\n{head}\n"
            ),
            Vec::new(),
        )
    }

    fn create(&self, workspace: &Path, name: &str) -> Result {
        let path = worktree_path(workspace, name);
        let path_text = path.to_string_lossy().into_owned();
        if path_exists(&path) {
            return self.bounded(
                true,
                WORKTREE_CODE_NAME_EXISTS,
                format!(
                    "worktree: {name:?} already exists at {path_text}; nothing was created.\n[hint] use that directory, run {{\"action\":\"list\"}} to see the registered worktrees, or pick another name\n"
                ),
                vec![("path", json!(path_text))],
            );
        }

        // The relative path keeps git's printed path and this tool's idea of the
        // location identical: cwd is the workspace.
        let relative = format!(".forge/worktrees/{name}");
        let branch_arg = name.to_string();
        let argv = ["worktree", "add", relative.as_str(), "-b", &branch_arg];
        let executed = match self.run_git(workspace, &argv) {
            Ok(executed) => executed,
            Err(result) => return result,
        };
        if executed.exit_code != 0 {
            return self.git_failure(
                "create",
                &argv,
                &executed,
                &format!("[state] {path_text} was not created by git"),
            );
        }

        // git exited 0. Verify before claiming success: a worktree that git
        // registered elsewhere is not the directory the next call will use.
        if !path_exists(&path) {
            return self.bounded(
                true,
                WORKTREE_CODE_GIT_FAILED,
                format!(
                    "worktree: git reported success for {name:?} but {path_text} does not exist.\n[hint] run {{\"action\":\"list\"}} to see where git actually put it\n"
                ),
                vec![("path", json!(path_text))],
            );
        }

        let mut output = format!("created worktree {name:?}\npath: {path_text}\nbranch: {name}\n");
        let details = git_output_text(&executed);
        if !details.is_empty() {
            output.push('\n');
            output.push_str("[git]\n");
            output.push_str(&details);
            output.push('\n');
        }
        output.push_str(&format!(
            "[hint] pass this path as the workspace of later tool calls to work in isolation; commit on branch {name} and merge it from the main workspace with the shell tool. Remove it with {{\"action\":\"remove\",\"name\":{name:?}}} when done\n"
        ));
        self.bounded(
            false,
            "",
            output,
            vec![
                ("name", json!(name)),
                ("path", json!(path_text)),
                ("branch", json!(name)),
            ],
        )
    }

    fn list(&self, workspace: &Path) -> Result {
        let argv = ["worktree", "list", "--porcelain"];
        let executed = match self.run_git(workspace, &argv) {
            Ok(executed) => executed,
            Err(result) => return result,
        };
        if executed.exit_code != 0 {
            return self.git_failure("list", &argv, &executed, "");
        }

        let text = String::from_utf8_lossy(&executed.stdout).into_owned();
        let entries = match parse_worktree_list(&text) {
            Ok(entries) => entries,
            Err(message) => return self.bad_content(&message, &text),
        };

        let roots = worktree_roots(workspace);
        let total = entries.len();
        let mut lines: Vec<String> = Vec::new();
        for entry in &entries {
            let Some(name) = name_under(&entry.path, &roots) else {
                continue;
            };
            let branch = match &entry.branch {
                Some(branch) => branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
                None if entry.detached => "(detached)".to_string(),
                None => "(no branch)".to_string(),
            };
            lines.push(format!("{name}\t{}\t{branch}", trim_path_text(&entry.path)));
        }
        lines.sort();

        let count = lines.len();
        let root = worktree_root(workspace);
        let output = if lines.is_empty() {
            let mut output = format!("no worktrees under {}\n", root.display());
            if total > 0 {
                output.push_str(&format!(
                    "(git reports {total} worktree(s) elsewhere; only this directory is listed)\n"
                ));
            }
            output
        } else {
            let mut output = lines.join("\n");
            output.push('\n');
            output
        };
        self.bounded(false, "", output, vec![("count", json!(count))])
    }

    fn remove(&self, workspace: &Path, name: &str) -> Result {
        let path = worktree_path(workspace, name);
        let path_text = path.to_string_lossy().into_owned();
        let argv = ["worktree", "remove", "--force", path_text.as_str()];
        let executed = match self.run_git(workspace, &argv) {
            Ok(executed) => executed,
            Err(result) => return result,
        };
        if executed.exit_code != 0 {
            let extra = if path_exists(&path) {
                format!("[state] {path_text} still exists; this call did not delete it")
            } else {
                format!("[state] {path_text} does not exist on disk")
            };
            return self.git_failure("remove", &argv, &executed, &extra);
        }

        // git exited 0. `worktree remove` is supposed to take the directory with
        // it; if the directory survived (a lock, a file in use, a path git did
        // not manage), say so and at least finish the deletion the user asked
        // for. Claiming success while the directory is still there is the one
        // thing this tool must not do.
        let mut remedy = String::new();
        if path_exists(&path) {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    remedy.push_str(&format!(
                        "[note] git exited 0 but left {path_text} behind; the leftover directory was deleted by this tool\n"
                    ));
                }
                Err(err) => {
                    return self.bounded(
                        true,
                        WORKTREE_CODE_GIT_FAILED,
                        format!(
                            "worktree: git reported {name:?} removed, but {path_text} still exists and could not be deleted: {err}\n[hint] a file in it is probably open or locked. Close whatever holds it and retry, or delete the directory by hand\n"
                        ),
                        vec![("path", json!(path_text))],
                    );
                }
            }
        }

        let mut output = format!("removed worktree {name:?}\npath: {path_text}\n");
        if !remedy.is_empty() {
            output.push_str(&remedy);
        }
        let details = git_output_text(&executed);
        if !details.is_empty() {
            output.push('\n');
            output.push_str("[git]\n");
            output.push_str(&details);
            output.push('\n');
        }
        self.bounded(false, "", output, vec![("path", json!(path_text))])
    }
}

impl Tool for WorktreeTool {
    fn spec(&self) -> ToolSpec {
        let containment = match &self.sandbox {
            Some(sandbox) => format!(
                "Process: git is submitted to the {} enforcer before it starts, so the sandbox mode decides what the git process may read or write. If the enforcer refuses it, no worktree action happens and the call reports that refusal.",
                sandbox.name()
            ),
            None => "Process: git runs with the user's own privileges and can read or write any repository the user can reach. Nothing here confines it.".to_string(),
        };
        let description = format!(
            "Create, list and remove isolated copies of the workspace, one git worktree per turn.\n\n\
When to use: a turn needs to edit, build or experiment without dirtying the main workspace, or two turns must not see each other's uncommitted changes. The copy is a git worktree, so no files are copied: it is a second checkout of the same repository on its own branch, at <workspace>/.forge/worktrees/<name>. Commits made there are ordinary commits of that repository. Use it for \"change this code but leave my working tree alone\"; do not use it as a backup or to copy files.\n\n\
When not to use: the workspace is not a git repository (the call fails and says so), or the job is one file that fs_read/fs_edit can handle in place. A worktree carries committed content only: untracked files are not there, nothing is stashed, and results are not merged back. The branch is yours to merge from the main workspace with the shell tool.\n\n\
Actions: create (needs name) runs `git worktree add .forge/worktrees/<name> -b <name>` in the workspace and returns the absolute path and the branch. list runs `git worktree list --porcelain` and prints only the worktrees under that directory, one line each: name<TAB>path<TAB>branch; a repository with none prints that fact instead of an empty answer. remove (needs name) runs `git worktree remove --force <path>`, which discards uncommitted changes in that worktree, so commit or copy anything you want to keep first.\n\n\
Name: required for create and remove, at most 64 characters from A-Z a-z 0-9 . _ - ; it becomes both a directory name and a branch name, so \".\" and \"..\" and anything with a path separator are rejected before git runs.\n\n\
Failures: unlike the shell tool, there is no \"the command ran and returned non-zero\" case here: an action either happened or it did not, so every failure is is_error. A missing git, a workspace that is not a repository, an unknown action, a taken name and any non-zero git exit all include git's own message; read it before retrying.\n\n\
{containment}\n\n\
Example arguments:\n{{\"action\":\"create\",\"name\":\"turn-1\"}}\n{{\"action\":\"list\"}}\n{{\"action\":\"remove\",\"name\":\"turn-1\"}}"
        );
        ToolSpec {
            name: "worktree".to_string(),
            description,
            schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "list", "remove"],
                        "description": "What to do: create a worktree, list the ones under <workspace>/.forge/worktrees/, or remove one."
                    },
                    "name": {
                        "type": "string",
                        "description": "Worktree name for create and remove: 1-64 characters from A-Z a-z 0-9 . _ - , not \".\" or \"..\". It is both the directory name and the branch name."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: WorktreeArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        let workspace = match workspace_dir(&call) {
            Ok(workspace) => workspace,
            Err(result) => return result,
        };
        match args.action.trim().to_ascii_lowercase().as_str() {
            "create" | "remove" => {
                // Validation happens before any path is built or any process
                // starts: a rejected name never reaches git as an argument.
                if let Err(reason) = validate_name(&args.name) {
                    return Result::error(
                        ERR_BAD_ARGS,
                        format!(
                            "worktree: {reason}; nothing was run. A name is one path component and one branch name: up to {MAX_NAME_LEN} characters from A-Z a-z 0-9 . _ - , for example \"turn-1\""
                        ),
                    );
                }
                if args.action.trim().eq_ignore_ascii_case("create") {
                    self.create(&workspace, &args.name)
                } else {
                    self.remove(&workspace, &args.name)
                }
            }
            "list" => self.list(&workspace),
            other => Result::error(
                ERR_BAD_ARGS,
                format!(
                    "worktree: unknown action {other:?}; use \"create\", \"list\" or \"remove\" (for example {{\"action\":\"list\"}})"
                ),
            ),
        }
    }
}

/// The absolute workspace of this call. An empty workspace falls back to the
/// harness's own directory, which is what the loop means by "no workspace".
fn workspace_dir(call: &Call) -> std::result::Result<PathBuf, Result> {
    let raw = if call.workspace.trim().is_empty() {
        std::env::current_dir().map_err(|err| {
            Result::error(
                CODE_IO_ERROR,
                format!("worktree: the call has no workspace and the current directory is unavailable: {err}"),
            )
        })?
    } else {
        PathBuf::from(&call.workspace)
    };
    std::path::absolute(&raw).map_err(|err| {
        Result::error(
            CODE_IO_ERROR,
            format!(
                "worktree: cannot resolve the workspace path {}: {err}",
                raw.display()
            ),
        )
    })
}

/// The one place a name becomes a path member. The caller has already validated
/// the name, so this cannot escape the worktree root.
fn worktree_path(workspace: &Path, name: &str) -> PathBuf {
    worktree_root(workspace).join(name)
}

fn worktree_root(workspace: &Path) -> PathBuf {
    let mut root = workspace.to_path_buf();
    for part in WORKTREE_DIR {
        root.push(part);
    }
    root
}

/// A name is exactly one path component and exactly one branch name. Anything
/// else is refused here, so it is never joined into a path or handed to git.
fn validate_name(raw: &str) -> std::result::Result<(), String> {
    if raw.is_empty() {
        return Err("the name is empty".to_string());
    }
    if raw.len() > MAX_NAME_LEN {
        return Err(format!(
            "the name is {} bytes, above the {MAX_NAME_LEN}-byte maximum",
            raw.len()
        ));
    }
    if raw == "." || raw == ".." {
        return Err(format!(
            "the name {raw:?} is a directory reference, not a worktree name"
        ));
    }
    for character in raw.chars() {
        if !(character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')) {
            return Err(format!(
                "the name {raw:?} contains {character:?}; only ASCII letters, digits, '.', '_' and '-' are allowed"
            ));
        }
    }
    Ok(())
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Read `git worktree list --porcelain`.
///
/// The format is one attribute per line, entries separated by a blank line, and
/// the first attribute of an entry is `worktree <path>`. Parsing is line-based
/// so an unknown attribute is reported rather than silently dropped: an empty
/// result from a failed parse would look exactly like "there are no worktrees".
fn parse_worktree_list(text: &str) -> std::result::Result<Vec<WorktreeEntry>, String> {
    if text.trim().is_empty() {
        return Err("the output was empty, so no worktree entry can be read".to_string());
    }
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            let path = path.trim();
            if path.is_empty() {
                return Err(format!("an entry has an empty worktree path ({line:?})"));
            }
            entries.push(WorktreeEntry {
                path: path.to_string(),
                branch: None,
                detached: false,
            });
            continue;
        }
        let Some(current) = entries.last_mut() else {
            return Err(format!(
                "the first non-empty line is not a \"worktree\" line ({line:?})"
            ));
        };
        if let Some(branch) = line.strip_prefix("branch ") {
            let branch = branch.trim();
            if branch.is_empty() {
                return Err(format!("an entry has an empty branch line ({line:?})"));
            }
            current.branch = Some(branch.to_string());
        } else if line == "detached" {
            current.detached = true;
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            if head.trim().is_empty() {
                return Err(format!("an entry has an empty HEAD line ({line:?})"));
            }
        } else if line == "bare" || line.starts_with("locked") || line.starts_with("prunable") {
            // Git bookkeeping that does not change name, path or branch.
        } else {
            return Err(format!("unrecognised line {line:?}"));
        }
    }
    Ok(entries)
}

/// Normalise a path as printed by git (or held by us) for comparison: forward
/// slashes, no trailing separator, no Windows verbatim prefix.
fn trim_path_text(text: &str) -> String {
    let mut out = text.replace('\\', "/");
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    if let Some(rest) = out.strip_prefix("//?/UNC/") {
        out = format!("//{rest}");
    } else if let Some(rest) = out.strip_prefix("//?/") {
        out = rest.to_string();
    }
    out
}

/// Windows path comparison is case-insensitive; every other platform is not.
fn fold_case(text: &str) -> String {
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// Every spelling of `<workspace>/.forge/worktrees` that git might print: the
/// path as given, and its canonical form (git reports resolved paths, so a
/// symlinked workspace or a short name would otherwise never match).
fn worktree_roots(workspace: &Path) -> Vec<String> {
    let mut bases = vec![workspace.to_path_buf()];
    if let Ok(canonical) = std::fs::canonicalize(workspace) {
        bases.push(canonical);
    }
    let mut roots: Vec<String> = Vec::new();
    for base in bases {
        let root = fold_case(&trim_path_text(&worktree_root(&base).to_string_lossy()));
        if !root.is_empty() && !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

/// The worktree name for a `git worktree list` path, or `None` when the path is
/// not one direct child of the worktree root (the main checkout lands here, and
/// so does anything a user created by hand).
fn name_under(path: &str, roots: &[String]) -> Option<String> {
    let raw = trim_path_text(path);
    let name = raw.rsplit('/').next()?;
    if name.is_empty() {
        return None;
    }
    let folded = fold_case(&raw);
    let folded_name = fold_case(name);
    for root in roots {
        if folded == format!("{root}/{folded_name}") {
            return Some(name.to_string());
        }
    }
    None
}

fn git_output_text(executed: &ExecResult) -> String {
    let mut out = String::new();
    let stdout = String::from_utf8_lossy(&executed.stdout);
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        out.push_str(stdout);
    }
    let stderr = String::from_utf8_lossy(&executed.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(STDERR_MARKER);
        out.push('\n');
        out.push_str(stderr);
    }
    out
}

/// Which stable code one failed git run gets. Matching on git's English text is
/// the only signal available; when it does not match, the generic code is still
/// honest and the raw output is in the message either way.
fn classify_git_failure(stdout: &[u8], stderr: &[u8]) -> &'static str {
    let mut text = String::from_utf8_lossy(stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(stdout));
    if text.to_lowercase().contains("not a git repository") {
        WORKTREE_CODE_NOT_A_REPO
    } else {
        WORKTREE_CODE_GIT_FAILED
    }
}

fn missing_program(message: &str) -> bool {
    let lower = message.to_lowercase();
    // std formats a failed spawn as "<os text> (os error 2)" on Windows; the OS
    // text is localised, the "os error 2" suffix is not.
    lower.contains("os error 2")
        || lower.contains("no such file")
        || lower.contains("program not found")
        || lower.contains("not recognized")
        || lower.contains("cannot find")
        || lower.contains("not found")
}

fn failure_hint(action: &str, code: &str) -> &'static str {
    if code == WORKTREE_CODE_NOT_A_REPO {
        return "[hint] the workspace is not a git repository, and this tool only works inside one. Run git init in it (or pass a workspace that is inside a repository) and retry\n";
    }
    match action {
        "create" => {
            "[hint] no worktree was created by this call. Run {\"action\":\"list\"} to see what is registered: a branch with the same name, a leftover directory, or a lock is the usual cause\n"
        }
        "remove" => {
            "[hint] the worktree was not removed. Run {\"action\":\"list\"} to see whether it is still registered, then retry with the exact name it lists\n"
        }
        _ => {
            "[hint] run {\"action\":\"list\"} to see what this workspace can offer; if that fails too, the workspace is not a repository git can read\n"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tool() -> WorktreeTool {
        WorktreeTool::new(Limits::default())
    }

    fn call(workspace: &Path, args: Value) -> Call {
        Call {
            id: "c1".to_string(),
            name: "worktree".to_string(),
            args,
            workspace: workspace.to_string_lossy().into_owned(),
        }
    }

    /// A fresh empty directory under the system temp directory.
    fn scratch(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let tail = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "forge-worktree-{tag}-{}-{nanos}-{tail}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch directory");
        dir
    }

    /// `git worktree list` prints the resolved long form of a path, while
    /// `std::env::temp_dir()` on Windows may hand out the 8.3 short form
    /// (`ADMINI~1`), so plain string equality is not the right comparison.
    fn same_dir(listed: &str, expected: &Path) -> bool {
        let listed = PathBuf::from(listed);
        let resolve = |path: &Path| {
            std::fs::canonicalize(path)
                .map(|resolved| fold_case(&trim_path_text(&resolved.to_string_lossy())))
                .unwrap_or_else(|_| fold_case(&trim_path_text(&path.to_string_lossy())))
        };
        resolve(&listed) == resolve(expected)
    }

    fn git_works() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn git(workspace: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository with one commit: `git worktree add -b` needs a commit-ish.
    fn init_repo(workspace: &Path) {
        git(workspace, &["init", "-q"]);
        git(
            workspace,
            &["config", "user.email", "worktree@example.invalid"],
        );
        git(workspace, &["config", "user.name", "worktree test"]);
        git(workspace, &["config", "commit.gpgsign", "false"]);
        std::fs::write(workspace.join("README.md"), "hello\n").expect("write README");
        git(workspace, &["add", "README.md"]);
        git(workspace, &["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn validate_name_rejects_every_escape_and_separator() {
        assert!(validate_name("turn-1_a.b").is_ok());
        assert!(validate_name("T").is_ok());
        assert!(validate_name("0123456789").is_ok());

        let longest = "x".repeat(MAX_NAME_LEN);
        assert!(validate_name(&longest).is_ok());
        let too_long = "x".repeat(MAX_NAME_LEN + 1);

        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a b",
            " turn",
            "turn ",
            "café",
            "a:b",
            "a*b",
            too_long.as_str(),
        ] {
            let error = validate_name(bad).expect_err("name should be rejected");
            assert!(!error.is_empty(), "rejection of {bad:?} needs a reason");
        }
    }

    #[test]
    fn bad_names_fail_the_call_before_any_git_run() {
        let workspace = scratch("bad-names");
        let too_long = "x".repeat(MAX_NAME_LEN + 1);
        for name in ["", "..", "a/b", "a\\b", "a b", too_long.as_str()] {
            let result = tool().call(call(&workspace, json!({"action":"create","name":name})));
            assert!(result.is_error, "name {name:?} must be refused");
            assert_eq!(
                result.code, ERR_BAD_ARGS,
                "name {name:?}: {}",
                result.output
            );
            assert!(
                result.output.contains("nothing was run"),
                "name {name:?}: {}",
                result.output
            );
        }
        // remove validates through the same path
        let result = tool().call(call(
            &workspace,
            json!({"action":"remove","name":"../boom"}),
        ));
        assert_eq!(result.code, ERR_BAD_ARGS);
        assert!(result.is_error);
        // and an unknown action is a bad argument, not a git failure
        let result = tool().call(call(&workspace, json!({"action":"destroy","name":"x"})));
        assert_eq!(result.code, ERR_BAD_ARGS);
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn spec_names_the_tool_and_its_actions() {
        let spec = tool().spec();
        assert_eq!(spec.name, "worktree");
        assert_eq!(spec.schema["type"], json!("object"));
        assert_eq!(
            spec.schema["properties"]["action"]["enum"],
            json!(["create", "list", "remove"])
        );
        assert_eq!(spec.schema["properties"]["name"]["type"], json!("string"));
        assert_eq!(spec.schema["required"], json!(["action"]));
        assert_eq!(spec.schema["additionalProperties"], json!(false));

        let description = &spec.description;
        assert!(description.contains(".forge/worktrees/"), "{description}");
        assert!(description.contains("git worktree add"), "{description}");
        assert!(
            description.contains("git worktree list --porcelain"),
            "{description}"
        );
        assert!(description.contains("Example arguments"), "{description}");
        assert!(
            description.contains("{\"action\":\"create\",\"name\":\"turn-1\"}"),
            "{description}"
        );
        assert!(description.contains("is_error"), "{description}");
    }

    #[test]
    fn create_list_remove_roundtrip() {
        if !git_works() {
            println!("skipping create_list_remove_roundtrip: git is not available");
            return;
        }
        let workspace = scratch("roundtrip");
        init_repo(&workspace);
        let worktree = workspace.join(".forge").join("worktrees").join("wt-one");

        let created = tool().call(call(&workspace, json!({"action":"create","name":"wt-one"})));
        assert!(!created.is_error, "create failed: {}", created.output);
        assert!(worktree.is_dir(), "{} should exist", worktree.display());
        assert!(
            worktree.join(".git").exists(),
            "the worktree should carry a .git link"
        );
        assert!(
            created
                .output
                .contains(&worktree.to_string_lossy().into_owned()),
            "create should name the path: {}",
            created.output
        );
        assert_eq!(created.meta.get("name"), Some(&json!("wt-one")));
        assert_eq!(created.meta.get("branch"), Some(&json!("wt-one")));

        let listed = tool().call(call(&workspace, json!({"action":"list"})));
        assert!(!listed.is_error, "list failed: {}", listed.output);
        let rows: Vec<&str> = listed
            .output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(rows.len(), 1, "one entry expected, got: {}", listed.output);
        let columns: Vec<&str> = rows[0].split('\t').collect();
        assert_eq!(columns.len(), 3, "row: {:?}", rows[0]);
        assert_eq!(columns[0], "wt-one");
        assert!(
            same_dir(columns[1], &worktree),
            "listed path {:?} should be {}",
            columns[1],
            worktree.display()
        );
        assert_eq!(columns[2], "wt-one");
        assert_eq!(listed.meta.get("count"), Some(&json!(1)));

        // The same name cannot be created twice.
        let again = tool().call(call(&workspace, json!({"action":"create","name":"wt-one"})));
        assert!(again.is_error);
        assert_eq!(again.code, WORKTREE_CODE_NAME_EXISTS, "{}", again.output);

        let removed = tool().call(call(&workspace, json!({"action":"remove","name":"wt-one"})));
        assert!(!removed.is_error, "remove failed: {}", removed.output);
        assert!(!worktree.exists(), "{} should be gone", worktree.display());

        let listed = tool().call(call(&workspace, json!({"action":"list"})));
        assert!(!listed.is_error, "list failed: {}", listed.output);
        assert!(
            !listed.output.contains("wt-one"),
            "the removed worktree must not be listed: {}",
            listed.output
        );
        assert_eq!(listed.meta.get("count"), Some(&json!(0)));

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn every_answer_is_bounded() {
        if !git_works() {
            println!("skipping every_answer_is_bounded: git is not available");
            return;
        }
        let workspace = scratch("bounded");
        init_repo(&workspace);
        let created = tool().call(call(&workspace, json!({"action":"create","name":"tight"})));
        assert!(!created.is_error, "create failed: {}", created.output);

        let tight = WorktreeTool::new(Limits {
            max_bytes: 16,
            max_lines: -1,
        });
        let listed = tight.call(call(&workspace, json!({"action":"list"})));
        assert!(!listed.is_error, "list failed: {}", listed.output);
        assert!(
            listed.truncated,
            "list output should be cut: {}",
            listed.output
        );
        assert_eq!(listed.meta.get("truncated"), Some(&json!(true)));

        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn workspace_with_spaces_and_non_ascii_roundtrip() {
        if !git_works() {
            println!(
                "skipping workspace_with_spaces_and_non_ascii_roundtrip: git is not available"
            );
            return;
        }
        // `git worktree list --porcelain` prints such a path unquoted, as the
        // rest of the "worktree " line, so the parser must keep spaces and
        // non-ASCII bytes instead of splitting on whitespace.
        let workspace = scratch("spaces").join("工作 目录");
        std::fs::create_dir_all(&workspace).expect("create non-ASCII scratch");
        init_repo(&workspace);

        let created = tool().call(call(&workspace, json!({"action":"create","name":"cn-one"})));
        assert!(!created.is_error, "create failed: {}", created.output);

        let listed = tool().call(call(&workspace, json!({"action":"list"})));
        assert!(!listed.is_error, "list failed: {}", listed.output);
        let rows: Vec<&str> = listed
            .output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(rows.len(), 1, "one entry expected: {}", listed.output);
        let columns: Vec<&str> = rows[0].split('\t').collect();
        assert_eq!(columns[0], "cn-one", "row: {:?}", rows[0]);
        assert!(
            same_dir(
                columns[1],
                &workspace.join(".forge").join("worktrees").join("cn-one")
            ),
            "listed path {:?} lost its spaces or non-ASCII bytes",
            columns[1]
        );

        let removed = tool().call(call(&workspace, json!({"action":"remove","name":"cn-one"})));
        assert!(!removed.is_error, "remove failed: {}", removed.output);
        assert!(!workspace
            .join(".forge")
            .join("worktrees")
            .join("cn-one")
            .exists());

        let _ = std::fs::remove_dir_all(workspace.parent().unwrap_or(&workspace));
    }

    #[test]
    fn workspace_without_a_repository_reports_git_itself() {
        if !git_works() {
            println!(
                "skipping workspace_without_a_repository_reports_git_itself: git is not available"
            );
            return;
        }
        let workspace = scratch("no-repo");
        // If the temp directory happens to live inside a repository, git walks
        // up and finds it; the case cannot be constructed then.
        let inside_repo = std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&workspace)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if inside_repo {
            println!("skipping: the temp directory is inside a git repository");
            let _ = std::fs::remove_dir_all(&workspace);
            return;
        }

        let result = tool().call(call(&workspace, json!({"action":"list"})));
        assert!(
            result.is_error,
            "a non-repository must fail: {}",
            result.output
        );
        // git's own words, not a paraphrase: "fatal: not a git repository ..."
        assert!(
            result.output.contains("fatal:") || result.output.contains("not a git repository"),
            "git's output must be in the message: {}",
            result.output
        );
        assert!(
            matches!(
                result.code.as_str(),
                WORKTREE_CODE_NOT_A_REPO | WORKTREE_CODE_GIT_FAILED
            ),
            "unexpected code: {}",
            result.code
        );

        let created = tool().call(call(&workspace, json!({"action":"create","name":"nope"})));
        assert!(created.is_error, "create must fail outside a repo");
        assert!(
            !workspace.join(".forge").exists(),
            "nothing should be left behind"
        );

        let _ = std::fs::remove_dir_all(&workspace);
    }
}
