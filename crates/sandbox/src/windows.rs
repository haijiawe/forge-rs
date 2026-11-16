//! Windows containment: an AppContainer inside a job object.
//!
//! Two rings, and each answers a question the other cannot.
//!
//! * The **job object** (`job_windows.rs`) owns lifetime and resources: the kernel
//!   kills every process in the job when its last handle closes
//!   (KILL_ON_JOB_CLOSE), and it caps commit memory per process, commit memory for
//!   the whole job, and the number of live processes.
//! * The **AppContainer** (`appcontainer.rs`) owns what the process may touch: the
//!   token carries a package SID that holds no rights except the ACEs granted to
//!   it by name, plus whatever capabilities name a network scope.
//!
//! No job-object flag can name a directory, and no AppContainer can guarantee that
//! a grandchild dies with its parent, so neither is decoration. What is still not
//! covered is spelled out in `support()`: the container bounds writes to the paths
//! we granted, and the job bounds lifetime.
//!
//! Because a primary token cannot be lowered in place, the restriction is installed
//! by re-executing the harness as a wrapper and letting *it* start the real command
//! inside the container — the same shape `linux.rs` uses for Landlock. The wrapper
//! is adopted into this execution's job before it runs, so the container's child is
//! born inside the job.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::appcontainer::{self, WrapperSpec, HELPER_ENV};
use crate::job_windows::{
    self, close_handle, create_job, empty_limits_error, format_bytes, set_limits, Limits,
    CREATE_SUSPENDED, DEFAULT_LIMITS,
};
use crate::{Enforcer, Spec, Support, ERR_CLOSED, ERR_UNAVAILABLE};

pub(crate) static PLATFORM: WindowsPlatform = WindowsPlatform;

pub(crate) struct WindowsPlatform;

impl crate::Platform for WindowsPlatform {
    fn support(&self) -> Support {
        // A machine that cannot create a package profile must say so here rather
        // than at the first command, so `forge doctor` and the refusal agree.
        if !appcontainer::available() {
            return Support {
                filesystem: false,
                network: false,
                process_tree: true,
                enforced: false,
                detail:
                    "windows: no AppContainer is available on this host, so only the job object \
                         applies — process tree, per-process and per-job commit memory and a \
                         process-count cap. Filesystem and network are NOT confined, so read-only \
                         is refused and workspace-write reports filesystem=false rather than \
                         silently downgrading to 'process limits only'"
                        .to_string(),
            };
        }
        Support {
            filesystem: true,
            network: true,
            process_tree: true,
            enforced: true,
            detail: "windows: AppContainer available (write access is granted to the workspace by \
                     package SID; no internetClient capability means no outbound sockets) inside a \
                     job object that owns lifetime and memory"
                .to_string(),
        }
    }

    fn install(&self, spec: &Spec, _logger: &dyn crate::Logger) -> CoreResult<Box<dyn Enforcer>> {
        if !appcontainer::available() {
            // Unreachable through `new` — `support()` already refused — but the
            // contract is "a platform that claims a capability and cannot install it
            // is an error, never a silent downgrade", so the branch is spelled out.
            return Err(Error::new(
                ErrorKind::Refused,
                "sandbox: windows: this host cannot create an AppContainer, so the requested mode \
                 cannot be enforced",
            ));
        }
        let self_path = match spec.wrapper.as_deref() {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe().map_err(|error| {
                Error::new(
                    ErrorKind::Io,
                    format!("sandbox: windows: cannot find the harness executable: {error}"),
                )
            })?,
        };
        Ok(Box::new(JobEnforcer::new(
            job_limits_for(spec),
            self_path,
            spec.mode.as_str().to_string(),
            spec.workspace.clone(),
            spec.extra_write.clone(),
            spec.extra_read.clone(),
            spec.allow_net,
        )?))
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
    /// The harness binary, re-executed as the container wrapper.
    self_path: PathBuf,
    /// The confinement the wrapper is told to install, encoded once per execution.
    spec: WrapperSpec,
}

impl JobEnforcer {
    pub(crate) fn new(
        limits: Limits,
        self_path: PathBuf,
        mode: String,
        workspace: String,
        extra_write: Vec<String>,
        extra_read: Vec<String>,
        allow_net: bool,
    ) -> CoreResult<Self> {
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
            self_path,
            spec: WrapperSpec {
                mode,
                workspace,
                extra_write,
                extra_read,
                allow_net,
            },
        })
    }
}

impl Enforcer for JobEnforcer {
    fn name(&self) -> &str {
        "windows-job-object+appcontainer"
    }

    fn support(&self) -> Support {
        Support {
            filesystem: true,
            network: true,
            process_tree: true,
            enforced: true,
            detail: format!(
                "windows: AppContainer (package SID, {} write access to the workspace, {} outbound \
                 sockets, {} extra read and {} extra write paths) inside a job object (process tree \
                 via KILL_ON_JOB_CLOSE, process memory {}, job memory {}, active processes {})",
                if self.spec.mode == "workspace-write" {
                    "read and"
                } else {
                    "no"
                },
                if self.spec.allow_net {
                    "internetClient capability allows"
                } else {
                    "no capability, so no"
                },
                self.spec.extra_read.len(),
                self.spec.extra_write.len(),
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

        // The confinement is installed by the wrapper, which is why the command is
        // rewritten here rather than merely flagged: a primary token cannot be
        // lowered in place, so the only way to hand a child an AppContainer token is
        // to have a process start it. The wrapper is what lands in the job and is
        // resumed above; its child inherits the job, so the tree still dies whole.
        let payload = serde_json::to_string(&self.spec).map_err(|error| {
            let _ = close_handle(job);
            Error::new(
                ErrorKind::Invalid,
                format!("sandbox: windows: cannot encode the container spec: {error}"),
            )
        })?;
        command.env(HELPER_ENV, payload);
        if let Err(error) = crate::wrap_command(command, &self.self_path, &[]) {
            let _ = close_handle(job);
            return Err(error);
        }

        // And the flag has to be set *after* the rewrite: `wrap_command` replaces the
        // command whole, so a flag set before it is simply gone.
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
