//! Windows containment, inner ring: an AppContainer.
//!
//! A job object (see `job_windows.rs`) can kill a tree and cap its memory, but no
//! job flag says "writes below the workspace are allowed and writes to
//! `C:\Windows` are not". The Windows primitive that answers that question is an
//! AppContainer: a process whose token carries a package SID with no access to
//! anything except what an ACE grants that SID by name, and whose network access
//! is whatever capabilities the token holds.
//!
//! Two facts shape the implementation:
//!
//! * You cannot lower your own primary token. An AppContainer has to be handed to
//!   a child at `CreateProcessW`, which needs `STARTUPINFOEX` and
//!   `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`. `std::process::Command` has no
//!   way to express that, so the harness re-executes itself as a wrapper — the
//!   same shape `linux.rs` uses for Landlock, and the reason `wrap_command`
//!   exists (`resolve.rs`).
//! * The wrapper stays unsandboxed; it runs no user code. It derives the package
//!   SID, grants the workspace to it, spawns the real command inside the container
//!   with the job object already covering it, proxies the exit code, and dies.
//!
//! Bindings are hand-written like the rest of this crate: the surface is fifteen
//! functions and seven structs, and the workspace dependency budget exists so the
//! build stays offline-reproducible and the binary small.

#![allow(unsafe_code)]

use std::ffi::{c_void, OsStr};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::job_windows::close_handle;

/// The channel the harness uses to tell its own re-executed self what to confine.
/// Mirrors `linux.rs`'s `HELPER_ENV`; the environment is the only thing that
/// survives a spawn without a second binary.
pub(crate) const HELPER_ENV: &str = "FORGE_SANDBOX_APPCONTAINER_SPEC";

type Sid = *mut c_void;
type Acl = *mut c_void;

const SE_FILE_OBJECT: u32 = 1;
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;

const GRANT_ACCESS: u32 = 1;
const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = 0x3;
const TRUSTEE_IS_SID: u32 = 0;
const TRUSTEE_IS_UNKNOWN: u32 = 0;

/// Read and traverse only.
const ACCESS_READ: u32 = 0x0012_0089 | 0x0012_00A0; // FILE_GENERIC_READ | EXECUTE
/// Read, write, delete and create children.
const ACCESS_WRITE: u32 = 0x0013_01FF; // +  FILE_GENERIC_WRITE | DELETE | FILE_DELETE_CHILD
/// FILE_TRAVERSE | FILE_READ_ATTRIBUTES: enough to walk *through* a directory and
/// stat it, not enough to list it. Granted on the ancestors of every allowed path.
const ACCESS_TRAVERSE: u32 = 0x0000_0020 | 0x0000_0080;
const NO_INHERITANCE: u32 = 0x0;

const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES: usize = 0x0002_0009;
const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF7;
const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF8;
const INFINITE: u32 = 0xFFFF_FFFF;
const ERROR_ALREADY_EXISTS: u32 = 183;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_PATH_NOT_FOUND: u32 = 3;
/// How many times a transient profile-service failure is retried before the
/// sandbox refuses. Four attempts with a rising 20–80 ms pause covers a busy
/// profile service without making a genuinely broken host wait.
const PROFILE_ATTEMPTS: usize = 8;
/// One identity for the whole user, used only when the profile service refuses to
/// create new names. See `package_sid`.
const SHARED_PROFILE: &str = "forge.sandbox";
/// Every profile this crate creates starts with this, which is what makes pruning
/// possible: the profile service cannot enumerate its own profiles.
const PROFILE_PREFIX: &str = "forge.sandbox.";
/// What `GetStdHandle` returns for a stream the parent does not have.
const INVALID_HANDLE_VALUE: isize = -1;

/// The capability SID `S-1-15-3-1` (`internetClient`), spelled as a string so a
/// future capability is a one-line change rather than a struct.
const INTERNET_CLIENT: &str = "S-1-15-3-1";

