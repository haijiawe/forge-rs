//! Named failure codes for the delegation surface.
//!
//! DSH keeps a closed vocabulary of about thirty `SubagentErrorCode` strings
//! (`packages/subagent/subagent/src/error.ts`) and puts one on every failure a
//! caller might want to route on. This module is the Rust port's subset: the
//! conditions this crate can actually produce, spelled the way DSH spells them
//! once they are snake_cased, plus the four codes the tool result contract has
//! already published.
//!
//! # Why a second vocabulary
//!
//! [`ErrorKind`] answers "what class of failure was this" — I/O, a conflicting
//! id, a refusal — because that is what an error envelope carries. A caller who
//! has to *decide* something needs the condition instead: "the depth limit was
//! reached" and "the provider is gone" can both arrive as `NotFound`-adjacent
//! failures, and one means "do the work here" while the other means "retry
//! against the current registration". The code is that condition.
//!
//! # Contract
//!
//! [`Code::as_str`] is frozen once published. It is written into a tool
//! result's `code` field, where an unknown string is data a reader tolerates
//! (AGENTS.md rule 2), but a *changed* string silently changes what every log
//! that already contains it means. Renaming a code is not a cleanup; it is a
//! new code.

use forge_core::{Error, ErrorKind};

/// One machine-routable subagent failure.
///
/// Reuse a code only when the caller's action is the same. The four codes at
/// the top are the ones the tool result contract published first; their string
/// forms may not change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    // ---- already published: the literal values are frozen ----
    /// The child had already written `session/end`; the turn was not applied.
    AlreadyTerminated,
    /// The child failed, in its setup or in its loop. The message says which.
    SubagentFailed,
    /// Delegating would have exceeded the depth limit.
    DepthExceeded,
    /// A schema was required and the child finished without satisfying it.
    BadOutput,
    // ---- delegation and capacity ----
    /// No backend is registered under the requested name.
    NoProvider,
    /// The backend was removed or replaced while the delegation was starting.
    ProviderChanged,
    /// The selected backend cannot do what the request needs.
    UnsupportedCapability,
    /// The tree has no activation capacity left.
    ActivationLimitReached,
    /// The target activation is closing and admits no new work.
    ActivationClosing,
    /// Releasing an activation's resources failed.
    ActivationTeardownFailed,
    /// A child with that session id already exists.
    DuplicateChild,
    /// The delegation depth is not a non-negative integer.
    InvalidDepth,
    /// The value is not a child session id.
    InvalidChildId,
    /// The child's working directory is relative or cannot be entered.
    InvalidCwd,
    // ---- resumption and delivery ----
    /// The child cannot be restored from its persisted state.
    NotResumable,
    /// The child was not started continuable and accepts no follow-up.
    NotContinuable,
    /// Continuation needs an agent runtime or a live tree root that is absent.
    ContinuationUnavailable,
    /// Session persistence is absent or failed.
    PersistenceUnavailable,
    /// The child accepts no more input.
    InputClosed,
    /// The operation was cancelled by its caller.
    Cancelled,
    /// No delivery channel exists for the child's result.
    DeliveryUnavailable,
    /// `structured_output` was called while accepted input is still pending.
    StructuredOutputPendingInput,
    // ---- identity ----
    /// The requested agent name does not satisfy the naming rule.
    InvalidAgentName,
    /// A sibling already uses that agent name.
    DuplicateAgentName,
    /// The agent path names no member of the tree.
    AgentNameNotFound,
    // ---- model ----
    /// The child's model cannot accept image input.
    ModelDoesNotSupportImages,
    /// The requested model or provider route is not authorized here.
    ModelRouteNotAllowed,
    // ---- other ----
    /// The caller is not the live agent that owns the target.
    Unauthorized,
    // ---- archiving ----
    /// The session being admitted, or one of its ancestors, is archived.
    ArchivedAncestor,
}

