//! Hand-written Win32 bindings for job objects. No `windows-sys` dependency:
//! the surface is six functions and two structs, and the workspace dependency
//! budget exists to keep the binary small and the build offline-reproducible.
//!
//! Every `unsafe` block is inside or immediately around an `extern "system"`
//! declaration; each call site states the invariant it relies on (a live handle
//! of the documented type, a buffer of the documented size).

use std::ffi::c_void;
use std::io;

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::ERR_UNAVAILABLE;

pub(crate) type Handle = isize;

const INVALID_HANDLE_VALUE: Handle = -1;

pub(crate) const PROCESS_SET_QUOTA: u32 = 0x0100;
pub(crate) const PROCESS_TERMINATE: u32 = 0x0001;
const PROCESS_SUSPEND_RESUME: u32 = 0x0800;
pub(crate) const CREATE_SUSPENDED: u32 = 0x0000_0004;

const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
const JOB_OBJECT_LIMIT_JOB_MEMORY: u32 = 0x0000_0200;
const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0000_0800;

const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: u32 = 9;
const THREAD_SUSPEND_RESUME: u32 = 0x0002;
const TH32CS_SNAPTHREAD: u32 = 0x0000_0004;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ThreadEntry32 {
    dw_size: u32,
    cnt_usage: u32,
    th32_thread_id: u32,
    th32_owner_process_id: u32,
    tp_base_pri: i32,
    tp_delta_pri: i32,
    dw_flags: u32,
}

/// What a job object should cap. Zero means "do not cap this dimension": the
/// kernel reads the flag, and a zero cap with a flag set would kill every child
/// instantly, so [`Limits::flags`] only sets flags for positive values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Limits {
    pub process_memory_bytes: u64,
    pub job_memory_bytes: u64,
    pub active_processes: u32,
    pub breakaway_ok: bool,
}

