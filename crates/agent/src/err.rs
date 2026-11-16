//! Consumer-owned error metadata.
//!
//! `forge_llm` deliberately keeps retry fields in `ProviderError`, which does
//! not implement `std::error::Error` and therefore cannot travel as the `source`
//! of a `forge_core::Error`. To keep the retry policy in this crate (and avoid
//! a dependency on any concrete provider), the scripted fake — and, later, a
//! real provider adapter — attaches [`ProviderFailure`] as the source. The
//! helpers here downcast that source to recover `code`, `retryable` and
//! `retry_after`.

use forge_core::{Error, ErrorKind};
use std::fmt;
use std::time::Duration;

/// Retry-relevant details carried as the source of a `forge_core::Error`.
#[derive(Debug, Clone)]
pub struct ProviderFailure {
    pub message: String,
    pub code: String,
    pub retryable: bool,
    pub retry_after: Duration,
    pub cancelled: bool,
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderFailure {}

/// Build a provider-shaped error with retry metadata attached.
pub fn provider_error(
    message: impl Into<String>,
    code: impl Into<String>,
    retryable: bool,
    retry_after: Duration,
) -> Error {
    let message = message.into();
    Error::new(ErrorKind::Refused, message.clone()).with_source(ProviderFailure {
        message,
        code: code.into(),
        retryable,
        retry_after,
        cancelled: false,
    })
}

/// The cancellation sentinel. Never retryable; never recorded as an `error`
/// event by the turn loop.
pub fn cancelled_error() -> Error {
    Error::new(ErrorKind::Refused, "agent: cancelled").with_source(ProviderFailure {
        message: "agent: cancelled".to_string(),
        code: "cancelled".to_string(),
        retryable: false,
        retry_after: Duration::ZERO,
        cancelled: true,
    })
}

fn failure(err: &Error) -> Option<&ProviderFailure> {
    std::error::Error::source(err)?.downcast_ref::<ProviderFailure>()
}

pub fn is_cancelled(err: &Error) -> bool {
    failure(err).map(|f| f.cancelled).unwrap_or(false)
}

pub fn code_of(err: &Error) -> &str {
    failure(err).map(|f| f.code.as_str()).unwrap_or("")
}

pub fn retryable_of(err: &Error) -> bool {
    failure(err).map(|f| f.retryable).unwrap_or(false)
}

pub fn retry_after_of(err: &Error) -> Duration {
    failure(err)
        .map(|f| f.retry_after)
        .unwrap_or(Duration::ZERO)
}

/// The internal error for a model that asked for tools none can run. The code
/// is carried as metadata so the loop can record it on the `error` event.
pub fn refusal_error(message: impl Into<String>, code: impl Into<String>) -> Error {
    let message = message.into();
    Error::new(ErrorKind::Refused, message.clone()).with_source(ProviderFailure {
        message,
        code: code.into(),
        retryable: false,
        retry_after: Duration::ZERO,
        cancelled: false,
    })
}