/// `OpenProcessToken` access for a read-only query.
const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenIntegrityLevel`.
const TOKEN_INTEGRITY_LEVEL: u32 = 25;
/// `SECURITY_MANDATORY_MEDIUM_RID`: anything below this (Low is exactly 0x1000,
/// Untrusted 0x0000) cannot write to the profile directories the profile service
/// needs. The comparison is `< medium`, not `< low`, because Low *is* 0x1000.
const MEDIUM_INTEGRITY_RID: u32 = 0x2000;

/// The layout of a SID up to its variable-length sub-authority array.
#[repr(C)]
struct SidHeader {
    revision: u8,
    sub_authority_count: u8,
    identifier_authority: [u8; 6],
    sub_authority: [u32; 1],
}

#[repr(C)]
struct TokenMandatoryLabel {
    label: SidAndAttributes,
}

#[repr(C)]
struct SidAndAttributes {
    sid: Sid,
    attributes: u32,
}

#[repr(C)]
struct SecurityCapabilities {
    app_container_sid: Sid,
    capabilities: *mut SidAndAttributes,
    capability_count: u32,
    reserved: u32,
}

#[repr(C)]
struct TrusteeW {
    multiple_trustee: *mut c_void,
    multiple_trustee_operation: u32,
    trustee_form: u32,
    trustee_type: u32,
    name: *mut u16,
}

#[repr(C)]
struct ExplicitAccessW {
    access_permissions: u32,
    access_mode: u32,
    inheritance: u32,
    trustee: TrusteeW,
}

#[repr(C)]
#[derive(Default)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2_size: u16,
    reserved2: *mut u8,
    std_input: isize,
    std_output: isize,
    std_error: isize,
}

#[repr(C)]
#[derive(Default)]
struct StartupInfoExW {
    startup_info: StartupInfoW,
    attribute_list: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
struct ProcessInformation {
    process: isize,
    thread: isize,
    process_id: u32,
    thread_id: u32,
}

// `userenv` and `advapi32` are not in rustc's default Windows import set; the rest
// of the surface lives in `kernel32`, which `std` already links.
#[link(name = "userenv")]
#[link(name = "advapi32")]
extern "system" {
    fn CreateAppContainerProfile(
        name: *const u16,
        display_name: *const u16,
        description: *const u16,
        capabilities: *const SidAndAttributes,
        capability_count: u32,
        sid: *mut Sid,
    ) -> i32;
    fn DeriveAppContainerSidFromAppContainerName(name: *const u16, sid: *mut Sid) -> i32;
    fn DeleteAppContainerProfile(name: *const u16) -> i32;
    fn ConvertStringSidToSidW(text: *const u16, sid: *mut Sid) -> i32;
    fn GetNamedSecurityInfoW(
        object: *mut u16,
        object_type: u32,
        security_info: u32,
        owner: *mut Sid,
        group: *mut Sid,
        dacl: *mut Acl,
        sacl: *mut Acl,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn SetNamedSecurityInfoW(
        object: *mut u16,
        object_type: u32,
        security_info: u32,
        owner: Sid,
        group: Sid,
        dacl: Acl,
        sacl: Acl,
    ) -> u32;
    fn SetEntriesInAclW(
        count: u32,
        entries: *const ExplicitAccessW,
        old_acl: Acl,
        new_acl: *mut Acl,
    ) -> u32;
    /// Answers "what does this DACL already allow *this* trustee", so a repeat grant
    /// can be skipped. Skipping is not an optimisation: rewriting a directory's DACL
    /// makes Windows re-evaluate inheritance across the whole subtree, which measured
    /// 9 s for `%TEMP%`, 11 s for `AppData\Local`, 20 s for `AppData` and 32 s for the
    /// profile root on this host. Paying that on every command would make the sandbox
    /// unusable, and the ACE is already on disk after the first one.
    fn GetEffectiveRightsFromAclW(acl: Acl, trustee: *const TrusteeW, rights: *mut u32) -> u32;
    fn InitializeProcThreadAttributeList(
        list: *mut c_void,
        count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *const c_void,
        size: usize,
        previous: *mut c_void,
        returned: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
    fn CreateProcessW(
        application: *const u16,
        command_line: *mut u16,
        process_attributes: *mut c_void,
        thread_attributes: *mut c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *mut c_void,
        current_directory: *const u16,
        startup: *mut StartupInfoExW,
        process: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(handle: isize, code: *mut u32) -> i32;
    fn GetStdHandle(which: u32) -> isize;
    fn GetHandleInformation(handle: isize, flags: *mut u32) -> i32;
    fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetLastError() -> u32;
    fn GetCurrentProcess() -> isize;
    fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
    fn GetTokenInformation(
        token: isize,
        class: u32,
        information: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;

/// Make a handle inheritable for the duration of one `CreateProcessW`, and undo it
/// afterwards.
///
/// `STARTF_USESTDHANDLES` with `bInheritHandles = TRUE` fails with
/// ERROR_INVALID_PARAMETER when any of the three handles lacks
/// HANDLE_FLAG_INHERIT — which is the normal state of a test harness's pipes. The
/// previous flag is restored so this cannot leak inheritance into unrelated
/// children.
struct Inheritable {
    handle: isize,
    changed: bool,
}

impl Inheritable {
    fn new(handle: isize) -> Self {
        let mut flags: u32 = 0;
        let known = unsafe { GetHandleInformation(handle, &mut flags) };
        if known == 0 || flags & HANDLE_FLAG_INHERIT != 0 {
            return Self {
                handle,
                changed: false,
            };
        }
        let set = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        Self {
            handle,
            changed: set != 0,
        }
    }

    fn restore(&self) {
        if self.changed {
            unsafe { SetHandleInformation(self.handle, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> Error {
    let code = unsafe { GetLastError() };
    Error::new(
        ErrorKind::Io,
        format!(
            "sandbox: windows appcontainer: {}: {}",
            what,
            io::Error::from_raw_os_error(code as i32)
        ),
    )
}

/// `HRESULT`s from the AppContainer API are signed; anything below zero failed.
fn hresult_error(what: &str, code: i32) -> Error {
    let win32 = (code as u32 & 0xFFFF) as i32;
    Error::new(
        ErrorKind::Io,
        format!(
            "sandbox: windows appcontainer: {}: {}",
            what,
            io::Error::from_raw_os_error(win32)
        ),
    )
}

/// Where Windows keeps the package folder for this profile name.
///
/// Only used as an existence hint: the profile service owns this directory, and a
/// folder is not a proof of registration. Its absence, on the other hand, is a
/// reliable sign that a create attempt is needed.
fn profile_folder(profile: &str) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    if base.is_empty() {
        return None;
    }
    Some(PathBuf::from(base).join("Packages").join(profile))
}

/// A deterministic package name per workspace. FNV-1a, hand-written: the name has
/// to survive a rebuild and a Rust upgrade, so `DefaultHasher` (whose algorithm is
/// not a stable promise) is not usable here.
fn profile_name(workspace: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in workspace.to_ascii_lowercase().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("forge.sandbox.{hash:016x}")
}

/// Create the package profile (or adopt the existing one) and return its SID.
///
/// The order matters and is not a micro-optimisation: `DeriveAppContainerSidFromAppContainerName`
/// happily derives a SID from a name that has no profile, and `CreateProcessW`
/// with such a SID fails with `ERROR_FILE_NOT_FOUND` (2) — a message that names
/// neither the profile nor the SID. So the profile is created first, and deriving
/// is only the "someone else already made it" branch.
///
/// The retry is not defensive noise: creating a profile touches the per-user
/// profile service, and the same call that succeeds ten times in a row returns
/// `ERROR_ACCESS_DENIED` or `ERROR_PATH_NOT_FOUND` when that service is busy or
/// another process is creating the same profile. One such miss used to make the
/// sandbox refuse to run a command on a machine where it works, so the transient
/// codes are retried briefly before the failure is reported. A profile that needs
/// a retry is still a profile; a command that could have run is not.
fn package_sid(workspace: &str) -> CoreResult<Sid> {
    // Preferred: one profile per workspace, so two workspaces are two identities.
    if let Ok(sid) = sid_for(&profile_name(workspace)) {
        return Ok(sid);
    }
    // Fallback: a single identity for this user. This exists because the profile
    // service on a busy host starts refusing *new* names outright — measured here
    // after roughly ninety `forge.sandbox.*` profiles had accumulated: fresh
    // workspaces were denied for minutes at a time while the ones whose profile
    // already existed kept working. A shared identity still confines the command to
    // the directories granted to it; what it gives up is the guarantee that two
    // workspaces are separate from each other. Losing that is better than a session
    // where no command can run at all, and it is the only identity available.
    sid_for(SHARED_PROFILE).map_err(|err| {
        Error::new(
            ErrorKind::Refused,
            format!(
                "sandbox: windows appcontainer: neither a per-workspace nor the shared package profile could be obtained ({}){}",
                err.message(),
                low_integrity_hint()
            ),
        )
    })
}

/// The token's mandatory integrity RID (0x1000 low, 0x2000 medium, 0x3000 high),
/// or `None` when the token cannot be queried.
fn integrity_rid() -> Option<u32> {
    let mut token: isize = 0;
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    // Aligned storage: `TOKEN_MANDATORY_LABEL` starts with a pointer, and the
    // label SID follows it, so the struct is pointer-aligned and small (< 64 bytes
    // for the one-sub-authority SIDs Windows uses for integrity labels).
    let mut buffer = [0usize; 8];
    let mut needed: u32 = 0;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_INTEGRITY_LEVEL,
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(&buffer) as u32,
            &mut needed,
        )
    };
    let _ = close_handle(token);
    if ok == 0 {
        return None;
    }
    let label = unsafe { &*(buffer.as_ptr().cast::<TokenMandatoryLabel>()) };
    if label.label.sid.is_null() {
        return None;
    }
    let sid = unsafe { &*(label.label.sid.cast::<SidHeader>()) };
    let count = usize::from(sid.sub_authority_count);
    if count == 0 {
        return None;
    }
    Some(unsafe { *sid.sub_authority.as_ptr().add(count - 1) })
}

/// Explain the one host condition that reads like a permissions bug but is not.
///
/// A process runs at the integrity level of its own image when that label is
/// *lower* than its parent's. An image under a directory labelled `Low` (very
/// commonly the whole tree a browser extracted a download into) therefore runs
/// `Low`, and a Low process cannot write to `%LOCALAPPDATA%` — where the profile
/// service creates profiles — nor read the DACL of a Medium object. Every failure
/// this function is attached to was exactly that: `ERROR_ACCESS_DENIED` from calls
/// that have nothing to do with the command being sandboxed.
fn low_integrity_hint() -> String {
    match integrity_rid() {
        Some(rid) if rid < MEDIUM_INTEGRITY_RID => format!(
            "; this process runs at Low mandatory integrity (RID 0x{rid:04x}), which cannot write %LOCALAPPDATA% — clear the label on the directory holding this executable with `icacls <dir> /setintegritylevel (OI)(CI)M /T /C`"
        ),
        _ => String::new(),
    }
}

/// The same explanation for `Support::detail`, where it opens a sentence.
pub(crate) fn unavailable_hint() -> String {
    match integrity_rid() {
        Some(rid) if rid < MEDIUM_INTEGRITY_RID => format!(
            " (this process runs at Low mandatory integrity, RID 0x{rid:04x}, which cannot \
             create a profile in %LOCALAPPDATA%; run the binary from a directory without a Low \
             integrity label)"
        ),
        _ => String::new(),
    }
}

/// Adopt the profile if it exists; create it if it does not.
fn sid_for(profile: &str) -> CoreResult<Sid> {
    let name = wide(OsStr::new(profile));
    // Adopt an existing profile without asking the profile service to make one.
    // Deriving is a lookup and does not have the failure mode described below;
    // creating does. Checking the folder first is what turns "the profile exists
    // but the service is having a bad minute" into a working command.
    if profile_folder(profile).is_some_and(|folder| folder.is_dir()) {
        let mut derived: Sid = std::ptr::null_mut();
        let derived_ok =
            unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut derived) };
        if derived_ok >= 0 && !derived.is_null() {
            return Ok(derived);
        }
    }
    let display = wide(OsStr::new("forge sandbox"));
    let description = wide(OsStr::new(
        "Filesystem and network containment for forge tool processes",
    ));
    let mut last: i32 = 0;
    for attempt in 0..PROFILE_ATTEMPTS {
        let mut sid: Sid = std::ptr::null_mut();
        let created = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                description.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if created >= 0 && !sid.is_null() {
            return Ok(sid);
        }
        last = created;
        if (created as u32 & 0xFFFF) == ERROR_ALREADY_EXISTS {
            let mut derived: Sid = std::ptr::null_mut();
            let derived_ok =
                unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut derived) };
            if derived_ok >= 0 && !derived.is_null() {
                return Ok(derived);
            }
        }
        if !transient_profile_error(created) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(profile_backoff(attempt)));
    }
    Err(hresult_error(
        &format!("cannot create the package profile {profile:?}"),
        last,
    ))
}

/// 50, 100, 200, 400, 800, 1500, 1500, 1500 — about 8 s of patience in total.
///
/// A busy profile service was measured to refuse for longer than a second: a batch
/// of six fresh workspaces would all succeed, then minutes later every fresh
/// workspace would fail, with no code change in between. Since a refusal here means
/// "no command may run in this session", waiting out a hiccup is always cheaper than
/// the alternative — and this path only runs the first time a workspace is used,
/// because an existing profile is adopted by lookup instead (see `package_sid`).
fn profile_backoff(attempt: usize) -> u64 {
    let step = 50u64 << attempt.min(4);
    step.min(1500)
}

/// The profile service reports these when it is busy rather than when the request
/// is wrong, so they are worth retrying.
fn transient_profile_error(hresult: i32) -> bool {
    const ERROR_SHARING_VIOLATION: u32 = 32;
    let code = hresult as u32 & 0xFFFF;
    code == ERROR_ACCESS_DENIED || code == ERROR_PATH_NOT_FOUND || code == ERROR_SHARING_VIOLATION
}

/// Grant `sid` access to `path` and the traverse rights it needs on every ancestor.
///
/// The ancestors matter as much as the target: on Windows a container opening
/// `C:\Users\Ann\AppData\Local\Temp\scratch.txt` walks each component, and a component
/// with no ACE for the package SID answers "permission denied" for the whole path. That
/// is why granting `%TEMP%` alone produced `failed to stat 'C:/Users/Ann/AppData/Local':
/// Permission denied` from a real `git`. Traverse is granted non-inherited and read-only,
/// so it lets the child *reach* a path without letting it *enumerate* the parent.
fn grant_path(path: &Path, sid: Sid, access: u32) -> CoreResult<()> {
    let mut ancestor = path.parent();
    while let Some(directory) = ancestor {
        // Never touch a volume root. Rewriting `E:\`'s DACL measured 160 s, because
        // Windows re-evaluates inheritance for every object on the volume; a root also
        // already grants the read a container needs to cross it. If some volume root
        // does deny it, the command fails loudly and the workaround is to name that
        // directory in `sandboxAccess.read` — a slow surprise is worse than a clear
        // refusal.
        if directory.parent().is_none() {
            debug_log(&format!(
                "traverse {} skipped: volume root",
                directory.display()
            ));
            ancestor = directory.parent();
            continue;
        }
        // A failure here is not fatal on its own: some system directories refuse DACL
        // edits to an unprivileged caller while still being reachable for the
        // container, and the target grant below is what decides whether the command
        // runs at all. Say so, then continue.
        let started = std::time::Instant::now();
        let step = grant_entry(directory, sid, ACCESS_TRAVERSE, false);
        let elapsed = started.elapsed();
        debug_log(&format!(
            "traverse {} -> {} in {} ms",
            directory.display(),
            step.is_ok(),
            elapsed.as_millis()
        ));
        // Rewriting a large directory's DACL makes Windows re-evaluate inheritance for
        // its whole subtree: measured 9 s for %TEMP%, 20 s for AppData and 32 s for the
        // profile root. That is paid once per directory per identity, so say what the
        // pause is instead of looking hung.
        if elapsed.as_millis() > 2000 {
            eprintln!(
                "forge: sandbox: granting read access to {} for the first time ({} ms)",
                directory.display(),
                elapsed.as_millis()
            );
        }
        if let Err(error) = step {
            if std::env::var_os("FORGE_SANDBOX_DEBUG").is_some() {
                eprintln!(
                    "sandbox: appcontainer: traverse on {} skipped: {}",
                    directory.display(),
                    error.message()
                );
            }
        }
        ancestor = directory.parent();
    }
    grant_entry(path, sid, access, true)
}

