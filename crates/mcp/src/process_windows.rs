//! Windows tree kill via a kernel32 job object. Raw externs (no windows-sys)
//! mirror `crates/exec/src/tree_windows.rs`: one small syscall surface, no new
//! dependency, and the same degraded direct-child fallback.

use std::ffi::c_void;
use std::io;
use std::process::Command;

use crate::Error;

pub(crate) const LOADER_ENV_NAMES: &[&str] =
    &["SystemRoot", "SystemDrive", "windir", "COMSPEC", "PATHEXT"];

const PROCESS_SET_QUOTA: u32 = 0x0100;
const PROCESS_TERMINATE: u32 = 0x0001;
const ERROR_ACCESS_DENIED: i32 = 5;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
    fn OpenProcess(desired_access: u32, inherit: i32, process_id: u32) -> *mut c_void;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn TerminateJobObject(job: *mut c_void, exit_code: u32) -> i32;
    fn TerminateProcess(process: *mut c_void, exit_code: u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// An open job handle. Zero means no tree tracking was possible; the client
/// still works and the kill degrades to the direct child.
pub(crate) struct TreeHandle {
    job: usize,
}

impl Drop for TreeHandle {
    fn drop(&mut self) {
        if self.job != 0 {
            unsafe {
                let _ = CloseHandle(self.job as *mut c_void);
            }
        }
    }
}

pub(crate) fn prepare_tree(_command: &mut Command) {
    // A job object tracks descendants; CREATE_NEW_PROCESS_GROUP would only
    // affect console control events.
}

pub(crate) fn new_tree(pid: u32) -> TreeHandle {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if job.is_null() {
            return TreeHandle { job: 0 };
        }
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            let _ = CloseHandle(job);
            return TreeHandle { job: 0 };
        }
        let assigned = AssignProcessToJobObject(job, process);
        let _ = CloseHandle(process);
        if assigned == 0 {
            let _ = CloseHandle(job);
            return TreeHandle { job: 0 };
        }
        TreeHandle { job: job as usize }
    }
}

pub(crate) fn terminate_tree(pid: u32, tree: &TreeHandle) -> Result<(), Error> {
    let mut job_failed = false;
    if tree.job != 0 {
        unsafe {
            if TerminateJobObject(tree.job as *mut c_void, 1) == 0 {
                job_failed = true;
            }
        }
    } else {
        job_failed = true;
    }
    if !job_failed {
        return Ok(());
    }
    // The direct child is the fallback when no job could be created or the
    // child left it. ACCESS-DENIED here only means it is already gone.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return Ok(());
        }
        let result = TerminateProcess(process, 1);
        let error = io::Error::last_os_error();
        let _ = CloseHandle(process);
        if result == 0 && error.raw_os_error() != Some(ERROR_ACCESS_DENIED) {
            return Err(Error::Io(error));
        }
    }
    Ok(())
}

pub(crate) fn drop_tree(tree: &mut TreeHandle) {
    if tree.job != 0 {
        unsafe {
            let _ = CloseHandle(tree.job as *mut c_void);
        }
        tree.job = 0;
    }
}
