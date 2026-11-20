//! What a worker is, and the two ways this crate can run one.
//!
//! The crate does not know what a model is. [`WorkerRunner`] is the only seam:
//! one method, given a [`Call`], returns a [`WorkerResult`]. That is deliberate
//! and it is the same split `hearth-subagent` uses with its `Runner`, for the same
//! reason — the concurrency, the board and the mailbox must be testable without
//! a provider, a network or a token budget.
//!
//! Two implementations ship:
//!
//! * [`RecordingRunner`] — a test double. It remembers what it was called with
//!   and counts how many workers were inside it at once, so a test can assert
//!   the dispatcher's concurrency bound as a measured invariant instead of a
//!   timing guess.
//! * [`CommandRunner`] — shells out through `hearth-exec`, so a team can dispatch
//!   real processes today: one program per worker, the prompt on stdin. It is
//!   the honest fallback until whoever embeds this crate supplies the
//!   model-backed runner.
//!
//! A third implementation, the model-backed one, belongs to the caller. Nothing
//! in this module would need to change for it: it is a trait, not an enum.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use hearth_core::{Error, ErrorKind, Result};

/// One unit of work handed to the dispatcher.
///
/// `task` is the id of a board task this worker owns. `None` means the worker has
/// no board task (a scout, a reviewer): the dispatcher then never claims anything
/// on its behalf, and its result is only a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpec {
    pub name: String,
    pub prompt: String,
    pub task: Option<String>,
}

impl WorkerSpec {
    /// A named worker with a prompt and no board task.
    pub fn new(name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            prompt: prompt.into(),
            task: None,
        }
    }

    /// Bind the worker to a task it must claim on the board before it starts.
    pub fn task(mut self, id: impl Into<String>) -> Self {
        self.task = Some(id.into());
        self
    }

    /// Is this spec usable as it stands?
    ///
    /// Checked before anything is spawned: a bad name would otherwise be
    /// discovered halfway through a dispatch, leaving a team half-started with
    /// no way to describe why.
    pub fn validate(&self) -> Result<()> {
        if !crate::valid_name(&self.name) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "team: worker name {:?} is not usable (1..={} chars of [A-Za-z0-9._-], not a Windows device name)",
                    self.name,
                    crate::MAX_NAME_LEN
                ),
            ));
        }
        if self.prompt.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: worker {:?} has an empty prompt", self.name),
            ));
        }
        if let Some(task) = &self.task {
            if task.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("team: worker {:?} names an empty task id", self.name),
                ));
            }
        }
        Ok(())
    }

    /// The prompt, always non-empty after [`WorkerSpec::validate`].
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
}

/// Everything a runner is given about one worker.
///
/// A struct rather than several parameters: adding a field later (the
/// transcript path was added exactly like this) must not break every
/// implementation.
pub struct Call<'a> {
    /// The worker's spec: name, prompt, task id.
    pub spec: &'a WorkerSpec,
    /// The task as it looked just after the successful claim, or `None`.
    ///
    /// A snapshot: the board may have moved on by the time the runner looks, and
    /// a runner that needs the current state can ask the board.
    pub task: Option<crate::Task>,
    /// The team root (`<root>/board.json` lives here).
    pub root: &'a std::path::Path,
    /// `<root>/workers/<name>.jsonl` — this worker's own transcript file.
    ///
    /// It is not a session log: nothing in `hearth-session` appends to it and no
    /// session id refers to it. Only one worker thread writes it, which is why no
    /// lock is needed and why law 1 ("one writer per log") is not bent.
    pub transcript: &'a std::path::Path,
    /// Messages drained from this worker's inbox just before the run started.
    ///
    /// Handed over here rather than left in the file so the runner does not have
    /// to know the mailbox exists. A message that arrives *during* the run is not
    /// in this list; it is delivered by the next dispatch, which is what makes a
    /// worker's starting state reproducible from the transcript.
    pub mail: Vec<crate::Message>,
}

impl Call<'_> {
    /// Shorthand for `call.spec.name`.
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// Shorthand for `call.spec.prompt`.
    pub fn prompt(&self) -> &str {
        &self.spec.prompt
    }
}

