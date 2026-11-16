//! Fallback for targets that are neither Windows nor Unix.
//!
//! There is no process-tree primitive here, so only the direct child can be
//! killed. Callers on such platforms must not claim tree guarantees; this
//! build is compile-only and the harness does not ship for it.

pub(crate) fn configure(_command: &mut std::process::Command) {}

pub(crate) struct ProcessTree;

impl ProcessTree {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn attach(&mut self, _pid: u32) {}

    pub(crate) fn kill(&mut self, child: &mut std::process::Child) {
        let _ = child.kill();
    }

    pub(crate) fn close(&mut self) {}
}
