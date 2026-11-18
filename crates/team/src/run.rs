//! The dispatcher: a roster in, bounded concurrency, one deterministic report.
//!
//! Four rules shape this module:
//!
//! 1. **No thread is left unjoined.** Every worker thread is spawned inside a
//!    [`std::thread::scope`] and joined inside it, so a panic in one worker
//!    unwinds only that worker and the scope still joins the rest. A team that
//!    hangs is worse than a team that reports one failed member.
//! 2. **A panic is a failed worker, not a failed team.** The runner call runs
//!    under [`std::panic::catch_unwind`], the payload becomes the worker's note,
//!    its task is reopened, and its result is counted as failed. The point is not
//!    to hide bugs — it is that the caller asked for N results, and N-1 plus an
//!    unwind is the failure this crate exists to avoid.
//! 3. **The report is ordered, not interleaved.** Workers are sorted by name,
//!    problems are sorted and deduplicated, scopes come sorted from the board.
//!    Two runs of one plan produce structurally identical reports, which is what
//!    lets a test compare the whole structure instead of poking at fields.
//! 4. **Claims happen before any worker runs, in name order.** Racing the claim
//!    itself would make a team's outcome depend on thread scheduling: the same
//!    plan could put the task in a different worker's hands on every run. The
//!    parent claims for the whole roster first (still through the board's CAS, so
//!    another process is still excluded), which makes "who owns what" a property
//!    of the plan. The race the CAS exists for is tested where it belongs, on
//!    [`Board::claim`](crate::Board::claim) itself.
//!
//! Concurrency is bounded by permits, not by thread count. The roster gets one
//! thread each (so a worker's stack and its panic are its own), but a thread waits
//! for a permit before it enters the runner. That is `forge-subagent`'s serial
//! semaphore widened to N — and it is measured, not inferred from timing, by
//! [`RecordingRunner::max_inside`](crate::RecordingRunner::max_inside).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use serde::{Deserialize, Serialize};

use crate::board::Board;
use crate::mailbox::Mailbox;
use crate::worker::{clip, Call, WorkerResult, WorkerRunner, WorkerSpec};
use crate::{lock, now_millis, write_atomic};

/// Directory under the team root holding each worker's transcript.
pub const WORKERS_DIR: &str = "workers";

/// How many workers may be inside the runner at once when a plan says nothing.
///
/// Four is what the tier doc's acceptance case needs with room to spare, and it
/// is low enough that a mistake here costs four processes rather than sixty-four.
/// A plan that wants more says so explicitly.
pub const DEFAULT_MAX_PARALLEL: usize = 4;

/// What to dispatch.
///
/// A plan is data, not a closure: the caller builds it, this crate runs it, and a
/// test can therefore describe a team without owning a scheduler.
#[derive(Debug, Clone)]
pub struct Plan {
    root: PathBuf,
    workers: Vec<WorkerSpec>,
    max_parallel: usize,
    mailbox: Option<Mailbox>,
}

impl Plan {
    /// An empty plan rooted at `root` (the directory holding `board.json`).
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            workers: Vec::new(),
            max_parallel: DEFAULT_MAX_PARALLEL,
            mailbox: None,
        }
    }

    /// Append one worker.
    pub fn worker(mut self, worker: WorkerSpec) -> Self {
        self.workers.push(worker);
        self
    }

    /// Append a named worker with a prompt and no board task.
    pub fn member(self, name: impl Into<String>, prompt: impl Into<String>) -> Self {
        self.worker(WorkerSpec::new(name, prompt))
    }

    /// Set the concurrency bound. Zero is treated as one, so a caller that
    /// computed it from an empty config gets a working team rather than a team
    /// that never starts.
    pub fn max_parallel(mut self, max_parallel: usize) -> Self {
        self.max_parallel = max_parallel;
        self
    }

    /// Give the team a mailbox: each worker's queued mail is drained and handed
    /// to its runner before the runner is called.
    pub fn mailbox(mut self, mailbox: Mailbox) -> Self {
        self.mailbox = Some(mailbox);
        self
    }

    /// The team root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The roster, in the order it was added.
    pub fn workers(&self) -> &[WorkerSpec] {
        &self.workers
    }

    /// The effective concurrency bound (never zero).
    pub fn parallelism(&self) -> usize {
        self.max_parallel.max(1)
    }
}

