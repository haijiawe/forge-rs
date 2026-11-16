//! Windows process-tree termination through a job object.
//!
//! A job object is the only standard-library-reachable mechanism that kills a
//! whole descendant tree atomically. `taskkill /T /F` walks the process list,
//! which fails with "Access denied" under restricted tokens and sandboxing —
//! exactly where the harness runs tools — and races against children that spawn
//! grandchildren between enumeration and termination.
//!
//! The job is *not* created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: a
//! command that deliberately leaves a detached daemon behind must be able to do
//! so after a successful run. We terminate explicitly on timeout/cancel.

use std::ffi::c_void;
use std::ptr;

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn TerminateJobObject(job: *mut c_void, exit_code: u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
}

const PROCESS_SET_QUOTA: u32 = 0x0100;
const PROCESS_TERMINATE: u32 = 0x0001;

pub(crate) fn configure(_command: &mut std::process::Command) {
    // CREATE_NEW_PROCESS_GROUP would only affect console control events; it
    // does not make a tree killable. The job object does the tracking.
}

pub(crate) struct ProcessTree {
    job: Option<isize>,
    killed: bool,
}

impl ProcessTree {
    /// Create an anonymous job. Failure is not fatal: the caller still has the
    /// direct-child fallback, and refusing to run a command because tree
    /// tracking is unavailable would make the harness unusable in environments
    /// it can otherwise serve.
    pub(crate) fn new() -> Self {
        // SAFETY: both arguments are documented as optional and we pass null;
        // no Rust memory is exposed to the kernel. A null return means the
        // call failed and is handled below.
        let handle = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null()) };
        Self {
            job: if handle.is_null() {
                None
            } else {
                Some(handle as isize)
            },
            killed: false,
        }
    }

    /// Adopt `pid` into the job after `spawn` returned.
    ///
    /// OpenProcess asks only for the rights assignment needs
    /// (PROCESS_SET_QUOTA plus PROCESS_TERMINATE), so it also works with a
    /// restricted token that denies PROCESS_ALL_ACCESS.
    pub(crate) fn attach(&mut self, pid: u32) {
        let Some(job) = self.job else {
            return;
        };
        // SAFETY: `job` is a live handle from CreateJobObjectW; OpenProcess
        // takes a plain integer pid and returns a new handle or null, which is
        // checked before use.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            self.release(job);
            return;
        }
        // SAFETY: both handles are valid and owned by this function; the
        // kernel only reads them for the duration of the call.
        let assigned = unsafe { AssignProcessToJobObject(job as *mut c_void, process) };
        // SAFETY: `process` was returned by OpenProcess and is closed exactly
        // once here; closing it does not affect the child's own handle.
        unsafe {
            CloseHandle(process);
        }
        if assigned == 0 {
            // The process is alive outside the job. Falling back to a direct
            // kill is still possible, so discard the useless job handle rather
            // than pretending the tree is tracked.
            self.release(job);
        }
    }

    /// Terminate every process currently in the job, then make sure the direct
    /// child is gone even if the job path failed.
    pub(crate) fn kill(&mut self, child: &mut std::process::Child) {
        if self.killed {
            return;
        }
        self.killed = true;
        if let Some(job) = self.job {
            // SAFETY: `job` is a live job handle; TerminateJobObject accepts
            // any handle with JOB_OBJECT_TERMINATE, which CreateJobObjectW
            // grants. The exit code is data for the parent, not memory.
            unsafe {
                TerminateJobObject(job as *mut c_void, 1);
            }
        }
        // Best effort and intentionally unconditional: TerminateJobObject can
        // fail for a process that was never assigned, and child.kill() needs no
        // job at all.
        let _ = child.kill();
    }

    /// Release the handle without killing survivors. Called after a successful
    /// run, so an intentionally detached daemon keeps running.
    pub(crate) fn close(&mut self) {
        if let Some(job) = self.job.take() {
            self.release(job);
        }
    }

    fn release(&mut self, job: isize) {
        // SAFETY: `job` is a handle created by CreateJobObjectW and is removed
        // from `self.job` by the caller, so it is closed exactly once.
        unsafe {
            CloseHandle(job as *mut c_void);
        }
        self.job = None;
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.close();
    }
}