/// Grant `sid` access to `path` by appending one inheritable ACE to its DACL.
///
/// The existing DACL is preserved: `SetEntriesInAclW` merges the new entry into a
/// copy of it, so the owner keeps their own rights. Nothing is revoked — the
/// container's token is what makes this a restriction, not the ACL.
fn grant(path: &Path, sid: Sid, access: u32) -> CoreResult<()> {
    grant_path(path, sid, access)
}

/// Whether `path`'s DACL already lets `sid` do `access`.
///
/// This asks the ACL, not a cache of our own: the grant is durable, so the answer
/// has to survive a restart, and only the ACL knows that.
fn already_granted(path: &Path, sid: Sid, access: u32) -> bool {
    let mut object = wide(path.as_os_str());
    let mut dacl: Acl = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    let read = unsafe {
        GetNamedSecurityInfoW(
            object.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if read != 0 {
        return false;
    }
    let trustee = TrusteeW {
        multiple_trustee: std::ptr::null_mut(),
        multiple_trustee_operation: 0,
        trustee_form: TRUSTEE_IS_SID,
        trustee_type: TRUSTEE_IS_UNKNOWN,
        name: sid.cast::<u16>(),
    };
    let mut rights: u32 = 0;
    let status = unsafe { GetEffectiveRightsFromAclW(dacl, &trustee, &mut rights) };
    if !descriptor.is_null() {
        unsafe { LocalFree(descriptor) };
    }
    status == 0 && (rights & access) == access
}

fn grant_entry(path: &Path, sid: Sid, access: u32, inheritable: bool) -> CoreResult<()> {
    // Already granted by an earlier command on this machine: leave the DACL alone.
    // The write itself is what costs seconds per directory (see
    // `GetEffectiveRightsFromAclW`'s comment), and the ACE survives process restarts.
    if already_granted(path, sid, access) {
        return Ok(());
    }
    let mut object = wide(path.as_os_str());
    let mut old_dacl: Acl = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    let read = unsafe {
        GetNamedSecurityInfoW(
            object.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if read != 0 {
        return Err(Error::new(
            ErrorKind::Io,
            format!(
                "sandbox: windows appcontainer: cannot read the DACL of {}: {}",
                path.display(),
                io::Error::from_raw_os_error(read as i32)
            ),
        ));
    }

    let entry = ExplicitAccessW {
        access_permissions: access,
        access_mode: GRANT_ACCESS,
        inheritance: if inheritable {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        trustee: TrusteeW {
            multiple_trustee: std::ptr::null_mut(),
            multiple_trustee_operation: 0,
            trustee_form: TRUSTEE_IS_SID,
            trustee_type: TRUSTEE_IS_UNKNOWN,
            name: sid.cast::<u16>(),
        },
    };

    let mut new_dacl: Acl = std::ptr::null_mut();
    let merged = unsafe { SetEntriesInAclW(1, &entry, old_dacl, &mut new_dacl) };
    let result = if merged != 0 {
        Err(Error::new(
            ErrorKind::Io,
            format!(
                "sandbox: windows appcontainer: cannot build a DACL for {}: {}",
                path.display(),
                io::Error::from_raw_os_error(merged as i32)
            ),
        ))
    } else {
        let written = unsafe {
            SetNamedSecurityInfoW(
                object.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                new_dacl,
                std::ptr::null_mut(),
            )
        };
        if written != 0 {
            Err(Error::new(
                ErrorKind::Io,
                format!(
                    "sandbox: windows appcontainer: cannot grant {} on {}: {}",
                    if access == ACCESS_WRITE {
                        "write"
                    } else {
                        "read"
                    },
                    path.display(),
                    io::Error::from_raw_os_error(written as i32)
                ),
            ))
        } else {
            Ok(())
        }
    };
    if !new_dacl.is_null() {
        unsafe { LocalFree(new_dacl) };
    }
    if !descriptor.is_null() {
        unsafe { LocalFree(descriptor) };
    }
    result
}

/// `CommandLineToArgvW`'s inverse, for the subset that matters: a quote when the
/// argument is empty or holds a space, a tab or a quote; backslashes are doubled
/// only when they precede a quote. Getting this wrong silently changes the tool's
/// argv, so it is a function with a test rather than an inline `format!`.
pub(crate) fn quote_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\x0b', '"']) {
        return argument.to_string();
    }
    let mut out = String::with_capacity(argument.len() + 2);
    out.push('"');
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            // Counted, not emitted: a run of backslashes only means something when a
            // quote follows it, so the run is flushed by whichever arm comes next.
            '\\' => {
                backslashes += 1;
                continue;
            }
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                backslashes = 0;
                out.push('"');
                continue;
            }
            _ => {}
        }
        out.extend(std::iter::repeat_n('\\', backslashes));
        backslashes = 0;
        out.push(character);
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Everything one wrapped execution needs. Mirrors `linux.rs`'s payload, which is
/// the same JSON the Linux wrapper parses.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WrapperSpec {
    pub mode: String,
    pub workspace: String,
    #[serde(default)]
    pub extra_write: Vec<String>,
    /// Paths the container may read although they are outside the workspace. An
    /// AppContainer starts with nothing readable, so this is how a tool gets at its
    /// own configuration.
    #[serde(default)]
    pub extra_read: Vec<String>,
    #[serde(default)]
    pub allow_net: bool,
}

/// Run `program args…` inside an AppContainer, and return its exit code.
///
/// Public because the honest test for this is a real spawn (see
/// `tests/appcontainer.rs`), not an inspection of the attribute list. Rule for
/// callers: grant nothing you were not asked for, and never continue when the
/// package could not be created — a container that silently did not happen is the
/// downgrade this crate exists to prevent.
pub fn run_in_container(spec: &WrapperSpec, program: &OsStr, args: &[String]) -> CoreResult<u32> {
    let workspace = Path::new(&spec.workspace);
    if !workspace.is_absolute() {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!(
                "sandbox: windows appcontainer: workspace must be absolute, got \"{}\"",
                spec.workspace
            ),
        ));
    }
    debug_log(&format!("profile: workspace={}", spec.workspace));
    let sid = package_sid(&spec.workspace)?;
    debug_log("profile: obtained");

    let writable = spec.mode == "workspace-write";
    let outcome = (|| -> CoreResult<()> {
        debug_log("grant: workspace");
        // Only the workspace itself: the container already walks the components above
        // it (that is how the tests' children reach a copy of themselves in %TEMP%),
        // and granting them would rewrite the DACL of every directory up to the drive
        // root on every fresh workspace. Extra paths are different: a tool that stats
        // its way to `~/.gitconfig` needs read attributes on the ancestors too, so
        // those go through `grant_path`.
        grant_entry(
            workspace,
            sid,
            if writable { ACCESS_WRITE } else { ACCESS_READ },
            true,
        )?;
        debug_log("grant: extras");
        if writable {
            for extra in &spec.extra_write {
                let path = Path::new(extra);
                if !path.is_absolute() {
                    return Err(Error::new(
                        ErrorKind::Invalid,
                        format!(
                            "sandbox: windows appcontainer: extra write path must be absolute, got \"{extra}\""
                        ),
                    ));
                }
                grant(path, sid, ACCESS_WRITE)?;
            }
        }
        for extra in &spec.extra_read {
            let path = Path::new(extra);
            if !path.is_absolute() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!(
                        "sandbox: windows appcontainer: extra read path must be absolute, got \"{extra}\""
                    ),
                ));
            }
            grant(path, sid, ACCESS_READ)?;
        }
        Ok(())
    })();
    debug_log(&format!(
        "grants: {:?}",
        outcome.as_ref().err().map(|e| e.message().to_string())
    ));
    if let Err(error) = outcome {
        unsafe { LocalFree(sid) };
        return Err(error);
    }

    let exit = spawn(spec, sid, program, args);
    unsafe { LocalFree(sid) };
    exit
}

