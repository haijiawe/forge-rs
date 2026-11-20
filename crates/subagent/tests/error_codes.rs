//! The named error-code vocabulary: uniqueness, the frozen literals, both
//! mapping functions, and one real delegation that fails for a named reason.
//!
//! Every string here is a wire value. `as_str()` is what a tool result carries
//! in its `code` field, so a test that pins it is pinning the contract, not an
//! implementation detail.

mod common;

use std::sync::Arc;

use common::{ScriptedProvider, TempDir};
use hearth_core::{Error, ErrorKind};
use hearth_subagent::error_code::{from_error, from_kind, Code, ALL};
use hearth_subagent::{
    Backend, BackendOutcome, BackendRequest, Options, Runner, CODE_BAD_OUTPUT, CODE_DEPTH_EXCEEDED,
    CODE_FAILED, CODE_TERMINATED,
};
use hearth_tool::Call;
use serde_json::json;

#[test]
fn every_code_has_its_own_string_and_the_published_four_are_frozen() {
    let mut seen: Vec<&'static str> = Vec::new();
    for code in ALL {
        let text = code.as_str();
        assert!(
            !seen.contains(&text),
            "two codes share the string {text:?}; a wire value must name one condition"
        );
        assert!(
            text.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{text:?} is not snake_case ASCII"
        );
        seen.push(text);
    }

    // The four codes the tool result contract published first. Their literals
    // are asserted here so a future edit to `as_str()` cannot move them.
    assert_eq!(CODE_TERMINATED, "already_terminated");
    assert_eq!(CODE_FAILED, "subagent_failed");
    assert_eq!(CODE_DEPTH_EXCEEDED, "depth_exceeded");
    assert_eq!(CODE_BAD_OUTPUT, "bad_output");

    assert_eq!(Code::AlreadyTerminated.as_str(), CODE_TERMINATED);
    assert_eq!(Code::SubagentFailed.as_str(), CODE_FAILED);
    assert_eq!(Code::DepthExceeded.as_str(), CODE_DEPTH_EXCEEDED);
    assert_eq!(Code::BadOutput.as_str(), CODE_BAD_OUTPUT);
}

#[test]
fn every_error_kind_maps_to_a_code_in_the_vocabulary() {
    // `from_kind`'s match is exhaustive, so a new `ErrorKind` fails to compile
    // rather than falling through a default; this list keeps the count honest.
    let kinds = [
        ErrorKind::Io,
        ErrorKind::Json,
        ErrorKind::Invalid,
        ErrorKind::UnsupportedVersion,
        ErrorKind::NotDense,
        ErrorKind::DuplicateId,
        ErrorKind::NotFound,
        ErrorKind::Conflict,
        ErrorKind::Refused,
    ];
    assert_eq!(
        kinds.len(),
        9,
        "ErrorKind changed size: from_kind and this list must change with it"
    );
    for kind in kinds {
        let code = from_kind(kind);
        assert!(
            ALL.contains(&code),
            "{kind:?} mapped to {code:?}, which is not part of the vocabulary"
        );
    }
}

#[test]
fn real_failure_messages_are_told_apart() {
    // Messages are copied from the sites that raise them: child.rs (child-id
    // shape, missing log, closed log, one-shot child), runner.rs (provider
    // lookup, background spawn without a parent log), providers.rs (duplicate
    // registration) and tool.rs (the depth limit).
    let cases = [
        (
            "subagent: not a child id: \"nope\"",
            ErrorKind::Invalid,
            Code::InvalidChildId,
        ),
        (
            "subagent: no such child session: /tmp/s/20260101-000000-deadbeef.jsonl",
            ErrorKind::NotFound,
            Code::NotResumable,
        ),
        (
            "subagent: child 20260101-000000-deadbeef already wrote session/end and cannot be resumed",
            ErrorKind::Refused,
            Code::NotResumable,
        ),
        (
            "subagent: child 20260101-000000-deadbeef was started as one-shot and cannot be resumed",
            ErrorKind::Refused,
            Code::NotContinuable,
        ),
        (
            "subagent: no provider named \"acp\" is registered",
            ErrorKind::NotFound,
            Code::NoProvider,
        ),
        (
            "subagent: spawn needs a parent log to deliver its notice to",
            ErrorKind::Invalid,
            Code::DeliveryUnavailable,
        ),
        (
            "subagent: delegation depth limit reached (this session is at depth 2, the limit is 2). \
             Do the task here instead of delegating it.",
            ErrorKind::Refused,
            Code::DepthExceeded,
        ),
        (
            "subagent: a provider named \"worker\" is already registered",
            ErrorKind::Conflict,
            Code::ProviderChanged,
        ),
        (
            "subagent: no such model: \"no-such-model\"",
            ErrorKind::NotFound,
            Code::ModelRouteNotAllowed,
        ),
    ];
    let mut distinct: Vec<Code> = Vec::new();
    for (message, kind, want) in cases {
        let err = Error::new(kind, message);
        assert_eq!(from_error(&err), want, "message {message:?}");
        if !distinct.contains(&want) {
            distinct.push(want);
        }
    }
    assert!(
        distinct.len() >= 6,
        "the checks collapsed into {} codes: {distinct:?}",
        distinct.len()
    );
}

