//! Linux containment through Landlock.
//!
//! Landlock is a per-process, unprivileged filesystem policy. It is applied by
//! the process that is about to call `execve`, and it survives the exec; there
//! is no way for the parent to attach a ruleset to an already-created child.
//! The enforcer therefore re-executes the harness itself as a tiny wrapper: the
//! wrapper applies the ruleset to its own thread, then becomes the real command.
//!
//! The syscall numbers are stable across the architectures this workspace
//! supports (x86_64, aarch64, x86, arm). The ABI number returned by
//! `landlock_create_ruleset(NULL, 0, VERSION)` decides which bits may be set in
//! `handled_access_fs`; setting a bit the running kernel does not know is
//! EINVAL, which would turn a working policy into a refused command.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::raw::{c_char, c_int, c_long};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::{Mode, Platform, Spec, Support, ERR_CLOSED, ERR_UNAVAILABLE};

pub(crate) static PLATFORM: LinuxPlatform = LinuxPlatform;

pub(crate) struct LinuxPlatform;

impl Platform for LinuxPlatform {
    fn support(&self) -> Support {
        match landlock_abi() {
            Ok(abi) => Support {
                filesystem: true,
                network: abi >= 4,
                process_tree: false,
                enforced: false,
                detail: format!(
                    "linux: landlock ABI {abi} detected (filesystem writes confinable; TCP bind/connect {}). \
                     Process-tree teardown is internal/exec's job.",
                    if abi >= 4 { "confined" } else { "NOT confined" }
                ),
            },
            Err(error) => Support {
                filesystem: false,
                network: false,
                process_tree: false,
                enforced: false,
                detail: format!(
                    "linux: no usable landlock ({error}); filesystem writes cannot be confined. \
                     Process-tree teardown is internal/exec's job, not this package's."
                ),
            },
        }
    }

    fn install(
        &self,
        spec: &Spec,
        _logger: &dyn crate::Logger,
    ) -> CoreResult<Box<dyn crate::Enforcer>> {
        let abi = landlock_abi().map_err(|error| {
            Error::new(
                ErrorKind::Refused,
                format!("sandbox: linux: landlock unavailable: {error}: {ERR_UNAVAILABLE}"),
            )
        })?;
        // The wrapper has to be a program that calls `maybe_exec_helper` before
        // it parses anything. In production that is `forge`; in an integration
        // test the test harness itself is the current executable and would run
        // its own argument parser instead, so `Spec::wrapper` must be honoured
        // here exactly as Windows honours it. Falling back to this process keeps
        // the embedding case (a host library that is the harness) working.
        let self_path = match spec.wrapper.as_deref() {
            Some(path) if !path.trim().is_empty() => PathBuf::from(path),
            _ => std::env::current_exe().map_err(|error| {
                Error::new(
                    ErrorKind::Refused,
                    format!(
                        "sandbox: linux: cannot locate own executable: {error}: {ERR_UNAVAILABLE}"
                    ),
                )
            })?,
        };
        Ok(Box::new(LandlockEnforcer {
            spec: spec.clone(),
            abi,
            self_path,
            closed: Mutex::new(false),
        }))
    }
}

struct LandlockEnforcer {
    spec: Spec,
    abi: u32,
    self_path: PathBuf,
    closed: Mutex<bool>,
}

impl crate::Enforcer for LandlockEnforcer {
    fn name(&self) -> &str {
        "linux-landlock"
    }

    fn support(&self) -> Support {
        Support {
            filesystem: true,
            network: self.abi >= 4,
            process_tree: false,
            enforced: true,
            detail: format!(
                "linux landlock ABI {}: filesystem writes confined to {}; TCP bind/connect {}. \
                 Process-tree teardown is internal/exec's job.",
                self.abi,
                self.spec.workspace,
                if self.abi >= 4 && !self.spec.allow_net {
                    "confined"
                } else {
                    "NOT confined"
                }
            ),
        }
    }