impl Code {
    /// The wire form written into a tool result's `code` field.
    ///
    /// Frozen once published: see the module contract.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::AlreadyTerminated => "already_terminated",
            Code::SubagentFailed => "subagent_failed",
            Code::DepthExceeded => "depth_exceeded",
            Code::BadOutput => "bad_output",
            Code::NoProvider => "no_provider",
            Code::ProviderChanged => "provider_changed",
            Code::UnsupportedCapability => "unsupported_capability",
            Code::ActivationLimitReached => "activation_limit_reached",
            Code::ActivationClosing => "activation_closing",
            Code::ActivationTeardownFailed => "activation_teardown_failed",
            Code::DuplicateChild => "duplicate_child",
            Code::InvalidDepth => "invalid_depth",
            Code::InvalidChildId => "invalid_child_id",
            Code::InvalidCwd => "invalid_cwd",
            Code::NotResumable => "not_resumable",
            Code::NotContinuable => "not_continuable",
            Code::ContinuationUnavailable => "continuation_unavailable",
            Code::PersistenceUnavailable => "persistence_unavailable",
            Code::InputClosed => "input_closed",
            Code::Cancelled => "cancelled",
            Code::DeliveryUnavailable => "delivery_unavailable",
            Code::StructuredOutputPendingInput => "structured_output_pending_input",
            Code::InvalidAgentName => "invalid_agent_name",
            Code::DuplicateAgentName => "duplicate_agent_name",
            Code::AgentNameNotFound => "agent_name_not_found",
            Code::ModelDoesNotSupportImages => "model_does_not_support_images",
            Code::ModelRouteNotAllowed => "model_route_not_allowed",
            Code::Unauthorized => "unauthorized",
            Code::ArchivedAncestor => "archived_ancestor",
        }
    }

    /// One sentence a human can act on, without the crate's `subagent:` prefix:
    /// the prefix belongs to the message the code is attached to, and stating
    /// it twice reads as two failures.
    pub fn message(self) -> &'static str {
        match self {
            Code::AlreadyTerminated => {
                "the child had already written session/end, so this turn was not applied"
            }
            Code::SubagentFailed => "the child failed; the message says where",
            Code::DepthExceeded => {
                "delegating would exceed the depth limit; do the task in this session"
            }
            Code::BadOutput => {
                "a result schema was required, but the child finished without calling \
                 structured_output"
            }
            Code::NoProvider => "no subagent provider is registered under that name",
            Code::ProviderChanged => {
                "the provider was removed or replaced while the delegation was starting"
            }
            Code::UnsupportedCapability => "the selected backend cannot do what this request needs",
            Code::ActivationLimitReached => "the subagent tree has no activation capacity left",
            Code::ActivationClosing => "the target activation is closing and admits no new work",
            Code::ActivationTeardownFailed => "releasing the activation's resources failed",
            Code::DuplicateChild => "a child with the same session id already exists",
            Code::InvalidDepth => "the delegation depth is not a non-negative integer",
            Code::InvalidChildId => "the value is not a child session id",
            Code::InvalidCwd => "the child's working directory is relative or cannot be entered",
            Code::NotResumable => {
                "the child cannot be restored from its persisted state, so it cannot be resumed"
            }
            Code::NotContinuable => {
                "the child was not started continuable and accepts no follow-up"
            }
            Code::ContinuationUnavailable => {
                "continuation needs an agent runtime or a live tree root that is absent"
            }
            Code::PersistenceUnavailable => {
                "session persistence is absent or failed, so the child cannot be kept"
            }
            Code::InputClosed => "the child accepts no more input",
            Code::Cancelled => "the operation was cancelled by its caller",
            Code::DeliveryUnavailable => "no delivery channel is available for the child's result",
            Code::StructuredOutputPendingInput => {
                "structured_output was called while accepted input is still pending"
            }
            Code::InvalidAgentName => "the requested agent name does not satisfy the naming rule",
            Code::DuplicateAgentName => "a sibling already uses that agent name",
            Code::AgentNameNotFound => "the agent path names no member of the tree",
            Code::ModelDoesNotSupportImages => "the child's model cannot accept image input",
            Code::ModelRouteNotAllowed => {
                "this session's model selection does not authorize the requested model or provider"
            }
            Code::Unauthorized => "the caller is not the live agent that owns that target",
            Code::ArchivedAncestor => {
                "the session, or an ancestor of it, is archived and starts no new work"
            }
        }
    }
}

/// Every code this build can emit, in declaration order.
///
/// A caller that wants to enumerate the vocabulary (a UI legend, a test that
/// asserts uniqueness) reads this rather than guessing at the variants.
pub const ALL: &[Code] = &[
    Code::AlreadyTerminated,
    Code::SubagentFailed,
    Code::DepthExceeded,
    Code::BadOutput,
    Code::NoProvider,
    Code::ProviderChanged,
    Code::UnsupportedCapability,
    Code::ActivationLimitReached,
    Code::ActivationClosing,
    Code::ActivationTeardownFailed,
    Code::DuplicateChild,
    Code::InvalidDepth,
    Code::InvalidChildId,
    Code::InvalidCwd,
    Code::NotResumable,
    Code::NotContinuable,
    Code::ContinuationUnavailable,
    Code::PersistenceUnavailable,
    Code::InputClosed,
    Code::Cancelled,
    Code::DeliveryUnavailable,
    Code::StructuredOutputPendingInput,
    Code::InvalidAgentName,
    Code::DuplicateAgentName,
    Code::AgentNameNotFound,
    Code::ModelDoesNotSupportImages,
    Code::ModelRouteNotAllowed,
    Code::Unauthorized,
    Code::ArchivedAncestor,
];