#[test]
fn the_specific_check_wins_when_a_message_names_several_things() {
    // "already registered" is a changed registry, not a missing provider, and
    // the message contains the word "provider" too: the order in `from_error`
    // is what keeps these apart.
    let duplicate = Error::new(
        ErrorKind::Conflict,
        "subagent: a provider named \"worker\" is already registered",
    );
    assert_eq!(from_error(&duplicate), Code::ProviderChanged);
    assert_ne!(from_error(&duplicate), Code::NoProvider);

    // A closed log and a one-shot child are both resume refusals of the same
    // kind, and the message is the only thing that separates them.
    let closed = Error::new(
        ErrorKind::Refused,
        "subagent: child 20260101-000000-deadbeef already wrote session/end and cannot be resumed",
    );
    let one_shot = Error::new(
        ErrorKind::Refused,
        "subagent: child 20260101-000000-deadbeef was started as one-shot and cannot be resumed",
    );
    assert_eq!(from_error(&closed), Code::NotResumable);
    assert_eq!(from_error(&one_shot), Code::NotContinuable);

    // Nothing recognizable in the text falls back to the kind, never to a panic.
    let opaque = Error::new(ErrorKind::Io, "the disk went away");
    assert_eq!(from_error(&opaque), from_kind(ErrorKind::Io));
}

#[test]
fn messages_are_sentences_without_a_crate_prefix() {
    for code in ALL {
        let message = code.message();
        assert!(!message.trim().is_empty(), "{code:?} has an empty message");
        assert!(
            !message.starts_with("subagent:"),
            "{code:?} repeats the crate prefix: {message:?}"
        );
        assert!(
            !message.starts_with("subagent "),
            "{code:?} names the crate instead of the condition: {message:?}"
        );
    }
}

#[test]
fn the_vocabulary_has_at_least_twenty_eight_codes() {
    assert!(
        ALL.len() >= 28,
        "only {} codes: the vocabulary lost variants",
        ALL.len()
    );
}

/// A backend that knows exactly one model. Anything else is the "that model
/// does not exist" failure a real provider reports, raised while the
/// delegation is still in setup.
struct OneModelRouter;

/// The one model [`OneModelRouter`] accepts.
const KNOWN_MODEL: &str = "known-model";

impl Backend for OneModelRouter {
    fn name(&self) -> &str {
        "one-model"
    }

    fn call(&self, request: &BackendRequest) -> hearth_core::Result<BackendOutcome> {
        let model = request.model.as_deref().unwrap_or("");
        if model == KNOWN_MODEL {
            return Ok(BackendOutcome::success(
                "20260101-000000-00000001",
                "the child ran",
                1,
                1,
            ));
        }
        Err(Error::new(
            ErrorKind::NotFound,
            format!("subagent: no such model: {model:?}"),
        ))
    }
}

#[test]
fn a_delegation_for_a_model_that_does_not_exist_reports_a_specific_code() {
    let dir = TempDir::new("error-codes-model");
    let mut opt = Options::new(
        Arc::new(ScriptedProvider::new(vec![])),
        dir.path(),
        dir.path().join("workspace").to_string_lossy().into_owned(),
        KNOWN_MODEL,
    );
    opt.providers
        .register(Arc::new(OneModelRouter))
        .expect("register the one-model router");
    opt.backend_name = Some("one-model".to_string());
    let runner = Arc::new(Runner::new(opt).expect("runner"));
    let tool = runner.tool();

    let res = tool.call(Call::new(
        "call-1",
        "subagent",
        json!({"prompt": "do the task", "model": "no-such-model"}),
        "",
    ));

    assert!(res.is_error, "expected a failure, got: {res:?}");
    assert_ne!(
        res.code, CODE_FAILED,
        "a named cause must not fall back to the generic code"
    );
    assert_eq!(res.code, Code::ModelRouteNotAllowed.as_str());
    assert!(
        res.output.contains("no such model"),
        "the cause must survive in the output: {}",
        res.output
    );
}