    fn apply(&self, command: &mut std::process::Command) -> CoreResult<()> {
        {
            let closed = self
                .closed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *closed {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!("sandbox: apply on \"{}\": {}", self.name(), ERR_CLOSED),
                ));
            }
        }

        let payload = HelperPayload {
            mode: self.spec.mode.as_str().to_string(),
            workspace: self.spec.workspace.clone(),
            extra_write: self.spec.extra_write.clone(),
            allow_net: self.spec.allow_net,
        };
        let json = serde_json::to_string(&payload).map_err(|error| {
            Error::new(
                ErrorKind::Invalid,
                format!("sandbox: linux: cannot encode landlock spec: {error}"),
            )
        })?;
        command.env(HELPER_ENV, json);
        crate::wrap_command(command, &self.self_path, &[])?;
        Ok(())
    }

    fn close(&self) -> CoreResult<()> {
        let mut closed = self
            .closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *closed = true;
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HelperPayload {
    mode: String,
    workspace: String,
    #[serde(default)]
    extra_write: Vec<String>,
    allow_net: bool,
}

const HELPER_ENV: &str = "FORGE_SANDBOX_LANDLOCK_SPEC";

const SYS_LANDLOCK_CREATE_RULESET: c_long = 444;
const SYS_LANDLOCK_ADD_RULE: c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: c_long = 446;
const LANDLOCK_CREATE_RULESET_VERSION: usize = 1;
const LANDLOCK_RULE_PATH_BENEATH: usize = 1;
const PR_SET_NO_NEW_PRIVS: c_int = 38;

const O_PATH: c_int = 0x200000;
const O_DIRECTORY: c_int = 0x10000;
const O_CLOEXEC: c_int = 0x80000;

const ACCESS_FS_EXECUTE: u64 = 0x1;
const ACCESS_FS_WRITE_FILE: u64 = 0x2;
const ACCESS_FS_READ_FILE: u64 = 0x4;
const ACCESS_FS_READ_DIR: u64 = 0x8;
const ACCESS_FS_REMOVE_DIR: u64 = 0x10;
const ACCESS_FS_REMOVE_FILE: u64 = 0x20;
const ACCESS_FS_MAKE_CHAR: u64 = 0x40;
const ACCESS_FS_MAKE_DIR: u64 = 0x80;
const ACCESS_FS_MAKE_REG: u64 = 0x100;
const ACCESS_FS_MAKE_SOCK: u64 = 0x200;
const ACCESS_FS_MAKE_FIFO: u64 = 0x400;
const ACCESS_FS_MAKE_BLOCK: u64 = 0x800;
const ACCESS_FS_MAKE_SYM: u64 = 0x1000;
const ACCESS_FS_REFER: u64 = 0x2000;
const ACCESS_FS_TRUNCATE: u64 = 0x4000;
const ACCESS_NET_BIND_TCP: u64 = 0x1;
const ACCESS_NET_CONNECT_TCP: u64 = 0x2;

extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn prctl(option: c_int, ...) -> c_int;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn execvp(file: *const c_char, argv: *const *const c_char) -> c_int;
}

#[repr(C)]
struct RulesetAttrV1 {
    handled_access_fs: u64,
}

#[repr(C)]
struct RulesetAttrV4 {
    handled_access_fs: u64,
    handled_access_net: u64,
}

#[repr(C, packed)]
struct PathBeneath {
    allowed_access: u64,
    parent_fd: i32,
}

fn landlock_abi() -> io::Result<u32> {
    // SAFETY: Landlock's ABI probe takes a null ruleset pointer, a zero size and
    // the VERSION flag; the kernel only reads those three integer arguments.
    let result = unsafe {
        syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttrV1>() as usize,
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as u32)
    }
}

fn supported_write_rights(abi: u32) -> u64 {
    let mut rights = ACCESS_FS_WRITE_FILE
        | ACCESS_FS_REMOVE_DIR
        | ACCESS_FS_REMOVE_FILE
        | ACCESS_FS_MAKE_CHAR
        | ACCESS_FS_MAKE_DIR
        | ACCESS_FS_MAKE_REG
        | ACCESS_FS_MAKE_SOCK
        | ACCESS_FS_MAKE_FIFO
        | ACCESS_FS_MAKE_BLOCK
        | ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        rights |= ACCESS_FS_REFER;
    }
    if abi >= 3 {
        rights |= ACCESS_FS_TRUNCATE;
    }
    rights
}

fn create_ruleset(abi: u32, allow_net: bool) -> io::Result<c_int> {
    let handled_fs = supported_write_rights(abi);
    let result = if abi >= 4 && !allow_net {
        let attr = RulesetAttrV4 {
            handled_access_fs: handled_fs,
            handled_access_net: ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP,
        };
        // SAFETY: `attr` is a correctly aligned C struct and 16 is its size.
        unsafe {
            syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &attr as *const RulesetAttrV4 as usize,
                std::mem::size_of::<RulesetAttrV4>(),
                0usize,
            )
        }
    } else {
        let attr = RulesetAttrV1 {
            handled_access_fs: handled_fs,
        };
        // SAFETY: as above with the 8-byte v1 layout.
        unsafe {
            syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &attr as *const RulesetAttrV1 as usize,
                std::mem::size_of::<RulesetAttrV1>(),
                0usize,
            )
        }
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as c_int)
    }
}

