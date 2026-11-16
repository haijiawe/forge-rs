//! Containment for the processes the harness starts: Windows job objects,
//! Linux Landlock, macOS `sandbox-exec`.
//!
//! Layer L1. This crate knows about the operating system and nothing about
//! sessions, tools or models. Its job is to answer one question honestly: what
//! can this host actually enforce, and did we install it?
//!
//! The fail-closed rule is the reason the crate exists. `full-access` returns a
//! no-op enforcer that says it enforces nothing; every other mode either
//! installs a real restriction or returns an error. A mode name must never
//! describe a guarantee the kernel was not given.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::fmt;
use std::path::{Component, Path, PathBuf};

use forge_core::Result as CoreResult;
pub use forge_core::{Error, ErrorKind};

mod noop;
mod resolve;

#[cfg(target_os = "windows")]
mod job_windows;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
mod darwin;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod unsupported;

/// Stable text for an invalid [`Spec`].
pub const ERR_INVALID_SPEC: &str = "sandbox: invalid spec";
/// Stable text for a host that cannot enforce the requested mode.
pub const ERR_UNAVAILABLE: &str = "sandbox: restrictions unavailable on this host";
/// Stable text for use of an enforcer after [`Enforcer::close`].
pub const ERR_CLOSED: &str = "sandbox: enforcer is closed";

/// The three policy names from the config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// No writes at all.
    ReadOnly,
    /// Writes only below the workspace (plus explicitly named extra paths).
    WorkspaceWrite,
    /// No restrictions; the child has the harness's own privileges.
    FullAccess,
}

impl Mode {
    /// The config spelling. This exact string is what appears in config files
    /// and in `forge doctor` output, so it is part of the contract.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ReadOnly => "read-only",
            Mode::WorkspaceWrite => "workspace-write",
            Mode::FullAccess => "full-access",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Parse a config spelling. Returns `None` for anything else: a typo must be an
/// invalid spec, not silently the most permissive mode.
pub fn mode_from_str(raw: &str) -> Option<Mode> {
    match raw {
        "read-only" => Some(Mode::ReadOnly),
        "workspace-write" => Some(Mode::WorkspaceWrite),
        "full-access" => Some(Mode::FullAccess),
        _ => None,
    }
}

/// One execution's requested guarantee.
///
/// `workspace` must be absolute; it is resolved (symlinks included) by [`new`],
/// because a rule attached to a symlink applies to its target and the target is
/// normally the broader thing. Guessing in the other direction would be a
/// sandbox that is quietly wider than the caller asked for.
#[derive(Debug, Clone)]
pub struct Spec {
    pub mode: Mode,
    pub workspace: String,
    pub allow_net: bool,
    pub extra_write: Vec<String>,
}

impl Spec {
    /// A spec with no workspace and no extra paths. Callers fill in what the
    /// mode needs; `full-access` deliberately ignores both.
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            workspace: String::new(),
            allow_net: false,
            extra_write: Vec::new(),
        }
    }
}

/// What a host can guarantee. The zero value guarantees nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Support {
    /// Can restrict write paths.
    pub filesystem: bool,
    /// Can restrict network sockets.
    pub network: bool,
    /// Can guarantee descendants die with the parent.
    pub process_tree: bool,
    /// Whether a restriction is actually installed right now. `false` from
    /// [`available`] is a statement about that call, not about the machine.
    pub enforced: bool,
    /// Human-readable explanation, used by `forge doctor` and error messages.
    pub detail: String,
}

/// The logger the sandbox uses for the two decisions an operator must see:
/// "workspace-write is partially enforced" and "no restrictions installed".
pub trait Logger: Send + Sync {
    fn debug(&self, message: &str);
    fn warn(&self, message: &str);
}

/// A logger that discards everything, for callers that do not configure one.
#[derive(Debug, Default)]
pub struct NoopLogger;

impl Logger for NoopLogger {
    fn debug(&self, _message: &str) {}
    fn warn(&self, _message: &str) {}
}

/// One installed restriction, reusable across many executions.
///
/// Implementations must be idempotent and safe to call from several threads:
/// the tool layer can run concurrent commands under one enforcer.
pub trait Enforcer: Send + Sync {
    /// Stable name used in errors and `forge doctor`.
    fn name(&self) -> &str;

    /// What this enforcer actually installed.
    fn support(&self) -> Support;

    /// Install the restriction on a command that is about to be spawned.
    /// Returning `Err` means the caller must not start the process.
    fn apply(&self, command: &mut std::process::Command) -> CoreResult<()>;

    /// Release anything [`apply`](Enforcer::apply) prepared for a command that
    /// never started. Called exactly once on spawn failure, never after success.
    fn abort_apply(&self) {}

