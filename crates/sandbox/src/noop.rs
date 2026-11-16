//! The full-access enforcer. It installs nothing, and says so: `enforced` stays
//! false so a caller that reads `Support` instead of the mode cannot mistake
//! "no restrictions" for "restrictions installed".

use std::sync::Mutex;

use forge_core::{Error, ErrorKind, Result as CoreResult};

use crate::{Enforcer, Support, ERR_CLOSED};

/// The sentence `forge doctor` prints for full-access.
pub(crate) const FULL_ACCESS_DETAIL: &str = "no restrictions installed (full-access): the child runs with the harness's own privileges and may read, write and connect anywhere the harness can";

pub(crate) struct NoopEnforcer {
    closed: Mutex<bool>,
}

impl NoopEnforcer {
    pub(crate) fn new() -> Self {
        Self {
            closed: Mutex::new(false),
        }
    }
}

impl Enforcer for NoopEnforcer {
    fn name(&self) -> &str {
        "none"
    }

    fn support(&self) -> Support {
        Support {
            detail: FULL_ACCESS_DETAIL.to_string(),
            ..Support::default()
        }
    }

    fn apply(&self, _command: &mut std::process::Command) -> CoreResult<()> {
        let closed = self
            .closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *closed {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("sandbox: apply on \"none\": {}", ERR_CLOSED),
            ));
        }
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