fn spawn(spec: &WrapperSpec, sid: Sid, program: &OsStr, args: &[String]) -> CoreResult<u32> {
    // Capabilities decide the network. No `internetClient` means the container has
    // no outbound sockets at all, which is what `allow_net == false` promises.
    let mut capabilities: Vec<SidAndAttributes> = Vec::new();
    if spec.allow_net {
        let text = wide(OsStr::new(INTERNET_CLIENT));
        let mut capability: Sid = std::ptr::null_mut();
        let converted = unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut capability) };
        if converted == 0 || capability.is_null() {
            return Err(last_error("cannot resolve the internetClient capability"));
        }
        capabilities.push(SidAndAttributes {
            sid: capability,
            attributes: 0x0000_0004, // SE_GROUP_ENABLED
        });
    }

    let security = SecurityCapabilities {
        app_container_sid: sid,
        // An empty vector's `as_mut_ptr` is a dangling well-aligned address, not
        // null; the kernel rejects that even though the count is zero.
        capabilities: if capabilities.is_empty() {
            std::ptr::null_mut()
        } else {
            capabilities.as_mut_ptr()
        },
        capability_count: capabilities.len() as u32,
        reserved: 0,
    };

    let mut size: usize = 0;
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
    if size == 0 {
        return Err(last_error("cannot size the process attribute list"));
    }
    // `usize` words give the list the pointer alignment the API documents; a byte
    // buffer would happen to work and would be wrong by the book.
    let words = size.div_ceil(std::mem::size_of::<usize>());
    let mut storage: Vec<usize> = vec![0; words];
    let list = storage.as_mut_ptr().cast::<c_void>();
    let initialized = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) };
    if initialized == 0 {
        let error = last_error("cannot initialize the process attribute list");
        return Err(error);
    }
    let updated = unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            (&security as *const SecurityCapabilities).cast::<c_void>(),
            std::mem::size_of::<SecurityCapabilities>(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if updated == 0 {
        unsafe { DeleteProcThreadAttributeList(list) };
        return Err(last_error(
            "cannot attach the security capabilities attribute",
        ));
    }

    let mut command_line = quote_argument(&program.to_string_lossy());
    for argument in args {
        command_line.push(' ');
        command_line.push_str(&quote_argument(argument));
    }
    let mut command_line = wide(OsStr::new(&command_line));

    let mut startup = StartupInfoExW::default();
    startup.startup_info.cb = std::mem::size_of::<StartupInfoExW>() as u32;
    // Hand the child our own streams only when they exist. A test binary (or a
    // harness started with a closed stdin) has null handles, and passing
    // STARTF_USESTDHANDLES with a null handle makes `CreateProcessW` fail with
    // ERROR_INVALID_PARAMETER instead of simply inheriting the console.
    let std_input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let std_output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let std_error = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    // A closed standard handle comes back as 0 *or* as INVALID_HANDLE_VALUE (-1),
    // and either one inside STARTF_USESTDHANDLES makes `CreateProcessW` fail with
    // ERROR_INVALID_PARAMETER. A test binary that only redirects stdin shows
    // exactly that: one real handle, two of them -1.
    let real = |handle: isize| handle != 0 && handle != INVALID_HANDLE_VALUE;
    let redirect = real(std_input) && real(std_output) && real(std_error);
    let mut inherits: Vec<Inheritable> = Vec::new();
    if redirect {
        startup.startup_info.flags = STARTF_USESTDHANDLES;
        startup.startup_info.std_input = std_input;
        startup.startup_info.std_output = std_output;
        startup.startup_info.std_error = std_error;
        inherits = vec![
            Inheritable::new(std_input),
            Inheritable::new(std_output),
            Inheritable::new(std_error),
        ];
    }
    startup.attribute_list = list;
    let mut process = ProcessInformation::default();

    // The child runs *in* the workspace: a bare `echo x > f` then writes somewhere
    // the container can actually reach, and a relative path means what the tool
    // said it means.
    let directory = wide(OsStr::new(&spec.workspace));

    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            i32::from(redirect),
            EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null_mut(),
            directory.as_ptr(),
            &mut startup,
            &mut process,
        )
    };
    unsafe { DeleteProcThreadAttributeList(list) };
    for inherit in &inherits {
        inherit.restore();
    }
    if created == 0 {
        return Err(last_error("cannot start the command inside the container"));
    }
    let _ = close_handle(process.thread);
    let waited = unsafe { WaitForSingleObject(process.process, INFINITE) };
    let mut code: u32 = 1;
    let read = unsafe { GetExitCodeProcess(process.process, &mut code) };
    let _ = close_handle(process.process);
    if read == 0 {
        return Err(last_error("cannot read the command's exit code"));
    }
    if waited != 0 {
        // 0 is WAIT_OBJECT_0; anything else means the wait failed, and the exit
        // code is not meaningful.
        return Err(Error::new(
            ErrorKind::Io,
            format!("sandbox: windows appcontainer: wait failed with status {waited}"),
        ));
    }
    Ok(code)
}

