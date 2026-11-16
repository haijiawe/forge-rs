//! macOS containment through `sandbox-exec`.
//!
//! Seatbelt profiles are the only unprivileged sandboxing mechanism the system
//! exposes to a process that is already running. The profile is written before
//! the command starts and handed to `/usr/bin/sandbox-exec`, which applies it to
//! itself and then executes the real command. Read access is deliberately open:
//! the requested mode is about writes and network, and a read restriction that
//! is not asked for would break ordinary builds in surprising ways.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::{Mode, Platform, Spec, Support, ERR_CLOSED, ERR_UNAVAILABLE};

pub(crate) static PLATFORM: DarwinPlatform = DarwinPlatform;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub(crate) struct DarwinPlatform;

impl Platform for DarwinPlatform {
    fn support(&self) -> Support {
        match std::fs::metadata(SANDBOX_EXEC) {
            Ok(_) => Support {
                filesystem: true,
                network: true,
                process_tree: false,
                enforced: false,
                detail: format!(
                    "darwin: {SANDBOX_EXEC} available (Seatbelt profile can confine filesystem writes and network). \
                     Process-tree teardown is internal/exec's job. unverified on this host"
                ),
            },
            Err(error) => Support {
                filesystem: false,
                network: false,
                process_tree: false,
                enforced: false,
                detail: format!(
                    "darwin: {SANDBOX_EXEC} is missing ({error}); filesystem writes cannot be confined. \
                     unverified on this host"
                ),
            },
        }
    }

    fn install(
        &self,
        spec: &Spec,
        _logger: &dyn crate::Logger,
    ) -> CoreResult<Box<dyn crate::Enforcer>> {
        std::fs::metadata(SANDBOX_EXEC).map_err(|error| {
            Error::new(
                ErrorKind::Refused,
                format!("sandbox: darwin: {SANDBOX_EXEC} unavailable: {error}: {ERR_UNAVAILABLE}"),
            )
        })?;
        let profile_path = write_profile(&seatbelt_profile(spec))?;
        Ok(Box::new(SeatbeltEnforcer {
            profile_path,
            closed: Mutex::new(false),
        }))
    }
}

fn write_profile(profile: &str) -> CoreResult<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path =
        std::env::temp_dir().join(format!("forge-sandbox-{}-{stamp}.sb", std::process::id()));
    std::fs::write(&path, profile).map_err(|error| {
        Error::new(
            ErrorKind::Refused,
            format!(
                "sandbox: darwin: cannot write seatbelt profile {}: {error}: {ERR_UNAVAILABLE}",
                path.display()
            ),
        )
    })?;
    Ok(path)
}

fn escape_profile_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            other => escaped.push(other),
        }
    }
    escaped
}

fn seatbelt_profile(spec: &Spec) -> String {
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-exec* process-fork signal)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow file-read*)\n\
         (allow file-write* (literal \"/dev/null\") (literal \"/dev/stdout\") \
         (literal \"/dev/stderr\") (literal \"/dev/stdin\") (literal \"/dev/tty\"))\n",
    );
    if spec.mode == Mode::WorkspaceWrite {
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            escape_profile_string(&spec.workspace)
        ));
        for path in &spec.extra_write {
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                escape_profile_string(path)
            ));
        }
    }
    if spec.allow_net {
        profile.push_str("(allow network*)\n");
    }
    profile
}

struct SeatbeltEnforcer {
    profile_path: PathBuf,
    closed: Mutex<bool>,
}

impl crate::Enforcer for SeatbeltEnforcer {
    fn name(&self) -> &str {
        "darwin-sandbox-exec"
    }

    fn support(&self) -> Support {
        Support {
            filesystem: true,
            network: true,
            process_tree: false,
            enforced: true,
            detail:
                "darwin sandbox-exec: seatbelt profile confines filesystem writes and network. \
                     Process-tree teardown is internal/exec's job. unverified on this host"
                    .to_string(),
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
        let profile = self.profile_path.to_string_lossy().into_owned();
        crate::wrap_command(command, Path::new(SANDBOX_EXEC), &["-f", profile.as_str()])
    }

    fn close(&self) -> CoreResult<()> {
        let mut closed = self
            .closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *closed {
            return Ok(());
        }
        *closed = true;
        match std::fs::remove_file(&self.profile_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::new(
                ErrorKind::Io,
                format!(
                    "sandbox: darwin: cannot remove seatbelt profile {}: {error}",
                    self.profile_path.display()
                ),
            )),
        }
    }
}
