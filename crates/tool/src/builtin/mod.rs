//! The seven built-in tools and their small helpers.

pub mod danger;
pub mod fs;
pub mod shell;
pub mod todo;

use std::sync::Arc;

use crate::truncate::Limits;
use crate::Tool;

pub use danger::{dangerous, Dangerous};
pub use fs::{FsEdit, FsGlob, FsGrep, FsRead, FsWrite, CODE_AMBIGUOUS, CODE_BINARY};
pub use shell::{
    ShellTool, SHELL_CODE_BLOCKED, SHELL_CODE_EMPTY_COMMAND, SHELL_CODE_EXEC_FAILED,
    SHELL_CODE_SANDBOX,
};
pub use todo::TodoWriteTool;

/// Construct all seven builtins, each with the same output limits.
pub fn all(limits: Limits) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(FsRead),
        Arc::new(FsWrite),
        Arc::new(FsEdit),
        Arc::new(FsGlob),
        Arc::new(FsGrep),
        Arc::new(ShellTool::new(limits)),
        Arc::new(TodoWriteTool::new(limits)),
    ]
}