/// Counts of tasks touched by the dispatch.
///
/// Three numbers, not two: "claimed but not completed" is the interesting one and
/// it cannot be derived from a claimed/completed pair when a claim itself failed.
///
/// * `claimed` — tasks this dispatch took ownership of.
/// * `completed` — claimed tasks it moved to `completed`.
/// * `failed` — claimed tasks left incomplete, plus tasks whose claim was refused
///   (another worker owned them, or the board said no).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskCounts {
    pub claimed: usize,
    pub completed: usize,
    pub failed: usize,
}

/// One worker in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerReport {
    pub name: String,
    pub ok: bool,
    /// The child's exit status when a child ran, `None` when the runner reports
    /// none (a pure in-process runner) or when the worker never got that far.
    pub exit: Option<i32>,
    pub note: String,
}

/// What one dispatch produced.
///
/// `problems` is the union of the board's own graph defects and the dispatch's
/// own failures, as stable one-line strings, sorted and deduplicated. A caller
/// that needs the typed defects for branching reads the board directly; the
/// report is for "what should a human be told".
///
/// `scopes` is the overlap as it stood *after the roster was claimed and before
/// any worker ran*, not the overlap at the end: by then every claimed task is
/// completed and the board has nothing left in progress to compare. A snapshot
/// taken at the moment the hint was actionable is the only one worth reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub tasks: TaskCounts,
    pub workers: Vec<WorkerReport>,
    pub scopes: Vec<crate::ScopeWarning>,
    pub problems: Vec<String>,
}

impl Report {
    /// Did the team do what it was asked?
    ///
    /// An empty roster is not a success: it means nothing was attempted, and a
    /// caller that read `true` there would report work that never happened. A
    /// board defect alone does not make the report not-ok — the workers may have
    /// done everything asked of them on a board that needs fixing — so defects
    /// live in `problems` and the caller decides.
    pub fn ok(&self) -> bool {
        !self.workers.is_empty()
            && self.tasks.failed == 0
            && self.workers.iter().all(|worker| worker.ok)
    }

    /// The worker named `name`, if it ran.
    pub fn worker(&self, name: &str) -> Option<&WorkerReport> {
        self.workers.iter().find(|worker| worker.name == name)
    }
}

/// A task this dispatch owns, and the revision its owner was given.
struct Claimed {
    id: String,
    revision: u32,
    task: crate::Task,
}

