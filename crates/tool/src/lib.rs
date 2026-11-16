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
    all, dangerous, Dangerous, FsEdit, FsGlob, FsGrep, FsRead, FsWrite, ShellTool, TodoWriteTool,
    CODE_AMBIGUOUS, CODE_BINARY, SHELL_CODE_BLOCKED, SHELL_CODE_EMPTY_COMMAND,
    SHELL_CODE_EXEC_FAILED, SHELL_CODE_SANDBOX,
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
    fn all_builtins_have_unique_sorted_specs() {
        let registry = Registry::new(all(DefaultLimits));
        let names = registry.names();
        assert_eq!(
            names,
            vec![
                "fs_edit",
                "fs_glob",
                "fs_grep",
                "fs_read",
                "fs_write",
                "shell",
                "todo_write"
            ]
        );
    }
}
