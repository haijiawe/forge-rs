//! The built-in tools and their small helpers.
//!
//! Ten of them are registered in every turn; the rest depend on what the turn
//! can actually do — network access, a data directory, a process runner, an open
//! goal. A missing capability is a missing tool rather than a tool that fails at
//! call time, so the tool list is a truthful answer to "what can this turn do".

pub mod danger;
pub mod fs;
pub mod goal;
pub mod image;
pub mod lines;
pub mod present;
pub mod shell;
pub mod todo;
pub mod web;
pub mod worktree;

use std::path::PathBuf;
use std::sync::Arc;

use hearth_exec::{Runner, Sandbox};

use crate::truncate::Limits;
use crate::Tool;

pub use danger::{dangerous, Dangerous};
pub use fs::{FsEdit, FsGlob, FsGrep, FsRead, FsWrite, CODE_AMBIGUOUS, CODE_BINARY};
pub use goal::{GoalEndTool, GOAL_META_KEY};
pub use image::{ReadImageTool, ALLOWED_MEDIA_TYPES, MAX_IMAGE_BYTES};
pub use present::PresentTool;
pub use shell::{
    shell_invocation, ShellTool, SHELL_CODE_BLOCKED, SHELL_CODE_EMPTY_COMMAND,
    SHELL_CODE_EXEC_FAILED, SHELL_CODE_SANDBOX,
};
pub use todo::TodoWriteTool;
pub use web::{SearchConfig, WebFetchTool, WebSearchTool, WEB_CODE_BAD_CONTENT};
pub use worktree::WorktreeTool;

/// Everything the builtin set needs that is not a limit.
///
/// Optional fields are capabilities: `None` means the corresponding tool is not
/// registered at all, which is how "no network configured" stays visible in the
/// tool list instead of becoming a runtime failure the model has to discover.
#[derive(Default)]
pub struct Options {
    pub limits: Limits,
    /// Network client. Without it, `web_fetch` and `web_search` are absent.
    pub net: Option<Arc<hearth_net::Client>>,
    /// Search endpoint and key. Both must be present for `web_search`.
    pub search: Option<SearchConfig>,
    /// Where `present` writes its manifest.
    pub data_dir: PathBuf,
    /// Process runner and sandbox for tools that shell out (`shell`, `worktree`).
    pub runner: Option<Arc<dyn Runner + Send + Sync>>,
    pub sandbox: Option<Arc<dyn Sandbox>>,
    /// Whether this turn belongs to an open goal. `goal_end` exists only then:
    /// a tool whose only effect is to be read by the loop that opened the round
    /// has nothing to say in a turn nobody is looping over, and offering it
    /// would let the model believe a sentence it wrote stopped something.
    pub goal: bool,
}

impl Options {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }
}

/// Construct the builtin set for these options.
pub fn all(options: &Options) -> Vec<Arc<dyn Tool>> {
    let limits = options.limits;
    // The shell tool starts child processes, so it is the tool a sandbox mode is
    // about. Handing it the enforcer here is what makes `--sandbox` describe the
    // kernel rather than the policy layer: without it the mode only decided which
    // calls were asked about, and an approved command ran unrestricted.
    let mut shell = ShellTool::new(limits);
    if let Some(sandbox) = &options.sandbox {
        shell = shell.with_sandbox(Arc::clone(sandbox));
    }
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FsRead),
        Arc::new(FsWrite),
        Arc::new(FsEdit),
        Arc::new(FsGlob),
        Arc::new(FsGrep),
        Arc::new(ReadImageTool),
        Arc::new(shell),
        Arc::new(TodoWriteTool::new(limits)),
    ];

    // Only while a loop is open. See `Options::goal`.
    if options.goal {
        tools.push(Arc::new(GoalEndTool::new(limits)));
    }

    if let Some(client) = &options.net {
        tools.push(Arc::new(WebFetchTool::new(limits, Arc::clone(client))));
        if let Some(search) = &options.search {
            tools.push(Arc::new(WebSearchTool::new(
                limits,
                Arc::clone(client),
                search.clone(),
            )));
        }
    }

    // Registered unconditionally: with no data directory the tool falls back to
    // `<workspace>/.hearth/presented.jsonl` and says so, which is a visible
    // degradation. An absent tool would be a silent one.
    tools.push(Arc::new(PresentTool::new(limits, options.data_dir.clone())));

    let mut worktree = WorktreeTool::new(limits);
    if let Some(runner) = &options.runner {
        worktree = worktree.with_runner(Arc::clone(runner));
    }
    if let Some(sandbox) = &options.sandbox {
        worktree = worktree.with_sandbox(Arc::clone(sandbox));
    }
    tools.push(Arc::new(worktree));

    tools
}

/// The names in the tool list, sorted, for tests and documentation.
pub fn names(options: &Options) -> Vec<String> {
    all(options)
        .iter()
        .map(|tool| tool.spec().name)
        .collect::<Vec<_>>()
        .tap_sorted()
}

trait Sorted {
    fn tap_sorted(self) -> Self;
}

impl Sorted for Vec<String> {
    fn tap_sorted(mut self) -> Self {
        self.sort();
        self
    }
}