    /// The second half of the Windows handshake: adopt the just-spawned child
    /// into its job and resume it. Implementations that do not suspend the
    /// child leave this as the default no-op.
    fn after_start(&self, _pid: u32) -> CoreResult<()> {
        Ok(())
    }

    /// Release handles. Must be safe to call more than once.
    fn close(&self) -> CoreResult<()>;
}

/// The concrete fail-closed error. It carries the host's [`Support`] so a
/// caller can print the same sentence the kernel would have said, instead of
/// inventing a reason.
#[derive(Debug, Clone)]
pub struct UnavailableError {
    pub mode: Mode,
    pub support: Support,
}

impl fmt::Display for UnavailableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = if self.support.detail.is_empty() {
            "this host reports no sandbox capability"
        } else {
            self.support.detail.as_str()
        };
        write!(
            formatter,
            "sandbox: mode {:?} cannot be enforced on this host: {}",
            self.mode.as_str(),
            detail
        )
    }
}

impl std::error::Error for UnavailableError {}

fn unavailable(mode: Mode, support: Support) -> Error {
    let concrete = UnavailableError {
        mode,
        support: support.clone(),
    };
    Error::new(ErrorKind::Refused, concrete.to_string()).with_source(concrete)
}

/// Select and install an enforcer for `spec`.
///
/// Returns `Err` whenever the caller must not run the command. The rulebook is:
///
/// 1. `full-access` returns the no-op enforcer (`enforced == false`); that is a
///    decision stated out loud, not an accident.
/// 2. `read-only` requires filesystem confinement. A process-tree or memory cap
///    cannot stop `echo x > C:\Windows\foo`, so refusing is the only honest
///    answer where paths cannot be confined.
/// 3. `workspace-write` may be partially enforced when at least one real
///    dimension exists, but [`Support::filesystem`] must stay `false` and the
///    detail must name the missing dimension. The operator's next move after a
///    refusal is `full-access`, which is strictly less restricted; hiding a
///    partial guarantee is worse than reporting it.
/// 4. A platform that claims a capability and then fails to install it is an
///    error, never a silent downgrade.
pub fn new(spec: Spec, logger: Option<&dyn Logger>) -> CoreResult<Box<dyn Enforcer>> {
    new_with(spec, logger, current_platform())
}

/// [`new`] with the platform injected, so the fail-closed branches are testable
/// on every host instead of only on a machine that happens to lack a sandbox.
fn new_with(
    spec: Spec,
    logger: Option<&dyn Logger>,
    platform: &dyn Platform,
) -> CoreResult<Box<dyn Enforcer>> {
    let logger: &dyn Logger = logger.unwrap_or(&NoopLogger);
    let resolved = resolve::resolve_spec(spec)?;

    if resolved.mode == Mode::FullAccess {
        logger.debug("sandbox: no restrictions installed");
        return Ok(Box::new(noop::NoopEnforcer::new()));
    }

    let support = platform.support();
    if !enforceable(resolved.mode, &support) {
        logger.warn(&format!(
            "sandbox: requested mode is not enforceable on this host (mode: {}, detail: {})",
            resolved.mode, support.detail
        ));
        return Err(unavailable(resolved.mode, support));
    }
    if resolved.mode == Mode::WorkspaceWrite && !support.filesystem {
        logger.warn(&format!(
            "sandbox: workspace-write is partially enforced on this host (unenforced: filesystem, detail: {})",
            support.detail
        ));
    }

    let enforcer = match platform.install(&resolved, logger) {
        Ok(enforcer) => enforcer,
        Err(error) => {
            return Err(Error::new(
                ErrorKind::Refused,
                format!(
                    "sandbox: install for mode {:?}: {}: {}",
                    resolved.mode.as_str(),
                    error.message(),
                    ERR_UNAVAILABLE
                ),
            )
            .with_source(error))
        }
    };
    let got = enforcer.support();
    if !got.enforced {
        return Err(Error::new(
            ErrorKind::Refused,
            format!(
                "sandbox: install for mode {:?}: enforcer {:?} reports no enforcement (detail: {}): {}",
                resolved.mode.as_str(),
                enforcer.name(),
                got.detail,
                ERR_UNAVAILABLE
            ),
        ));
    }
    logger.debug(&format!(
        "sandbox: restrictions installed (mode: {}, enforcer: {}, detail: {})",
        resolved.mode,
        enforcer.name(),
        got.detail
    ));
    Ok(enforcer)
}

/// Report what this host can enforce without installing anything. Used by
/// `forge doctor`; `enforced` is always `false` here because nothing was
/// installed by this call.
pub fn available() -> Support {
    current_platform().support()
}