/// The closest [`Code`] for an [`ErrorKind`].
///
/// Coarse on purpose: the kind is the class the rest of the tree already
/// branches on, and this mapping has to be total and deterministic so it can be
/// the fallback for [`from_error`]. The match is exhaustive — a new `ErrorKind`
/// is a decision here, not a default.
pub fn from_kind(kind: ErrorKind) -> Code {
    match kind {
        // The only I/O this crate performs on its own account is the child log
        // and the sessions directory that holds it.
        ErrorKind::Io => Code::PersistenceUnavailable,
        // Bytes that were not what they claimed to be: here that is a
        // structured result or a tool argument the child got wrong.
        ErrorKind::Json => Code::BadOutput,
        // A caller-supplied value whose shape is frozen; the child id is the
        // shape this crate defines and checks.
        ErrorKind::Invalid => Code::InvalidChildId,
        // A log written by a version this build does not know cannot be reopened.
        ErrorKind::UnsupportedVersion => Code::NotResumable,
        // Records out of order or missing: the log cannot be read back.
        ErrorKind::NotDense => Code::PersistenceUnavailable,
        // Two records share an id: the child id was handed out twice.
        ErrorKind::DuplicateId => Code::DuplicateChild,
        // The thing named does not exist; the thing this crate names is a child
        // log it was asked to resume.
        ErrorKind::NotFound => Code::NotResumable,
        // The thing named exists and the request contradicts it: a taken id.
        ErrorKind::Conflict => Code::DuplicateChild,
        // A request the callee will not guess at. The refusals this crate
        // raises are resume refusals, and "not continuable" names them.
        ErrorKind::Refused => Code::NotContinuable,
    }
}

/// The most specific [`Code`] for a concrete error.
///
/// The kind alone cannot separate "the depth limit was reached" from "the log
/// was already closed", so the message is consulted first. The checks are
/// ordered most-specific-first: a message may name several nouns ("a provider
/// named X is already registered" names both a provider and a conflict), and
/// the first match wins. Anything unrecognized falls back to [`from_kind`],
/// which is total — this function never panics.
pub fn from_error(err: &Error) -> Code {
    let text = err.message().to_lowercase();
    if text.contains("is archived") || text.contains("which is archived") {
        // Ahead of every resume refusal: "archived" is a decision the operator
        // made, and reporting it as "not continuable" would send them looking
        // for a child problem that is not there.
        Code::ArchivedAncestor
    } else if text.contains("not a child id") {
        Code::InvalidChildId
    } else if text.contains("already wrote session/end") {
        Code::NotResumable
    } else if text.contains("one-shot") {
        Code::NotContinuable
    } else if text.contains("has no descriptor") || text.contains("no such child session") {
        // The child is gone or its descriptor is: either way there is nothing
        // left to resume. The one-shot check above stays ahead of this one
        // because that refusal names a different condition on a live child.
        Code::NotResumable
    } else if text.contains("no such model")
        || text.contains("unknown model")
        || text.contains("model not found")
    {
        Code::ModelRouteNotAllowed
    } else if text.contains("already registered") {
        // Before the generic provider check: a duplicate registration is a
        // changed registry, not a missing provider.
        Code::ProviderChanged
    } else if text.contains("no provider named") || text.contains("provider") {
        Code::NoProvider
    } else if text.contains("parent log") {
        Code::DeliveryUnavailable
    } else if text.contains("cancelled") {
        Code::Cancelled
    } else if text.contains("depth") {
        Code::DepthExceeded
    } else if text.contains("agent name") {
        Code::InvalidAgentName
    } else if text.contains("working directory") || text.contains("cwd") {
        Code::InvalidCwd
    } else if text.contains("image") {
        Code::ModelDoesNotSupportImages
    } else if text.contains("schema") || text.contains("structured_output") {
        Code::BadOutput
    } else {
        from_kind(err.kind())
    }
}
