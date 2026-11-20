//! Delegated child agents (port of `forge-go/internal/subagent` M5 U3).
//!
//! Layer L2. A `Runner` exposes one `subagent` tool; calling it opens a fresh
//! child session log under `<data_dir>/sessions/<fresh-id>.jsonl`, runs one
//! `hearth_agent::Agent` turn there, and returns the child's last non-empty
//! assistant message. The event log is the only truth: final text and turn
//! counts are read back from the log, never taken from the aggregate result.
//!
//! No async runtime. Cancellation is an `Arc<AtomicBool>` carried by
//! [`Canceller`]; the agent polls it at stream boundaries, and the serial
//! semaphore polls it while a call is queued.

#![forbid(unsafe_code)]

pub mod archive;
pub mod authority;
pub mod backend;
pub mod cancel_file;
pub mod catalog;
pub mod child;
pub mod continuation;
pub mod descriptor;
pub mod error_code;
pub mod list;
pub mod lock;
pub mod names;
pub mod options;
pub mod providers;
pub mod runner;
pub mod structured;
pub mod timing;
pub mod tool;

pub use archive::{admit, is_archived};
pub use authority::{ancestry, check, describe_ancestry, Caller, Verdict};
pub use backend::{
    Backend, BackendOutcome, BackendRequest, ProcessBackend, BACKEND_PROTOCOL_VERSION,
    DEFAULT_BACKEND_TIMEOUT,
};
pub use catalog::{CatalogEntry, CATALOG_META, SUBAGENT_CATALOG_VERSION};
pub use child::{child_log_path, format_stamp, is_child_id, ChildRun};
pub use continuation::{
    agent_message, settlement_notice, settlement_summary, with_result_guidance, RESULT_GUIDANCE,
};
pub use descriptor::{Descriptor, Mode, ToolFilter, SUBAGENT_DESCRIPTOR_VERSION};
pub use error_code::{from_error, from_kind, Code, ALL as ERROR_CODES};
pub use list::{descendants, render_tree, resolve_name, Diagnostic, Node};
pub use lock::ChildLock;
pub use names::{
    agent_path_segments, is_agent_name, is_agent_path, join_agent_path, parent_agent_path,
    NameError, ROOT_AGENT_NAME,
};
pub use options::{
    Canceller, Clock, Logger, NoopLogger, NowFn, Options, DEFAULT_MAX_ACTIVE, DEFAULT_MAX_DEPTH,
    DEFAULT_MAX_STEPS,
};
pub use providers::Providers;
pub use runner::{CallContext, Delivery, Runner};
pub use structured::{spec as structured_spec, CODE_TERMINATED, STRUCTURED_OUTPUT_TOOL};
pub use timing::{of_child, of_log, Timing};
pub use tool::{
    spec, tool_schema, DelegateArgs, CODE_BAD_OUTPUT, CODE_DEPTH_EXCEEDED, CODE_FAILED, TOOL_NAME,
};