/// What a worker produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerResult {
    /// Did the worker succeed? `false` here is a fact, never an error: the team
    /// reports failures and keeps running, so `Ok(WorkerResult { ok: false })`
    /// and `Err(..)` differ only in who gets blamed.
    pub ok: bool,
    /// The child's exit status when one was started. `None` for a runner that
    /// runs no process.
    pub exit: Option<i32>,
    /// One line for a human: what happened, or why it did not.
    pub note: String,
    /// The worker's answer or output, possibly clipped by the runner.
    pub text: String,
}

impl WorkerResult {
    /// A success with a note.
    pub fn ok(note: impl Into<String>) -> Self {
        Self {
            ok: true,
            exit: Some(0),
            note: note.into(),
            text: String::new(),
        }
    }

    /// A failure with a note.
    pub fn failed(note: impl Into<String>) -> Self {
        Self {
            ok: false,
            exit: Some(1),
            note: note.into(),
            text: String::new(),
        }
    }

    /// Attach the child's exit status.
    pub fn exit(mut self, code: i32) -> Self {
        self.exit = Some(code);
        self
    }

    /// Attach the worker's output.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = text.into();
        self
    }
}

/// The one seam between this crate and whatever actually runs a worker.
///
/// `Send + Sync` because the dispatcher hands one `&dyn WorkerRunner` to several
/// worker threads at once. An implementation that cannot be shared cannot be
/// used here; an implementation that must serialise can lock internally, which
/// is the right place for that decision because only it knows whether overlap is
/// safe.
pub trait WorkerRunner: Send + Sync {
    /// Run one worker to completion.
    ///
    /// Returning `Err` means the runner itself failed (no program, no provider)
    /// rather than the worker doing badly; the dispatcher turns either into a
    /// failed worker, but the note names which.
    fn run(&self, call: &Call<'_>) -> Result<WorkerResult>;
}

/// One call a [`RecordingRunner`] saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenCall {
    pub name: String,
    pub prompt: String,
    pub task: Option<String>,
}

#[derive(Debug, Default)]
struct Presence {
    inside: usize,
    max: usize,
}

/// What a [`RecordingRunner`] does on each call.
///
/// A named alias rather than an inline `Box<dyn Fn..>` because the type has to
/// carry its own higher-ranked lifetime: the closure is called with a `&Call`
/// that borrows a different worker every time, so writing the bound inline at
/// every use site is noise a reader has to re-derive.
type Behavior = Box<dyn for<'a> Fn(&Call<'a>) -> Result<WorkerResult> + Send + Sync>;

/// A runner that records what it was asked to do.
///
/// Its counting is the part that matters: `inside` is incremented on entry and
/// decremented on exit, so the maximum is a measured fact about how many workers
/// really overlapped rather than an inference from elapsed time.
pub struct RecordingRunner {
    behavior: Behavior,
    seen: Mutex<Vec<SeenCall>>,
    presence: Mutex<Presence>,
    delay: Duration,
}