/// Run a plan against a board.
///
/// Never returns `Err`, and that is deliberate: a caller that got an error would
/// learn nothing about the four workers that did run, and its next move (retry
/// what failed, report what passed) needs exactly that information. An unusable
/// plan is reported the same way — as problems.
pub fn run(plan: &Plan, board: &Board, runner: &dyn WorkerRunner) -> Report {
    let mut problems: BTreeSet<String> = BTreeSet::new();

    // The board's own defects come first: they explain why a worker may
    // legitimately find nothing ready, and a report of only successes would hide
    // the reason.
    for problem in board.board_problems() {
        problems.insert(format!(
            "board: {} ({})",
            problem.message,
            problem.kind.as_str()
        ));
    }

    // A duplicate name would mean two threads writing one transcript and two
    // workers believing one inbox is theirs. Refuse the second rather than let
    // the file decide.
    let mut seen_names: BTreeSet<String> = BTreeSet::new();
    let mut roster: Vec<WorkerSpec> = Vec::new();
    for worker in plan.workers() {
        if let Err(error) = worker.validate() {
            problems.insert(format!(
                "plan: worker {:?} skipped: {}",
                worker.name,
                error.message()
            ));
            continue;
        }
        if !seen_names.insert(worker.name.clone()) {
            problems.insert(format!(
                "plan: worker {:?} appears twice in the roster; the second entry was skipped",
                worker.name
            ));
            continue;
        }
        roster.push(worker.clone());
    }
    roster.sort_by(|a, b| a.name.cmp(&b.name));

    if roster.is_empty() {
        problems.insert("plan: the roster has no runnable worker".to_string());
        return Report {
            tasks: TaskCounts::default(),
            workers: Vec::new(),
            scopes: board.warned_scopes(),
            problems: problems.into_iter().collect(),
        };
    }

    let workers_dir = plan.root().join(WORKERS_DIR);
    if let Err(error) = std::fs::create_dir_all(&workers_dir) {
        // Not fatal: a team that can work without a transcript should work, and
        // each transcript write reports its own failure.
        problems.insert(format!(
            "plan: cannot create {}: {error}",
            workers_dir.display()
        ));
    }

    // Claim for the whole roster before any worker runs. Deterministic in name
    // order (the roster is sorted above), so the same plan produces the same
    // ownership; still the board's compare-and-set, so a worker in another
    // process cannot be talking itself into the same task at the same time.
    let mut refusals: Vec<WorkerOutcome> = Vec::new();
    let mut runnable: Vec<(WorkerSpec, Option<Claimed>)> = Vec::with_capacity(roster.len());
    for worker in roster {
        let claim = match &worker.task {
            None => None,
            Some(id) => match board.try_claim(id, &worker.name) {
                Ok(task) => Some(Claimed {
                    id: id.clone(),
                    revision: task.revision,
                    task,
                }),
                Err(error) => {
                    refusals.push(refused(
                        &worker,
                        &workers_dir,
                        format!("could not claim task {id:?}: {}", error.message()),
                    ));
                    continue;
                }
            },
        };
        runnable.push((worker, claim));
    }

    // The hint is only visible while the tasks are in progress, which is exactly
    // now: after this loop every claim either completes or is reopened.
    let scopes = board.warned_scopes();

    let permits = Permits::new(plan.parallelism());
    let mut outcomes: Vec<WorkerOutcome> = refusals;
    std::thread::scope(|scope| {
        let mut jobs = Vec::with_capacity(runnable.len());
        for (worker, claim) in &runnable {
            let name = worker.name.clone();
            let guard_name = name.clone();
            let permits = &permits;
            let root = plan.root();
            let workers_dir = workers_dir.as_path();
            let mailbox = plan.mailbox.as_ref();
            let handle = scope.spawn(move || {
                permits.acquire();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch_one(
                        worker,
                        claim.as_ref(),
                        board,
                        runner,
                        root,
                        workers_dir,
                        mailbox,
                    )
                }));
                permits.release();
                match outcome {
                    Ok(outcome) => outcome,
                    // A panic outside the runner guard (in the board code or a
                    // transcript write). Still one worker's failure, still joined.
                    Err(payload) => WorkerOutcome::panicked(&guard_name, payload.as_ref()),
                }
            });
            jobs.push((name, handle));
        }
        for (name, handle) in jobs {
            // A failed `join` means the worker's own guard did not hold (a panic
            // in a destructor, say). Report it as that worker's failure instead
            // of losing every other result by unwinding here.
            match handle.join() {
                Ok(outcome) => outcomes.push(outcome),
                Err(payload) => outcomes.push(WorkerOutcome::panicked(&name, payload.as_ref())),
            }
        }
    });

    outcomes.sort_by(|a, b| a.report.name.cmp(&b.report.name));
    let mut tasks = TaskCounts::default();
    for outcome in &outcomes {
        tasks.claimed += usize::from(outcome.claimed);
        tasks.completed += usize::from(outcome.completed);
        tasks.failed += usize::from(outcome.failed_task);
        for problem in &outcome.problems {
            problems.insert(problem.clone());
        }
    }

    Report {
        tasks,
        workers: outcomes.into_iter().map(|outcome| outcome.report).collect(),
        scopes,
        problems: problems.into_iter().collect(),
    }
}

