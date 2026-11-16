//! Hosts with no implemented containment mechanism.
//!
//! Returning a support matrix full of `false` is the honest answer; returning a
//! no-op enforcer with `enforced = true` would be the exact lie this crate is
//! meant to prevent. `new` therefore always fails, and `workspace-write` cannot
//! be selected just because the process-count cap on another platform exists.

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::{Platform, Spec, Support, ERR_UNAVAILABLE};

pub(crate) static PLATFORM: UnsupportedPlatform = UnsupportedPlatform;

pub(crate) struct UnsupportedPlatform;

impl Platform for UnsupportedPlatform {
    fn support(&self) -> Support {
        Support {
            filesystem: false,
            network: false,
            process_tree: false,
            enforced: false,
            detail: format!(
                "no sandbox mechanism is implemented for {}/{}: filesystem writes cannot be confined. \
                 unverified on this host",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        }
    }

    fn install(
        &self,
        _spec: &Spec,
        _logger: &dyn crate::Logger,
    ) -> CoreResult<Box<dyn crate::Enforcer>> {
        Err(Error::new(
            ErrorKind::Refused,
            format!(
                "sandbox: no sandbox mechanism is implemented for {}/{}: {ERR_UNAVAILABLE}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        ))
    }
}
