//! Teams: several workers at once, one board they share, one mailbox each.
//!
//! This crate is what a single `subagent` call cannot be. `forge-subagent` is
//! serial by design — capacity one, one child at a time — because its job is
//! "delegate one self-contained task". A team is a different shape of work:
//! several workers run at the same time, they must not fight over the same
//! task, they must be able to leave each other messages, and the caller needs
//! one deterministic report at the end rather than N interleaved transcripts.
//!
//! Three pieces, deliberately separate:
//!
//! * [`board`] — the shared task board. A JSON file, rewritten through a temp
//!   file and a rename, with a compare-and-set on every transition. The CAS is
//!   the point: two workers claiming one task is the failure this crate exists
//!   to prevent, and a lock alone would let the second claim overwrite the
//!   first.
//! * [`mailbox`] — `<root>/inbox/<worker>.jsonl`, append-only, drained by a
//!   per-worker cursor. A worker that is not running still has an inbox, so a
//!   message sent before it starts waits for it.
//! * [`run`] — the dispatcher: bounded concurrency on plain `std::thread`
//!   scoped threads (no async runtime, no thread pool crate), a runner trait so
//!   this crate never learns what a model is, and a [`Report`] whose ordering
//!   is fixed so a test can compare the whole structure.
//!
//! What this crate deliberately does not own: the LLM-backed runner (the trait
//! is the seam), the event log (no session events are written here — see the
//! README on why `<root>/workers/<name>.jsonl` is a team file and not a
//! session log), and the CLI tool that exposes all of this to a model.
//!
//! Every failure inside [`run`] is data: it lands in `Report::problems` or in
//! one worker's `ok=false`, because a team that returns `Err` after three of
//! five workers finished leaves the caller with nothing to report. Only the
//! board's own I/O errors propagate as `Err`, and those are the callers' doing.

#![forbid(unsafe_code)]

pub mod board;
pub mod mailbox;
pub mod run;
pub mod worker;

pub use board::{
    Board, NewTask, Problem, ProblemKind, ScopeWarning, Status, Task, TaskPatch, BOARD_FILE,
    BOARD_VERSION,
};
pub use mailbox::{Drain, Mailbox, Message, INBOX_DIR};
pub use run::{Plan, Report, TaskCounts, WorkerReport, DEFAULT_MAX_PARALLEL, WORKERS_DIR};
pub use worker::{Call, CommandRunner, RecordingRunner, WorkerResult, WorkerRunner, WorkerSpec};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// The longest name this crate accepts for a worker or a task id.
///
/// The bound is not about taste: both names become file names
/// (`inbox/<name>.jsonl`, `workers/<name>.jsonl`), and Windows refuses paths
/// over 260 characters without long-path opt-in. 64 leaves room for the
/// directory part and the extension on every target.
pub const MAX_NAME_LEN: usize = 64;

/// Is `name` usable as a worker name or a task id?
///
/// A worker name is a file name, so this is a security boundary and not a
/// formatting preference: `..`, a separator, a drive letter or a leading dot
/// would let a plan write outside `<root>`. Refusing the whole name is
/// fail-closed; silently sanitising it would make two different plans collide
/// on one inbox.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !is_windows_device_name(name)
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Is this one of Windows' reserved device names?
///
/// `CON.jsonl`, `NUL.jsonl` and `COM1.jsonl` are not files: the OS opens a
/// device instead, so a worker called `con` would have a mailbox that silently
/// discarded its messages. The check is unconditional (not `cfg!(windows)`)
/// because a team root can be a network share or a copied directory, and a name
/// that is unsafe on one target is not worth keeping safe only on that target.
fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Epoch milliseconds for message and transcript timestamps.
///
/// A clock that cannot be read is reported as `0` rather than as an error: a
/// timestamp is context for a human reading the file, and losing it must not
/// lose the message it describes.
pub(crate) fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(0),
        Err(_) => 0,
    }
}

/// Take a lock, recovering from poisoning.
///
/// A panic in one worker must not turn every later board read into a panic of
/// its own: the file is the truth, the mutex only orders access to it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A process-local counter used to make temp file names unique.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A sibling temp path next to `path`: `board.json` becomes
/// `board.json.<pid>.<n>.tmp`.
///
/// The pid keeps two processes from sharing one temp file; the counter keeps
/// two writes in one process from doing the same even if a lock is bypassed.
pub(crate) fn temp_sibling(path: &std::path::Path) -> std::path::PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{}.tmp", std::process::id(), counter));
    path.with_file_name(name)
}

/// Write `bytes` to `path` atomically: temp file, flush, rename over.
///
/// A rename replaces the target in one step on every platform this build
/// targets, so a reader sees either the old bytes or the new ones. Truncating
/// in place would let a crash leave a half-written file, which for a board
/// means a half-written claim.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> forge_core::Result<()> {
    use std::io::Write;

    let temp = temp_sibling(path);
    {
        let mut handle = std::fs::File::create(&temp)?;
        handle.write_all(bytes)?;
        handle.flush()?;
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}
