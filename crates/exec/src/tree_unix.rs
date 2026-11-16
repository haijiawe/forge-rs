//! Unix process-tree termination through a process group.
//!
//! `process_group(0)` puts the child in a new group whose id is its pid. On
//! timeout we signal the whole group, which catches grandchildren that the
//! direct child spawned; `child.kill()` remains as a fallback for the case
//! where the group setup or the group signal failed.

use std::os::unix::process::CommandExt;

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

const SIGKILL: i32 = 9;

pub(crate) fn configure(command: &mut std::process::Command) {
    command.process_group(0);
}

pub(crate) struct ProcessTree {
    pid: Option<u32>,
    killed: bool,
}

impl ProcessTree {
    pub(crate) fn new() -> Self {
        Self {
            pid: None,
            killed: false,
        }
    }

    pub(crate) fn attach(&mut self, pid: u32) {
        self.pid = Some(pid);
    }

    pub(crate) fn kill(&mut self, child: &mut std::process::Child) {
        if self.killed {
            return;
        }
        self.killed = true;
        if let Some(pid) = self.pid {
            // SAFETY: kill(2) takes an integer pid and signal; negating the
            // group id is the documented way to signal the group created by
            // process_group(0). No memory crosses the FFI boundary.
            unsafe {
                kill(-(pid as i32), SIGKILL);
            }
        }
        let _ = child.kill();
    }

    pub(crate) fn close(&mut self) {
        // Nothing to release: the process group disappears with its members.
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.close();
    }
}