/// The wrapper role: called from `main`'s first statement, before any argument is
/// parsed. Returns the process exit code when this process *is* the wrapper.
pub(crate) fn helper_code() -> Option<i32> {
    let raw = std::env::var(HELPER_ENV).ok()?;
    let spec: WrapperSpec = match serde_json::from_str(&raw) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("forge: sandbox wrapper: bad spec: {error}");
            debug_log(&format!("bad spec: {error}"));
            return Some(1);
        }
    };
    // Not inherited by the tool: a `forge` spawned inside the sandbox must not
    // think it is a wrapper too.
    std::env::remove_var(HELPER_ENV);

    let mut arguments = std::env::args_os().skip(1);
    let Some(program) = arguments.next() else {
        eprintln!("forge: sandbox wrapper: no program to run");
        debug_log("no program to run");
        return Some(1);
    };
    let args: Vec<String> = arguments
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    debug_log(&format!(
        "workspace={} program={} args={args:?} mode={} read={} write={} net={}",
        spec.workspace,
        program.to_string_lossy(),
        spec.mode,
        spec.extra_read.len(),
        spec.extra_write.len(),
        spec.allow_net
    ));
    match run_in_container(&spec, &program, &args) {
        Ok(code) => {
            debug_log(&format!("exit code {code}"));
            Some(code as i32)
        }
        Err(error) => {
            // Fail closed: the caller's contract is "either confined or refused".
            eprintln!("forge: sandbox: {}", error.message());
            debug_log(&format!("refused: {}", error.message()));
            Some(1)
        }
    }
}

