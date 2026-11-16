//! POSIX process-group tree kill. The child is placed in its own group; every
//! descendant that has not deliberately left it dies with `kill(-pgid, SIGKILL)`.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::Error;

pub(crate) const LOADER_ENV_NAMES: &[&str] = &[];

pub(crate) struct TreeHandle;

pub(crate) fn prepare_tree(command: &mut Command) {
    // process_group(0) makes the child's pid the new process group id.
    command.process_group(0);
}

pub(crate) fn new_tree(_pid: u32) -> TreeHandle {
    TreeHandle
}

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

const SIGKILL: i32 = 9;
const ESRCH: i32 = 3;

pub(crate) fn terminate_tree(pid: u32, _tree: &TreeHandle) -> Result<(), Error> {
    let pid = pid as i32;
    // A negative pid signals the whole process group. ESRCH means it is gone.
    let group = unsafe { kill(-pid, SIGKILL) };
    if group == 0 || io::Error::last_os_error().raw_os_error() == Some(ESRCH) {
        return Ok(());
    }
    let direct = unsafe { kill(pid, SIGKILL) };
    if direct == 0 || io::Error::last_os_error().raw_os_error() == Some(ESRCH) {
        return Ok(());
    }
    Err(Error::Io(io::Error::last_os_error()))
}

pub(crate) fn drop_tree(_tree: &mut TreeHandle) {}
