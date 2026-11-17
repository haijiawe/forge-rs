//! Registry, executor, policy, approvals and the seven built-in tools.
//!
//! Layer L2. The crate depends only on `forge-core`, `forge-exec`, `forge-llm`,
//! `serde` and `serde_json`. It writes no events: the loop owns the session log.

#![forbid(unsafe_code)]

pub mod approval;
pub mod builtin;
pub mod contract;
pub mod executor;
pub mod path;
pub mod policy;
pub mod registry;
pub mod truncate;

pub use approval::{
    AllowAll, ApprovalRequest, Approver, Decision, DecisionRecorder, Policy, PolicyFunc,
};
pub use builtin::{
    all, dangerous, names, Dangerous, FsEdit, FsGlob, FsGrep, FsRead, FsWrite, GoalEndTool,
    Options, PresentTool, SearchConfig, ShellTool, TodoWriteTool, WebFetchTool, WebSearchTool,
    WorktreeTool, CODE_AMBIGUOUS, CODE_BINARY, GOAL_META_KEY, SHELL_CODE_BLOCKED,
    SHELL_CODE_EMPTY_COMMAND, SHELL_CODE_EXEC_FAILED, SHELL_CODE_SANDBOX, WEB_CODE_BAD_CONTENT,
};
pub use contract::{
    decode_args, Call, Meta, Result, Tool, CODE_CANCELLED, CODE_DENIED, CODE_IO_ERROR,
    CODE_NOT_FOUND, CODE_OUTSIDE_WORKSPACE, CODE_TIMEOUT, ERR_BAD_ARGS,
};
pub use executor::{
    Executor, ExecutorOptions, CODE_APPROVAL_FAILED, CODE_DENIED_REFUSED, CODE_NO_APPROVER,
    CODE_PANIC, CODE_UNKNOWN_TOOL,
};
pub use path::{is_outside, rel, resolve, PathError};
pub use policy::{
    approval_request, default_rules, merge, Rule, Rules, ACTION_ALLOW, ACTION_ASK, ACTION_DENY,
};
pub use registry::Registry;
pub use truncate::{
    effective_limits, truncate, DefaultLimits, Limits, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

/// A deliberately local shape mirroring the agent crate's `ToolRunner`. The
/// agent is written concurrently and this crate must not depend on it; a small
/// adapter test below proves `Executor` satisfies the same specs/run contract.
#[cfg(test)]
trait ToolRunner {
    fn specs(&self) -> Vec<forge_llm::ToolSpec>;
    fn run(&self, call: Call) -> Result;
}

#[cfg(test)]
impl ToolRunner for Executor {
    fn specs(&self) -> Vec<forge_llm::ToolSpec> {
        Executor::specs(self)
    }

    fn run(&self, call: Call) -> Result {
        Executor::run(self, call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    struct Echo;

    impl Tool for Echo {
        fn spec(&self) -> forge_llm::ToolSpec {
            forge_llm::ToolSpec {
                name: "echo".into(),
                description: "Echo the text argument".into(),
                schema: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            }
        }

        fn call(&self, call: Call) -> Result {
            Result::ok(
                call.args
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
            )
        }
    }

    #[test]
    fn registry_satisfies_generic_specs_and_run_shape() {
        let registry = Registry::new(vec![Arc::new(Echo) as Arc<dyn Tool>]);
        let executor = Executor::from_registry(registry, ExecutorOptions::default());
        let runner: &dyn ToolRunner = &executor;
        assert_eq!(runner.specs().len(), 1);
        let result = runner.run(Call::new("c1", "echo", json!({"text":"hello"}), "."));
        assert_eq!(result.output, "hello");
    }

    #[test]
    fn every_turn_gets_the_same_eight_tools_and_goals_add_a_ninth() {
        // "A turn" means no goal, no network, no process runner. The list is
        // sorted so a reviewer can diff it against the document.
        let registry = Registry::new(all(&Options::new(DefaultLimits)));
        assert_eq!(
            registry.names(),
            vec![
                "fs_edit",
                "fs_glob",
                "fs_grep",
                "fs_read",
                "fs_write",
                "present",
                "shell",
                "todo_write",
                "worktree"
            ]
        );

        let mut options = Options::new(DefaultLimits);
        options.goal = true;
        assert_eq!(
            Registry::new(all(&options)).names(),
            vec![
                "fs_edit",
                "fs_glob",
                "fs_grep",
                "fs_read",
                "fs_write",
                "goal_end",
                "present",
                "shell",
                "todo_write",
                "worktree"
            ]
        );
    }

    /// `goal_end` is the model's half of a conversation with the turn loop. With
    /// no loop there is nobody to hear it, so the tool is not offered — a tool
    /// that cannot have its effect must not be able to claim it did.
    #[test]
    fn goal_end_is_absent_without_an_open_goal() {
        let options = Options::new(DefaultLimits);
        assert!(!names(&options).contains(&"goal_end".to_string()));
        assert!(!Registry::new(all(&options))
            .names()
            .contains(&"goal_end".to_string()));
    }

    /// The platform tools appear only when their capability is configured, so a
    /// caller can read the tool list and know what this turn can actually do.
    #[test]
    fn configured_capabilities_add_tools_rather_than_enabling_them() {
        // `present` is registered even with no data directory: it degrades to
        // the workspace and reports that, which is better than disappearing.
        let options = Options::new(DefaultLimits);
        let bare = names(&options);
        assert!(bare.contains(&"present".to_string()));
        assert!(!bare.contains(&"web_fetch".to_string()));

        let mut options = Options::new(DefaultLimits);
        let client = std::sync::Arc::new(
            forge_net::Client::new(forge_net::ClientOptions::default()).expect("client"),
        );
        options.net = Some(std::sync::Arc::clone(&client));
        let with_fetch = names(&options);
        assert!(with_fetch.contains(&"web_fetch".to_string()));
        assert!(!with_fetch.contains(&"web_search".to_string()));

        options.search = Some(SearchConfig {
            endpoint: "https://example.invalid/search".to_string(),
            key: String::new(),
        });
        let with_search = names(&options);
        assert!(with_search.contains(&"web_search".to_string()));
    }
}