fn add_path_rule(ruleset_fd: c_int, path: &str, abi: u32) -> io::Result<()> {
    let path = CString::new(path.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // A rule on a directory may grant every write right below it; a rule on a
    // single file may grant only writing (and truncating) that file. Opening a
    // file with O_DIRECTORY fails, which would turn a valid `extra_write` file
    // into a wrapper that exits 126 instead of running the command.
    let metadata = std::fs::metadata(Path::new(OsStr::from_bytes(path.as_bytes())))
        .map_err(|error| io::Error::new(error.kind(), format!("stat {path:?}: {error}")))?;
    let (open_flags, allowed_access) = if metadata.is_dir() {
        (
            O_PATH | O_DIRECTORY | O_CLOEXEC,
            supported_write_rights(abi),
        )
    } else {
        let mut rights = ACCESS_FS_WRITE_FILE;
        if abi >= 3 {
            rights |= ACCESS_FS_TRUNCATE;
        }
        (O_PATH | O_CLOEXEC, rights)
    };
    // SAFETY: `path` is a valid NUL-terminated string and the flags are the
    // documented O_PATH combination for the stat result above.
    let fd = unsafe { open(path.as_ptr(), open_flags, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let rule = PathBeneath {
        allowed_access,
        parent_fd: fd,
    };
    // SAFETY: `rule` is the exact packed 12-byte path_beneath layout the kernel
    // expects; the borrowed fd stays open until after the syscall.
    let result = unsafe {
        syscall(
            SYS_LANDLOCK_ADD_RULE,
            ruleset_fd as usize,
            LANDLOCK_RULE_PATH_BENEATH,
            &rule as *const PathBeneath as usize,
            0usize,
        )
    };
    // SAFETY: the fd was produced by open above and is owned by this function.
    unsafe {
        close(fd);
    }
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn restrict_self(ruleset_fd: c_int) -> io::Result<()> {
    // SAFETY: PR_SET_NO_NEW_PRIVS takes one integer; the remaining arguments are
    // unused and zero.
    let result = unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1usize, 0usize, 0usize, 0usize) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `ruleset_fd` is a live Landlock ruleset descriptor.
    let result = unsafe {
        syscall(
            SYS_LANDLOCK_RESTRICT_SELF,
            ruleset_fd as usize,
            0usize,
            0usize,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Entry point when the process was launched as the Landlock wrapper. Returns
/// normally when the helper variable is absent; otherwise it either execs the
/// real program or terminates with exit code 126.
pub(crate) fn maybe_exec_helper() {
    let Some(raw) = std::env::var_os(HELPER_ENV) else {
        return;
    };
    let raw = raw.to_string_lossy().into_owned();
    if let Err(error) = run_as_helper(&raw) {
        eprintln!("sandbox: landlock helper: {error}");
        std::process::exit(126);
    }
}

fn run_as_helper(raw: &str) -> Result<(), String> {
    let payload: HelperPayload =
        serde_json::from_str(raw).map_err(|error| format!("decode spec: {error}"))?;
    let mode = crate::mode_from_str(&payload.mode)
        .ok_or_else(|| format!("unknown mode {:?}", payload.mode))?;

    let args: Vec<OsString> = std::env::args_os().collect();
    if args.len() < 2 {
        return Err("missing command after the wrapper".to_string());
    }

    let abi = landlock_abi().map_err(|error| format!("landlock ABI probe: {error}"))?;
    let ruleset = create_ruleset(abi, payload.allow_net)
        .map_err(|error| format!("create ruleset: {error}"))?;
    if mode == Mode::WorkspaceWrite {
        add_path_rule(ruleset, &payload.workspace, abi)
            .map_err(|error| format!("allow workspace {:?}: {error}", payload.workspace))?;
        for path in &payload.extra_write {
            add_path_rule(ruleset, path, abi)
                .map_err(|error| format!("allow extra path {path:?}: {error}"))?;
        }
    }
    restrict_self(ruleset).map_err(|error| format!("restrict self: {error}"))?;
    // SAFETY: the descriptor is live and owned by this function.
    unsafe {
        close(ruleset);
    }

    std::env::remove_var(HELPER_ENV);

    let target = &args[1];
    let target =
        CString::new(target.as_bytes()).map_err(|_| "command contains a NUL byte".to_string())?;
    let argv: Vec<CString> = args[1..]
        .iter()
        .map(|arg| CString::new(arg.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|_| "argument contains a NUL byte".to_string())?;
    let mut argv_ptrs: Vec<*const c_char> = argv.iter().map(|arg| arg.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    // SAFETY: every pointer in `argv_ptrs` points to a live CString, and the
    // vector ends with the required null terminator.
    unsafe {
        execvp(target.as_ptr(), argv_ptrs.as_ptr());
    }
    Err(format!(
        "exec {:?}: {}",
        args[1],
        io::Error::last_os_error()
    ))
}

const _: () = {
    // Keep the read/execute constants visible in the policy documentation even
    // though the write-only policy never grants them.
    let _ = ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;
};
