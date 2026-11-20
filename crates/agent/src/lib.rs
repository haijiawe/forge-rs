//! hearth-agent — the one-turn agent loop and transcript summarizer.
//!
//! A port of forge-go's `internal/agent` package. The event log is the only
//! truth: every turn re-projects it through `hearth-session` (invariant I1), and
//! this crate never keeps a second copy of conversation history. The tool hook
//! is consumer-owned so no `hearth-tool` dependency is needed; the W3 tool crate
//! implements [`ToolRunner`] later.

pub mod agent;
pub mod cancel;
pub mod dispatch;
pub mod err;
pub mod history;
pub mod hooks;
pub mod inbox;
pub mod payload;
pub mod summarizer;
pub mod tool;

pub use agent::{
    Agent, AttachmentHook, Clock, DeltaHook, Options, RunResult, SteeringHook, TextHook,
};
pub use cancel::{Cancel, Cause};
pub use dispatch::{Dispatch, EventView, Listener, Problem};
pub use err::{
    cancelled_error, code_of, is_cancelled, provider_error, refusal_error, retry_after_of,
    retryable_of, ProviderFailure,
};
pub use history::{next_turn, open_turn, to_llm_messages, to_llm_messages_with_parts};
pub use hooks::{is_hook_phase, Hook, HookCall, HookOutcome, Hooks, HOOK_PHASES};
pub use inbox::{Inbox, Msg, Target};
pub use payload::{
    clip_system, error_event, llm_call, normalize_stop, step_end, step_start, stop_with_calls,
    turn_end, turn_reason, turn_start, usage_payload, user_message, CODE_CANCELLED,
    MAX_SYSTEM_BYTES, STOP_CANCELLED, STOP_END_TURN, STOP_ERROR, STOP_MAX_TOKENS, STOP_TOOL_USE,
    TURN_CANCELLED, TURN_COMPLETED, TURN_ERROR,
};
pub use summarizer::{
    clip_summary, strip_thinking, ProviderSummarizer, Summary, DEFAULT_SUMMARY_BYTES,
    ERR_NO_SUMMARY_TEXT, ERR_SUMMARY_TOOL_CALL, ERR_SUMMARY_TRUNCATED,
};
pub use tool::{EventLog, ToolResult, ToolRunner};