/// What one worker contributed to the report.
struct WorkerOutcome {
    report: WorkerReport,
    claimed: bool,
    completed: bool,
    failed_task: bool,
    problems: Vec<String>,
}

impl WorkerOutcome {
    /// The verdict for a worker whose thread panicked outside its own guard.
    fn panicked(name: &str, payload: &(dyn std::any::Any + Send)) -> Self {
        Self {
            report: WorkerReport {
                name: name.to_string(),
                ok: false,
                exit: None,
                note: format!("panicked: {}", panic_text(payload)),
            },
            claimed: false,
            completed: false,
            failed_task: false,
            problems: vec![format!("worker {name:?} panicked")],
        }
    }
}

/// The verdict for a worker that never ran because its task could not be claimed.
///
/// The transcript is still written: "this worker was asked to do X and never
/// started" is exactly what someone debugging a stuck board needs to see, and a
/// missing file would look like the worker was never in the roster.
fn refused(worker: &WorkerSpec, workers_dir: &Path, note: String) -> WorkerOutcome {
    let transcript = workers_dir.join(format!("{}.jsonl", worker.name));
    let mut problems = Vec::new();
    write_start(&transcript, worker, &mut problems);
    write_end(&transcript, worker, false, None, &note, "", &mut problems);
    WorkerOutcome {
        report: WorkerReport {
            name: worker.name.clone(),
            ok: false,
            exit: None,
            note,
        },
        claimed: false,
        completed: false,
        failed_task: true,
        problems,
    }
}

/// Run one worker: mail, transcript, runner, board transition.
///
/// The order matters. The claim happened before this function was called so two
/// workers can never both be told "you own this task"; the transcript start line
/// comes before the runner so a worker that hangs still leaves evidence of what
/// it was asked; the board transition comes after the runner returned, and a
/// failed worker's task is reopened rather than left `in_progress` — an owned task
/// nobody is working on blocks the board forever.
fn dispatch_one(
    worker: &WorkerSpec,
    claim: Option<&Claimed>,
    board: &Board,
    runner: &dyn WorkerRunner,
    root: &Path,
    workers_dir: &Path,
    mailbox: Option<&Mailbox>,
) -> WorkerOutcome {
    let name = worker.name.clone();
    let transcript = workers_dir.join(format!("{name}.jsonl"));
    let mut problems: Vec<String> = Vec::new();

    // Mail is drained before the runner so a message that arrived while the
    // worker did not exist is delivered on its first run.
    let mut mail = Vec::new();
    if let Some(mailbox) = mailbox {
        match mailbox.drain(&name) {
            Ok(messages) => mail = messages,
            Err(error) => problems.push(format!(
                "worker {name:?} could not read its mailbox: {}",
                error.message()
            )),
        }
    }

    let call = Call {
        spec: worker,
        task: claim.map(|claim| claim.task.clone()),
        root,
        transcript: &transcript,
        mail,
    };
    write_start(&transcript, worker, &mut problems);

    // A guard around the runner call: an in-process runner can panic, and the
    // end line plus the reopen below must still happen. `AssertUnwindSafe` is
    // honest here — the runner is shared immutably and the panic path only
    // records a note.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runner.run(&call)));
    let result = match outcome {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => WorkerResult::failed(format!("runner failed: {}", error.message())),
        Err(payload) => {
            let note = format!("panicked: {}", panic_text(payload.as_ref()));
            problems.push(format!("worker {name:?} {note}"));
            WorkerResult::failed(note)
        }
    };
    write_end(
        &transcript,
        worker,
        result.ok,
        result.exit,
        &result.note,
        &result.text,
        &mut problems,
    );

    let mut completed = false;
    let mut failed_task = false;
    if let Some(claim) = claim {
        let id = claim.id.as_str();
        if result.ok {
            match board.complete(id, claim.revision) {
                Ok(_) => completed = true,
                Err(error) => {
                    // The runner said it succeeded but the board would not accept
                    // the completion (another writer moved the task). Report it
                    // rather than silently claiming a completed task.
                    failed_task = true;
                    problems.push(format!(
                        "worker {name:?} finished task {id:?} but the board refused the completion: {}",
                        error.message()
                    ));
                }
            }
        } else {
            failed_task = true;
            if let Err(error) = board.reopen(id, claim.revision) {
                problems.push(format!(
                    "worker {name:?} failed and task {id:?} could not be reopened: {}",
                    error.message()
                ));
            }
        }
    }

    let mut note = clip(&result.note, 400);
    if !result.ok && note.trim().is_empty() {
        note = "worker did not succeed and gave no note".to_string();
    }
    WorkerOutcome {
        report: WorkerReport {
            name,
            ok: result.ok && !failed_task,
            exit: result.exit,
            note,
        },
        claimed: claim.is_some(),
        completed,
        failed_task,
        problems,
    }
}