/// Append one line to `%TEMP%/forge-sandbox-wrapper.log` when `FORGE_SANDBOX_DEBUG`
/// is set, and do nothing otherwise.
///
/// The wrapper's stderr is not always a place a message survives: a test harness, a
/// service manager or a closed pipe discards it, and all that remains is a command
/// that never ran. A file is the one channel that works in every one of those cases.
fn debug_log(line: &str) {
    if std::env::var_os("FORGE_SANDBOX_DEBUG").is_none() {
        return;
    }
    eprintln!("forge: sandbox debug: {line}");
    let path = std::env::temp_dir().join("forge-sandbox-wrapper.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// Whether this machine can create an AppContainer at all. Used by the enforcer
/// so `support()` describes the host instead of a constant, and by `forge doctor`.
///
/// Deriving is a capability probe, not a state change: it answers "does this
/// Windows know what an AppContainer is" without leaving a profile behind, and it
/// cannot race. Creating a profile here used to be the probe, and it made
/// `support()` intermittently claim the host had no container at all: several test
/// processes asked for a profile named after the shared temp directory at the same
/// moment, one of them got `ERROR_PATH_NOT_FOUND`, and the enforcer then reported
/// `filesystem: false` on a machine that builds containers fine.
pub fn available() -> bool {
    // A Low-integrity process cannot write `%LOCALAPPDATA%`, so the profile service
    // refuses it every time. Reporting the mechanism as present would make the
    // strict modes claim enforcement they cannot deliver on this host, so it is
    // reported as absent; the hint travels with the refusal (see `low_integrity_hint`).
    if integrity_rid().is_some_and(|rid| rid < MEDIUM_INTEGRITY_RID) {
        return false;
    }
    let probe = wide(OsStr::new("forge.sandbox.probe"));
    let mut sid: Sid = std::ptr::null_mut();
    let derived = unsafe { DeriveAppContainerSidFromAppContainerName(probe.as_ptr(), &mut sid) };
    if derived < 0 || sid.is_null() {
        return false;
    }
    unsafe { LocalFree(sid) };
    true
}

/// Which profiles look like ours, and what deleting one did.
///
/// The profile service has no "list" call, so the folder name is the only index that
/// exists: `%LOCALAPPDATA%\Packages\<name>` is created by the service itself, which is
/// also why this cannot delete a profile that was never created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneReport {
    /// Profile names that were found.
    pub found: usize,
    /// Names the service accepted a delete for.
    pub removed: Vec<String>,
    /// `(name, hr)` for the ones it refused.
    pub failed: Vec<(String, i32)>,
    /// True when pruning stopped because the service was refusing everything.
    ///
    /// The profile service goes through windows (measured, minutes long) where it
    /// refuses *every* create and delete with `0x80070005`. Walking all N names in
    /// that window costs seconds each and always ends the same way, so the first
    /// refusal that survives the retries ends the run and says why.
    pub stopped_early: bool,
}

impl PruneReport {
    /// True when every profile that was found is gone.
    pub fn clean(&self) -> bool {
        self.failed.is_empty() && !self.stopped_early
    }
}

/// Delete every package profile this crate created for this user.
///
/// Leaving one profile per workspace behind is the price of per-workspace containment,
/// and on a machine that has seen many workspaces the profile service starts refusing
/// new names (see `package_sid`). Pruning is therefore maintenance, not cleanup for
/// tidiness: it is how the strict path becomes available again.
pub fn prune() -> CoreResult<PruneReport> {
    let local = match std::env::var_os("LOCALAPPDATA") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            return Err(Error::new(
                ErrorKind::Io,
                "sandbox: windows appcontainer: LOCALAPPDATA is not set, so the package directory is unknown",
            ))
        }
    };
    let packages = local.join("Packages");
    let entries = match std::fs::read_dir(&packages) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(Error::new(
                ErrorKind::Io,
                format!(
                    "sandbox: windows appcontainer: read {}: {error}",
                    packages.display()
                ),
            ))
        }
    };

    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(PROFILE_PREFIX) {
            names.push(name);
        }
    }
    names.sort();

    let mut report = PruneReport {
        found: names.len(),
        removed: Vec::new(),
        failed: Vec::new(),
        stopped_early: false,
    };
    for name in names {
        let deleted = delete_profile(&name);
        if deleted >= 0 {
            report.removed.push(name);
            continue;
        }
        let refused_wholesale = report.removed.is_empty() && transient_profile_error(deleted);
        report.failed.push((name, deleted));
        if refused_wholesale {
            // One probe is enough: the first name failed after the full retry budget
            // and nothing has succeeded, so this is the service refusing, not this
            // profile being stubborn. Repeating that 56 more times only wastes the
            // operator's time in a way that looks like work.
            report.stopped_early = true;
            break;
        }
    }
    Ok(report)
}