impl RecordingRunner {
    /// A runner whose behaviour is the given closure.
    pub fn from_fn<F>(behavior: F) -> Self
    where
        F: Fn(&Call<'_>) -> Result<WorkerResult> + Send + Sync + 'static,
    {
        Self {
            behavior: Box::new(behavior),
            seen: Mutex::new(Vec::new()),
            presence: Mutex::new(Presence::default()),
            delay: Duration::ZERO,
        }
    }

    /// A runner that always succeeds.
    pub fn succeeding() -> Self {
        Self::from_fn(|call| Ok(WorkerResult::ok(format!("{} finished", call.name()))))
    }

    /// A runner that always fails, with a fixed note.
    pub fn failing(note: impl Into<String>) -> Self {
        let note = note.into();
        Self::from_fn(move |_| Ok(WorkerResult::failed(note.clone())))
    }

    /// Spend this long inside the runner, so overlap is observable.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Every call seen, in the order the calls started.
    pub fn seen(&self) -> Vec<SeenCall> {
        lock(&self.seen).clone()
    }

    /// How many calls were made in total.
    pub fn calls(&self) -> usize {
        lock(&self.seen).len()
    }

    /// The largest number of workers that were inside the runner at one time.
    pub fn max_inside(&self) -> usize {
        lock(&self.presence).max
    }

    /// How many workers are inside the runner right now (0 once the run ended).
    pub fn current_inside(&self) -> usize {
        lock(&self.presence).inside
    }

    fn enter(&self) {
        let mut presence = lock(&self.presence);
        presence.inside += 1;
        presence.max = presence.max.max(presence.inside);
    }

    fn leave(&self) {
        lock(&self.presence).inside -= 1;
    }
}

impl WorkerRunner for RecordingRunner {
    fn run(&self, call: &Call<'_>) -> Result<WorkerResult> {
        lock(&self.seen).push(SeenCall {
            name: call.name().to_string(),
            prompt: call.prompt().to_string(),
            task: call.spec.task.clone(),
        });
        self.enter();
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let outcome = (self.behavior)(call);
        self.leave();
        outcome
    }
}

/// Runs a program per worker through `hearth-exec`.
///
/// The prompt goes in on **stdin**, not on the command line: a prompt is
/// arbitrarily large and arbitrarily full of quotes, and the argv limit plus the
/// shell-quoting question are two problems not worth having. Nothing is expanded
/// by a shell either — `hearth-exec` spawns the program directly.
pub struct CommandRunner {
    program: String,
    args: Vec<String>,
    dir: Option<String>,
    timeout: Option<Duration>,
    max_output_bytes: usize,
    env: Option<Vec<String>>,
}

impl CommandRunner {
    /// A runner for `program`, called with no fixed arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            dir: None,
            timeout: None,
            max_output_bytes: 64 * 1024,
            env: None,
        }
    }

    /// Add one fixed argument, before the prompt on stdin.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add several fixed arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Working directory for the program.
    pub fn dir(mut self, dir: impl Into<String>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    /// Whole-invocation deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Per-stream output cap.
    pub fn max_output_bytes(mut self, max: usize) -> Self {
        self.max_output_bytes = max;
        self
    }

    /// Replace the environment policy instead of using `hearth-exec`'s minimal
    /// whitelist.
    pub fn env(mut self, env: Vec<String>) -> Self {
        self.env = Some(env);
        self
    }
}

impl WorkerRunner for CommandRunner {
    fn run(&self, call: &Call<'_>) -> Result<WorkerResult> {
        if self.program.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "team: CommandRunner needs a non-empty program",
            ));
        }
        let mut command = hearth_exec::Command::new(self.program.clone())
            .args(self.args.clone())
            .stdin(call.prompt().as_bytes().to_vec())
            .max_output_bytes(self.max_output_bytes);
        if let Some(dir) = &self.dir {
            command = command.dir(dir.clone());
        }
        if let Some(timeout) = self.timeout {
            command = command.timeout(timeout);
        }
        if let Some(env) = &self.env {
            command = command.env(env.clone());
        }
        let result = hearth_exec::Pipe.run(command)?;

        // A non-zero exit is data, not an error: the team reports it and keeps
        // the other workers running.
        let ok = result.exit_code == 0;
        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        let mut note = format!("exit {}", result.exit_code);
        if result.truncated {
            note.push_str(" (output truncated)");
        }
        if let Some(line) = stderr.lines().rev().find(|line| !line.trim().is_empty()) {
            note.push_str(": ");
            note.push_str(line.trim());
        }
        Ok(WorkerResult {
            ok,
            exit: Some(result.exit_code),
            note: clip(&note, 400),
            text: clip(&stdout, 4000),
        })
    }
}

/// Cut `text` to at most `max` characters, keeping the head and marking the cut.
///
/// A report is read by a model and by a terminal; an unbounded field makes both
/// useless with one large capture. The marker keeps the cut visible so nobody
/// mistakes a prefix for the whole output.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("…(clipped)");
    out
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A deadline that a runner can hand to a blocking operation.
///
/// Exposed because [`CommandRunner`] takes a [`Duration`] and a caller composing
/// its own runner often wants the same "at most N seconds" arithmetic without
/// importing `std::time` twice.
pub fn deadline_in(seconds: u64) -> Instant {
    Instant::now() + Duration::from_secs(seconds)
}