/// A counting semaphore over a mutex and a condition variable.
///
/// `std::sync::Semaphore` is still unstable and a dependency is not worth one
/// permit. Acquire and release are separate calls rather than a guard because the
/// guard would be dropped during an unwind, and the count must stay correct in
/// exactly that path.
struct Permits {
    available: Mutex<usize>,
    wake: Condvar,
}

impl Permits {
    fn new(limit: usize) -> Self {
        Self {
            available: Mutex::new(limit.max(1)),
            wake: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let mut available = lock(&self.available);
        while *available == 0 {
            available = self
                .wake
                .wait(available)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        *available -= 1;
    }

    fn release(&self) {
        let mut available = lock(&self.available);
        *available += 1;
        self.wake.notify_one();
    }
}

/// The transcript's first line: what this worker was told.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartLine<'a> {
    kind: &'static str,
    ts: i64,
    worker: &'a str,
    task: Option<&'a str>,
    prompt: String,
}

/// The transcript's last line: what it answered.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EndLine<'a> {
    kind: &'static str,
    ts: i64,
    worker: &'a str,
    ok: bool,
    exit: Option<i32>,
    note: String,
    text: String,
}

fn write_start(path: &Path, worker: &WorkerSpec, problems: &mut Vec<String>) {
    let line = StartLine {
        kind: "worker/start",
        ts: now_millis(),
        worker: &worker.name,
        task: worker.task.as_deref(),
        prompt: clip(&worker.prompt, 4000),
    };
    let text = match serde_json::to_string(&line) {
        Ok(text) => text,
        Err(error) => {
            problems.push(format!(
                "transcript {} could not be encoded: {error}",
                path.display()
            ));
            return;
        }
    };
    // Replace, don't append: a transcript holding two runs with no separator is
    // worse than none, because the reader cannot tell which prompt produced which
    // answer. Replacing is safe because exactly one writer owns the file per run,
    // so law 1 ("one writer per log") still holds for it.
    if let Err(error) = write_atomic(path, format!("{text}\n").as_bytes()) {
        problems.push(format!(
            "transcript {} could not be written: {}",
            path.display(),
            error.message()
        ));
    }
}

fn write_end(
    path: &Path,
    worker: &WorkerSpec,
    ok: bool,
    exit: Option<i32>,
    note: &str,
    text: &str,
    problems: &mut Vec<String>,
) {
    use std::io::Write;

    let line = EndLine {
        kind: "worker/end",
        ts: now_millis(),
        worker: &worker.name,
        ok,
        exit,
        note: clip(note, 400),
        text: clip(text, 8000),
    };
    let json = match serde_json::to_string(&line) {
        Ok(json) => json,
        Err(error) => {
            problems.push(format!(
                "transcript {} could not be encoded: {error}",
                path.display()
            ));
            return;
        }
    };
    let appended = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()
    })();
    if let Err(error) = appended {
        problems.push(format!(
            "transcript {} could not be appended: {error}",
            path.display()
        ));
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "non-string panic payload".to_string()
}