/// The mode-dependent part of the fail-closed matrix. See [`new`] rule 2/3.
fn enforceable(mode: Mode, support: &Support) -> bool {
    match mode {
        Mode::ReadOnly => support.filesystem,
        Mode::WorkspaceWrite => support.filesystem || support.process_tree,
        Mode::FullAccess => false,
    }
}

/// One host's capability, sealed behind a build target. It is a trait so the
/// fail-closed branches can be tested with a fake host.
trait Platform: Sync {
    fn support(&self) -> Support;
    fn install(&self, spec: &Spec, logger: &dyn Logger) -> CoreResult<Box<dyn Enforcer>>;
}

#[cfg(target_os = "windows")]
fn current_platform() -> &'static dyn Platform {
    &windows::PLATFORM
}

#[cfg(target_os = "linux")]
fn current_platform() -> &'static dyn Platform {
    &linux::PLATFORM
}

#[cfg(target_os = "macos")]
fn current_platform() -> &'static dyn Platform {
    &darwin::PLATFORM
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn current_platform() -> &'static dyn Platform {
    &unsupported::PLATFORM
}

/// The child side of the Linux Landlock enforcer, and a no-op everywhere else.
///
/// The harness's `main` must call this as its first statement. When the process
/// was started as a Landlock wrapper, this does not return: it restricts the
/// current thread and replaces the process image with the real command.
pub fn maybe_exec_helper() {
    #[cfg(target_os = "linux")]
    linux::maybe_exec_helper();
}

/// Whether a path is a filesystem root. `workspace-write` at `/` (or `C:\`) is
/// `full-access` with extra steps, so it must be requested by name instead.
pub(crate) fn is_filesystem_root(path: &Path) -> bool {
    let mut components = path.components();
    match components.next() {
        Some(Component::Prefix(_)) => {
            matches!(components.next(), Some(Component::RootDir)) && components.next().is_none()
        }
        Some(Component::RootDir) => components.next().is_none(),
        _ => false,
    }
}