/// Delete one profile, waiting out the service when it is merely busy.
///
/// `DeleteAppContainerProfile` refuses with the same transient errors that
/// `CreateAppContainerProfile` does (measured: 57 profiles answered
/// `0x80070005` in a window where nothing could be created either, and the same
/// command removed 106 of them half an hour earlier). Without the retry the
/// operator reads a hard failure and concludes the leftovers are permanent —
/// which is the one wrong conclusion, since retrying is what makes the strict
/// path available again.
fn delete_profile(name: &str) -> i32 {
    let wide_name = wide(OsStr::new(name));
    let mut last = 0;
    for attempt in 0..PROFILE_ATTEMPTS {
        last = unsafe { DeleteAppContainerProfile(wide_name.as_ptr()) };
        if last >= 0 {
            return last;
        }
        if !transient_profile_error(last) {
            return last;
        }
        if attempt + 1 < PROFILE_ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(profile_backoff(attempt)));
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::{available, quote_argument, run_in_container, PruneReport, WrapperSpec};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_prune_that_stopped_early_is_not_reported_as_clean() {
        // One profile left because the service refused is exactly as unfinished as
        // fifty-seven: `clean()` answers "may the strict path be used again", and it
        // may not. The early stop is a courtesy to the operator, not a success.
        let stopped = PruneReport {
            found: 57,
            removed: Vec::new(),
            failed: vec![("forge.sandbox.aa".to_string(), -2147024891)],
            stopped_early: true,
        };
        assert!(!stopped.clean());
        let finished = PruneReport {
            found: 1,
            removed: vec!["forge.sandbox.aa".to_string()],
            failed: Vec::new(),
            stopped_early: false,
        };
        assert!(finished.clean());
    }

    #[test]
    fn arguments_are_quoted_the_way_commandlinetoargvw_parses_them() {
        assert_eq!(quote_argument("plain"), "plain");
        assert_eq!(quote_argument(""), "\"\"");
        assert_eq!(quote_argument("has space"), "\"has space\"");
        assert_eq!(quote_argument("say \"hi\""), "\"say \\\"hi\\\"\"");
        // A trailing backslash must be doubled or it would escape the closing quote.
        assert_eq!(quote_argument("dir\\"), "dir\\");
        assert_eq!(quote_argument("with space\\"), "\"with space\\\\\"");
        assert_eq!(quote_argument("a\\\"b"), "\"a\\\\\\\"b\"");
        // Backslashes away from a quote are literal, and must not be doubled: a
        // Windows path inside a quoted argument round-trips unchanged.
        assert_eq!(
            quote_argument(r#"echo x> "C:\a b\c.txt""#),
            "\"echo x> \\\"C:\\a b\\c.txt\\\"\""
        );
    }

    fn scratch(name: &str) -> PathBuf {
        // `CARGO_TARGET_TMPDIR` only exists for integration tests, so the target
        // directory is derived from the test binary: `<target>/<profile>/deps/x.exe`.
        let target = std::env::current_exe()
            .expect("current exe")
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("target dir")
            .to_path_buf();
        let root = target.join("tmp-appcontainer").join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch dir");
        root
    }

    fn cmd_exe() -> PathBuf {
        std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
    }

    fn spec(root: &Path) -> WrapperSpec {
        WrapperSpec {
            mode: "workspace-write".to_string(),
            workspace: root.display().to_string(),
            extra_write: Vec::new(),
            extra_read: Vec::new(),
            allow_net: false,
        }
    }

    /// The claim this module makes is "a child inside the container can write the
    /// workspace", so the test is a child inside the container writing the
    /// workspace. A test that only inspected the attribute list would pass with the
    /// container never applied.
    #[test]
    fn a_command_inside_the_container_writes_its_workspace() {
        if !available() {
            eprintln!("skipping: this host cannot create an AppContainer");
            return;
        }
        let root = scratch("inside");
        let code = run_in_container(
            &spec(&root),
            OsStr::new(&cmd_exe()),
            &["/c".to_string(), "echo container> hello.txt".to_string()],
        )
        .expect("spawn inside the container");
        assert_eq!(code, 0, "cmd should succeed inside the workspace");
        assert_eq!(
            std::fs::read_to_string(root.join("hello.txt"))
                .expect("the workspace write should have landed")
                .trim(),
            "container"
        );
    }

    /// The other half of the claim: a path that was not granted is not writable,
    /// behaviourally — the process really is denied, not merely "reported denied".
    #[test]
    fn a_command_inside_the_container_cannot_write_a_denied_directory() {
        if !available() {
            eprintln!("skipping: this host cannot create an AppContainer");
            return;
        }
        let root = scratch("outside-denied");
        let sibling = root.parent().expect("parent").join("outside-denied-target");
        let _ = std::fs::remove_dir_all(&sibling);
        std::fs::create_dir_all(&sibling).expect("create sibling dir");
        let target = sibling.join("denied.txt");

        let command = format!("echo nope> \"{}\"", target.display());
        let code = run_in_container(
            &spec(&root),
            OsStr::new(&cmd_exe()),
            &["/c".to_string(), command],
        )
        .expect("spawn inside the container");
        assert_ne!(code, 0, "cmd must fail to redirect outside the workspace");
        assert!(
            !target.exists(),
            "{} must not exist after a denied write",
            target.display()
        );
        let _ = std::fs::remove_dir_all(&sibling);
    }
}
