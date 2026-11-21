//! Bridges the sandbox crate's `Enforcer` onto `hearth-exec`'s `Sandbox` trait.
//!
//! Layer L3. The two traits have the same shape but live in crates that must not
//! know about each other: `hearth-exec` is L1 and deliberately has no sandbox
//! dependency, and `hearth-tool` names only `hearth_exec::Sandbox`. This module
//! is the one place that knows both, so a command started by the `shell` tool or
//! typed into the terminal is confined by the enforcer the host built.
//!
//! It is not optional plumbing. Before this bridge reached the tool set,
//! `--sandbox` reached the policy layer (which decides whether to ask) and the
//! terminal, but every command the `shell` tool started ran with this process's
//! own privileges: the flag named a guarantee the kernel was never given, which
//! is the one thing `hearth-sandbox` exists to prevent.

use std::sync::Arc;

use hearth_core::Result;

/// One enforcer, seen through `hearth-exec`'s trait.
pub struct ExecSandbox {
    enforcer: Arc<dyn hearth_sandbox::Enforcer>,
}

impl ExecSandbox {
    pub fn new(enforcer: Arc<dyn hearth_sandbox::Enforcer>) -> Self {
        Self { enforcer }
    }
}

impl hearth_exec::Sandbox for ExecSandbox {
    fn name(&self) -> &str {
        self.enforcer.name()
    }

    fn apply(&self, command: &mut std::process::Command) -> Result<()> {
        self.enforcer.apply(command)
    }

    fn abort_apply(&self) {
        self.enforcer.abort_apply()
    }

    fn after_start(&self, pid: u32) -> Result<()> {
        self.enforcer.after_start(pid)
    }
}

/// The same enforcer, as the handle the tool layer holds.
pub fn tool_sandbox(enforcer: Arc<dyn hearth_sandbox::Enforcer>) -> Arc<dyn hearth_exec::Sandbox> {
    Arc::new(ExecSandbox::new(enforcer))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Forwards its name and counts applies, so a test can tell that the bridge
    /// called through rather than reporting something of its own.
    struct Fake {
        applies: AtomicUsize,
    }

    impl hearth_sandbox::Enforcer for Fake {
        fn name(&self) -> &str {
            "fake-enforcer"
        }
        fn support(&self) -> hearth_sandbox::Support {
            hearth_sandbox::Support {
                detail: "a test enforcer".to_string(),
                ..hearth_sandbox::Support::default()
            }
        }
        fn apply(&self, _command: &mut std::process::Command) -> Result<()> {
            self.applies.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn close(&self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_bridge_forwards_to_the_enforcer_it_was_given() {
        let fake = Arc::new(Fake {
            applies: AtomicUsize::new(0),
        });
        let enforcer: Arc<dyn hearth_sandbox::Enforcer> = Arc::clone(&fake) as Arc<_>;
        let bridge = tool_sandbox(enforcer);
        assert_eq!(bridge.name(), "fake-enforcer");
        let mut command = std::process::Command::new("no-such-program");
        bridge.apply(&mut command).expect("the fake always agrees");
        bridge.abort_apply();
        assert_eq!(fake.applies.load(Ordering::SeqCst), 1);
    }

    /// The runner writes the raw command line into one variable name and the
    /// enforcer looks for another; both crates spell the name themselves
    /// because neither may depend on the other. This file is the one place that
    /// sees both, so it is where the two spellings are held together.
    #[test]
    fn the_raw_command_line_variable_is_spelled_the_same_on_both_sides() {
        assert_eq!(
            hearth_exec::RAW_CMDLINE_ENV,
            hearth_sandbox::RAW_CMDLINE_ENV
        );
    }
}