/// Canonicalize and strip the Windows verbatim prefix. The prefix is correct for
/// the kernel but makes every human-facing `Detail` string unreadable.
pub(crate) fn canonical_path(path: &Path) -> std::io::Result<PathBuf> {
    let resolved = std::fs::canonicalize(path)?;
    #[cfg(target_os = "windows")]
    {
        let text = resolved.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(resolved)
}

/// Rewrite a command to start `program` with `prefix` arguments followed by the
/// original argv. Used by Unix enforcers that must insert a wrapper executable.
///
/// The environment is replayed explicitly: the exec runner has already applied
/// its env policy with `env_clear`, and a naive `Command::new` would inherit the
/// harness's whole environment and silently widen the child's view.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn wrap_command(
    command: &mut std::process::Command,
    program: &Path,
    prefix: &[&str],
) -> CoreResult<()> {
    let original_program = command.get_program().to_os_string();
    let original_args: Vec<std::ffi::OsString> =
        command.get_args().map(|arg| arg.to_os_string()).collect();
    let dir = command.get_current_dir().map(Path::to_path_buf);
    let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(|v| v.to_os_string())))
        .collect();

    let mut wrapped = std::process::Command::new(program);
    wrapped.args(prefix.iter().map(std::ffi::OsString::from));
    wrapped.arg(original_program);
    wrapped.args(original_args);
    if let Some(dir) = dir {
        wrapped.current_dir(dir);
    }
    wrapped.env_clear();
    for (key, value) in envs {
        match value {
            Some(value) => {
                wrapped.env(key, value);
            }
            None => {
                wrapped.env_remove(key);
            }
        }
    }
    *command = wrapped;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEnforcer {
        support: Support,
    }

    impl Enforcer for FakeEnforcer {
        fn name(&self) -> &str {
            "fake"
        }

        fn support(&self) -> Support {
            self.support.clone()
        }

        fn apply(&self, _command: &mut std::process::Command) -> CoreResult<()> {
            Ok(())
        }

        fn close(&self) -> CoreResult<()> {
            Ok(())
        }
    }

    struct FakePlatform {
        support: Support,
        install_support: Support,
        install_error: bool,
    }

    impl Platform for FakePlatform {
        fn support(&self) -> Support {
            self.support.clone()
        }

        fn install(&self, _spec: &Spec, _logger: &dyn Logger) -> CoreResult<Box<dyn Enforcer>> {
            if self.install_error {
                return Err(Error::new(ErrorKind::Refused, "boom"));
            }
            Ok(Box::new(FakeEnforcer {
                support: self.install_support.clone(),
            }))
        }
    }

    fn temp_workspace() -> String {
        let path = std::env::temp_dir().join(format!(
            "forge-sandbox-unit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&path).expect("create temp workspace");
        canonical_path(&path)
            .expect("canonical temp workspace")
            .display()
            .to_string()
    }

    fn partial_support() -> Support {
        Support {
            filesystem: false,
            network: false,
            process_tree: true,
            enforced: false,
            detail: "process tree only; filesystem is NOT confined".to_string(),
        }
    }

    fn enforced_partial_support() -> Support {
        Support {
            filesystem: false,
            network: false,
            process_tree: true,
            enforced: true,
            detail: "process tree only; filesystem is NOT confined".to_string(),
        }
    }

    #[test]
    fn mode_spellings_are_stable() {
        assert_eq!(Mode::ReadOnly.as_str(), "read-only");
        assert_eq!(Mode::WorkspaceWrite.as_str(), "workspace-write");
        assert_eq!(Mode::FullAccess.as_str(), "full-access");
        assert_eq!(mode_from_str("read-only"), Some(Mode::ReadOnly));
        assert_eq!(mode_from_str("workspace-write"), Some(Mode::WorkspaceWrite));
        assert_eq!(mode_from_str("full-access"), Some(Mode::FullAccess));
        assert_eq!(mode_from_str("readonly"), None);
        assert_eq!(mode_from_str(""), None);
    }

    #[test]
    fn full_access_is_a_stated_noop() {
        let enforcer = new(Spec::new(Mode::FullAccess), None).expect("full access always installs");
        assert_eq!(enforcer.name(), "none");
        let support = enforcer.support();
        assert!(!support.filesystem);
        assert!(!support.network);
        assert!(!support.process_tree);
        assert!(!support.enforced);
        let mut command = std::process::Command::new("unused");
        assert!(enforcer.apply(&mut command).is_ok());
        assert!(enforcer.close().is_ok());
        assert!(enforcer.close().is_ok());
        assert_eq!(
            enforcer.apply(&mut command).unwrap_err().kind(),
            ErrorKind::Conflict
        );
    }

    #[test]
    fn invalid_workspace_is_a_spec_error() {
        let mut spec = Spec::new(Mode::ReadOnly);
        spec.workspace = "relative/path".to_string();
        let error = new(spec, None).err().expect("invalid spec must fail");
        assert_eq!(error.kind(), ErrorKind::Invalid);
        assert!(error.message().contains(ERR_INVALID_SPEC));
    }

    #[test]
    fn read_only_refused_when_filesystem_cannot_be_confined() {
        let platform = FakePlatform {
            support: partial_support(),
            install_support: enforced_partial_support(),
            install_error: false,
        };
        let mut spec = Spec::new(Mode::ReadOnly);
        spec.workspace = temp_workspace();
        let error = new_with(spec, None, &platform).err().expect("must refuse");
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(error.message().contains("filesystem"));
    }

    #[test]
    fn workspace_write_refused_when_no_dimension_exists() {
        let platform = FakePlatform {
            support: Support::default(),
            install_support: Support::default(),
            install_error: false,
        };
        let mut spec = Spec::new(Mode::WorkspaceWrite);
        spec.workspace = temp_workspace();
        let error = new_with(spec, None, &platform).err().expect("must refuse");
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(error.message().contains("cannot be enforced on this host"));
    }

    #[test]
    fn workspace_write_accepts_partial_enforcement_but_reports_filesystem_false() {
        let platform = FakePlatform {
            support: partial_support(),
            install_support: enforced_partial_support(),
            install_error: false,
        };
        let mut spec = Spec::new(Mode::WorkspaceWrite);
        spec.workspace = temp_workspace();
        let enforcer = new_with(spec, None, &platform).expect("partial enforcement is accepted");
        let support = enforcer.support();
        assert!(support.enforced);
        assert!(!support.filesystem);
        assert!(support.detail.contains("filesystem"));
    }

    #[test]
    fn install_error_is_reported_as_unavailable() {
        let platform = FakePlatform {
            support: partial_support(),
            install_support: enforced_partial_support(),
            install_error: true,
        };
        let mut spec = Spec::new(Mode::WorkspaceWrite);
        spec.workspace = temp_workspace();
        let error = new_with(spec, None, &platform).err().expect("must refuse");
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(error.message().contains(ERR_UNAVAILABLE));
        assert!(error.message().contains("boom"));
    }

    #[test]
    fn enforcer_that_enforces_nothing_is_rejected() {
        let platform = FakePlatform {
            support: partial_support(),
            install_support: Support {
                detail: "nothing installed".to_string(),
                ..Support::default()
            },
            install_error: false,
        };
        let mut spec = Spec::new(Mode::WorkspaceWrite);
        spec.workspace = temp_workspace();
        let error = new_with(spec, None, &platform).err().expect("must refuse");
        assert_eq!(error.kind(), ErrorKind::Refused);
        assert!(error.message().contains("reports no enforcement"));
    }

    #[test]
    fn available_never_claims_enforcement() {
        let support = available();
        assert!(!support.enforced);
    }
}
