//! Windows containment: a job object per execution.
//!
//! A job object is the strongest stdlib-reachable primitive on Windows. The
//! kernel terminates every process in the job when its last handle closes
//! (KILL_ON_JOB_CLOSE), and it caps commit memory per process, commit memory for
//! the whole job, and the number of live processes. What it cannot do is name a
//! directory: no job-object flag says "writes below the workspace are allowed
//! and writes to C:\Windows are not".
//!
//! So `Support::filesystem` is false here, and `workspace-write` is reported as
//! partially enforced. `read-only` is refused because its whole promise is the
//! missing dimension. Claiming otherwise would be the silent downgrade this
//! crate exists to prevent.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::job_windows::{
    self, close_handle, create_job, empty_limits_error, format_bytes, set_limits, Limits,
    CREATE_SUSPENDED, DEFAULT_LIMITS,
};
use crate::{Enforcer, Spec, Support, ERR_CLOSED, ERR_UNAVAILABLE};

pub(crate) static PLATFORM: WindowsPlatform = WindowsPlatform;

pub(crate) struct WindowsPlatform;

impl crate::Platform for WindowsPlatform {
    fn support(&self) -> Support {
        Support {
            filesystem: false,
            network: false,
            process_tree: true,
            enforced: false,
            detail: "windows: job objects can enforce a process tree, per-process and per-job commit \
                     memory and a process-count cap (measured on this host), but no job-object limit \
                     restricts filesystem paths or network sockets, so read-only is refused and \
                     workspace-write reports filesystem=false rather than silently downgrading to \
                     'process limits only'"
                .to_string(),
        }
    }

    fn install(&self, spec: &Spec, _logger: &dyn crate::Logger) -> CoreResult<Box<dyn Enforcer>> {
        Ok(Box::new(JobEnforcer::new(job_limits_for(spec))?))
    }
}

/// Map the frozen [`Spec`] to job limits. `Spec` has no memory fields, so the
/// only input that matters is the mode; the function exists so the day the
/// contract grows a field there is one place to change.
fn job_limits_for(_spec: &Spec) -> Limits {
    DEFAULT_LIMITS
}

struct JobState {
    /// Apply'd, awaiting AfterStart, in call order.
    pending: VecDeque<job_windows::Handle>,
    /// Every job handle not yet closed.
    live: HashSet<job_windows::Handle>,
    closed: bool,
}

pub(crate) struct JobEnforcer {
    state: Mutex<JobState>,
    limits: Limits,
}

impl JobEnforcer {
    pub(crate) fn new(limits: Limits) -> CoreResult<Self> {
        if limits.process_memory_bytes == 0
            && limits.job_memory_bytes == 0
            && limits.active_processes == 0
        {
            return Err(empty_limits_error());
        }
        // Probe the host now: a machine that cannot create or configure a job
        // object must fail at New, not at the first command.
        let probe = create_job()?;
        if let Err(error) = set_limits(probe, limits) {
            let _ = close_handle(probe);
            return Err(error);
        }
        close_handle(probe)?;
        Ok(Self {
            state: Mutex::new(JobState {
                pending: VecDeque::new(),
                live: HashSet::new(),
                closed: false,
            }),
            limits,
        })
    }
}

impl Enforcer for JobEnforcer {
    fn name(&self) -> &str {
        "windows-job-object"
    }

    fn support(&self) -> Support {
        Support {
            filesystem: false,
            network: false,
            process_tree: true,
            enforced: true,
            detail: format!(
                "windows job object: process tree (KILL_ON_JOB_CLOSE), process memory {}, job memory {}, \
                 active processes {} — filesystem and network are NOT confined",
                format_bytes(self.limits.process_memory_bytes),
                format_bytes(self.limits.job_memory_bytes),
                self.limits.active_processes
            ),
        }
    }

    fn apply(&self, command: &mut std::process::Command) -> CoreResult<()> {
        use std::os::windows::process::CommandExt;

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.closed {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("sandbox: apply on \"{}\": {}", self.name(), ERR_CLOSED),
            ));
        }

        let job = create_job()?;
        if let Err(error) = set_limits(job, self.limits) {
            let _ = close_handle(job);
            return Err(error);
        }

        // CREATE_SUSPENDED is not optional: adoption must happen before the
        // child executes its first instruction, or a grandchild spawned in that
        // window escapes the job entirely.
        command.creation_flags(CREATE_SUSPENDED);

        state.pending.push_back(job);
        state.live.insert(job);
        Ok(())
    }

    fn after_start(&self, pid: u32) -> CoreResult<()> {
        let job = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.closed {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!("sandbox: after start pid {}: {}", pid, ERR_CLOSED),
                ));
            }
            match state.pending.pop_front() {
                Some(job) => job,
                None => {
                    return Err(Error::new(
                        ErrorKind::Refused,
                        format!(
                            "sandbox: after start pid {} without a preceding Apply: {}",
                            pid, ERR_UNAVAILABLE
                        ),
                    ))
                }
            }
        };

        let fail = |error: Error| -> CoreResult<()> {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.live.remove(&job);
            let _ = job_windows::terminate_job(job, 1);
            let _ = close_handle(job);
            Err(Error::new(
                ErrorKind::Refused,
                format!("sandbox: adopt pid {}: {}", pid, error.message()),
            ))
        };

        let process = match job_windows::process_handle_for(pid) {
            Ok(handle) => handle,
            Err(error) => return fail(error),
        };
        let assigned = job_windows::assign_to_job(job, process);
        let _ = close_handle(process);
        if let Err(error) = assigned {
            return fail(error);
        }
        if let Err(error) = job_windows::resume_process(pid) {
            return fail(error);
        }
        Ok(())
    }

    fn abort_apply(&self) {
        let job = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.closed {
                return;
            }
            match state.pending.pop_front() {
                Some(job) => {
                    state.live.remove(&job);
                    job
                }
                None => return,
            }
        };
        let _ = close_handle(job);
    }

    fn close(&self) -> CoreResult<()> {
        let jobs = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.closed {
                return Ok(());
            }
            state.closed = true;
            let jobs: Vec<_> = state.live.drain().collect();
            state.pending.clear();
            jobs
        };
        let mut first_error = None;
        for job in jobs {
            if let Err(error) = close_handle(job) {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