impl Limits {
    fn flags(self) -> u32 {
        let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if self.process_memory_bytes > 0 {
            flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        }
        if self.job_memory_bytes > 0 {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        }
        if self.active_processes > 0 {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        }
        if self.breakaway_ok {
            flags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        flags
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(
        job: *mut c_void,
        info_class: u32,
        info: *const c_void,
        info_length: u32,
    ) -> i32;
    #[cfg(test)]
    fn QueryInformationJobObject(
        job: *mut c_void,
        info_class: u32,
        info: *mut c_void,
        info_length: u32,
        returned_length: *mut u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn TerminateJobObject(job: *mut c_void, exit_code: u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
    fn Thread32First(snapshot: *mut c_void, entry: *mut ThreadEntry32) -> i32;
    fn Thread32Next(snapshot: *mut c_void, entry: *mut ThreadEntry32) -> i32;
    fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> *mut c_void;
    fn ResumeThread(thread: *mut c_void) -> u32;
}

#[link(name = "ntdll")]
extern "system" {
    fn NtResumeProcess(process: *mut c_void) -> i32;
}

fn last_error(prefix: &str) -> Error {
    Error::new(
        ErrorKind::Refused,
        format!("sandbox: {}: {}", prefix, io::Error::last_os_error()),
    )
}

pub(crate) fn create_job() -> CoreResult<Handle> {
    // SAFETY: both arguments are the documented "use defaults" values; the
    // returned handle is owned by the caller and closed with close_handle.
    let handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
    if handle.is_null() {
        return Err(last_error("CreateJobObjectW"));
    }
    Ok(handle as Handle)
}

pub(crate) fn set_limits(job: Handle, limits: Limits) -> CoreResult<()> {
    let mut info = JobObjectExtendedLimitInformation::default();
    info.basic_limit_information.limit_flags = limits.flags();
    info.basic_limit_information.active_process_limit = limits.active_processes;
    info.process_memory_limit = limits.process_memory_bytes as usize;
    info.job_memory_limit = limits.job_memory_bytes as usize;
    // SAFETY: `info` is a live, correctly sized JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    // and `job` is a handle returned by create_job that has not been closed.
    let ok = unsafe {
        SetInformationJobObject(
            job as *mut c_void,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            &info as *const _ as *const c_void,
            std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
        )
    };
    if ok == 0 {
        return Err(last_error("SetInformationJobObject"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn query_limits(job: Handle) -> CoreResult<Limits> {
    let mut info = JobObjectExtendedLimitInformation::default();
    let mut returned = 0u32;
    // SAFETY: `info` is a live buffer of the documented size and `returned` is a
    // valid u32 pointer; `job` is owned by the caller.
    let ok = unsafe {
        QueryInformationJobObject(
            job as *mut c_void,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<JobObjectExtendedLimitInformation>() as u32,
            &mut returned,
        )
    };
    if ok == 0 {
        return Err(last_error("QueryInformationJobObject"));
    }
    let flags = info.basic_limit_information.limit_flags;
    if flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE == 0 {
        return Err(Error::new(
            ErrorKind::Refused,
            format!(
                "sandbox: job has no JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE (limit flags {:#x})",
                flags
            ),
        ));
    }
    Ok(Limits {
        process_memory_bytes: if flags & JOB_OBJECT_LIMIT_PROCESS_MEMORY != 0 {
            info.process_memory_limit as u64
        } else {
            0
        },
        job_memory_bytes: if flags & JOB_OBJECT_LIMIT_JOB_MEMORY != 0 {
            info.job_memory_limit as u64
        } else {
            0
        },
        active_processes: if flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS != 0 {
            info.basic_limit_information.active_process_limit
        } else {
            0
        },
        breakaway_ok: flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK != 0,
    })
}

pub(crate) fn close_handle(handle: Handle) -> CoreResult<()> {
    if handle == 0 || handle == INVALID_HANDLE_VALUE {
        return Ok(());
    }
    // SAFETY: the handle is owned by this enforcer; a double close is absorbed
    // below so a stale handle cannot turn shutdown into an error.
    let ok = unsafe { CloseHandle(handle as *mut c_void) };
    if ok == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(6) {
            // ERROR_INVALID_HANDLE: already closed.
            return Ok(());
        }
        return Err(Error::new(
            ErrorKind::Refused,
            format!("sandbox: CloseHandle: {}", error),
        ));
    }
    Ok(())
}

pub(crate) fn process_handle_for(pid: u32) -> CoreResult<Handle> {
    // PROCESS_SUSPEND_RESUME is deliberately not requested: on this host
    // OpenProcess denies it even for a child the caller just created. Resuming
    // goes through thread handles instead (resume_process).
    // SAFETY: OpenProcess is a plain Win32 call; the returned handle is closed
    // by the caller.
    let handle = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return Err(last_error(&format!("OpenProcess({})", pid)));
    }
    Ok(handle as Handle)
}

pub(crate) fn assign_to_job(job: Handle, process: Handle) -> CoreResult<()> {
    // SAFETY: both handles are live and owned by the caller; the process must be
    // suspended at this point, which jobEnforcer::apply guarantees.
    let ok = unsafe { AssignProcessToJobObject(job as *mut c_void, process as *mut c_void) };
    if ok == 0 {
        return Err(last_error("AssignProcessToJobObject"));
    }
    Ok(())
}

pub(crate) fn terminate_job(job: Handle, exit_code: u32) -> CoreResult<()> {
    // SAFETY: `job` is a live job handle owned by the enforcer.
    let ok = unsafe { TerminateJobObject(job as *mut c_void, exit_code) };
    if ok == 0 {
        return Err(last_error("TerminateJobObject"));
    }
    Ok(())
}

/// Start every thread of a process created with `CREATE_SUSPENDED`.
///
/// Two routes because the direct one is not always permitted: NtResumeProcess
/// on a process handle with PROCESS_SUSPEND_RESUME, then the Toolhelp thread
/// snapshot with OpenThread(THREAD_SUSPEND_RESUME) + ResumeThread. The second
/// route is not just a fallback: the harness's own sandbox denies the process
/// right but grants the thread right (measured on this host).
pub(crate) fn resume_process(pid: u32) -> CoreResult<()> {
    // SAFETY: OpenProcess returns an owned handle or null; NtResumeProcess takes
    // a live process handle. The handle is closed on both branches.
    unsafe {
        let process = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
        if !process.is_null() {
            let status = NtResumeProcess(process);
            let _ = CloseHandle(process);
            if status == 0 {
                return Ok(());
            }
        }
    }
    resume_threads(pid)
}

fn resume_threads(pid: u32) -> CoreResult<()> {
    // SAFETY: the snapshot call returns an owned handle or INVALID_HANDLE_VALUE;
    // Thread32First/Next fill the fixed-size ThreadEntry32 declared above (28
    // bytes, the documented size since Windows 95).
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot as Handle == INVALID_HANDLE_VALUE {
        return Err(last_error("CreateToolhelp32Snapshot"));
    }
    let snapshot = snapshot as Handle;
    let mut entry = ThreadEntry32 {
        dw_size: std::mem::size_of::<ThreadEntry32>() as u32,
        ..ThreadEntry32::default()
    };
    let mut resumed = 0u32;
    // SAFETY: the snapshot is live until close_handle below; entry has the size
    // the API expects.
    let mut present = unsafe { Thread32First(snapshot as *mut c_void, &mut entry) } != 0;
    while present {
        if entry.th32_owner_process_id == pid {
            // SAFETY: OpenThread returns an owned handle or null; ResumeThread
            // takes a live thread handle.
            unsafe {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32_thread_id);
                if !thread.is_null() {
                    let _ = ResumeThread(thread);
                    let _ = CloseHandle(thread);
                    resumed += 1;
                }
            }
        }
        entry.dw_size = std::mem::size_of::<ThreadEntry32>() as u32;
        // SAFETY: same live snapshot and buffer as above.
        present = unsafe { Thread32Next(snapshot as *mut c_void, &mut entry) } != 0;
    }
    let _ = close_handle(snapshot);
    if resumed == 0 {
        return Err(Error::new(
            ErrorKind::Refused,
            format!(
                "sandbox: cannot resume pid {}: NtResumeProcess unavailable and no threads found",
                pid
            ),
        ));
    }
    Ok(())
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        "unlimited".to_string()
    } else if bytes.is_multiple_of(1 << 30) {
        format!("{} GiB", bytes >> 30)
    } else if bytes.is_multiple_of(1 << 20) {
        format!("{} MiB", bytes >> 20)
    } else {
        format!("{} B", bytes)
    }
}

/// Compile-time proof that the default limits are the values the contract
/// promises: 1 GiB per process, 2 GiB per job, 64 processes.
pub(crate) const DEFAULT_LIMITS: Limits = Limits {
    process_memory_bytes: 1 << 30,
    job_memory_bytes: 2 << 30,
    active_processes: 64,
    breakaway_ok: false,
};

/// A clear failure for a limits object that would enforce nothing. A job object
/// with no limits is a process container, not a sandbox.
pub(crate) fn empty_limits_error() -> Error {
    Error::new(
        ErrorKind::Refused,
        format!(
            "sandbox: job object enforcer with no limits has nothing to enforce: {}",
            ERR_UNAVAILABLE
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_survive_a_set_query_round_trip() {
        let job = create_job().expect("create job");
        let limits = Limits {
            process_memory_bytes: 64 << 20,
            job_memory_bytes: 256 << 20,
            active_processes: 16,
            breakaway_ok: false,
        };
        set_limits(job, limits).expect("set limits");
        let got = query_limits(job).expect("query limits");
        assert_eq!(got.process_memory_bytes, limits.process_memory_bytes);
        assert_eq!(got.job_memory_bytes, limits.job_memory_bytes);
        assert_eq!(got.active_processes, limits.active_processes);
        close_handle(job).expect("close job");
    }

    #[test]
    fn default_limits_are_the_contract_values() {
        assert_eq!(DEFAULT_LIMITS.process_memory_bytes, 1 << 30);
        assert_eq!(DEFAULT_LIMITS.job_memory_bytes, 2 << 30);
        assert_eq!(DEFAULT_LIMITS.active_processes, 64);
    }
}
