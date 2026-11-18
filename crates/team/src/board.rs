//! The shared task board: one JSON file, every transition a compare-and-set.
//!
//! Why a file and not a queue in memory: the board is the only fact two workers
//! can disagree about. If it lived in the dispatcher's address space, a restart
//! would forget which tasks were claimed, and a worker that had already started
//! would look idle — the exact double-dispatch this crate exists to prevent.
//! So the board is `<root>/board.json`, it is rewritten through a temp file and
//! a rename (never truncated in place), and the in-memory copy is only a cache
//! of what the file says.
//!
//! Why a revision on every task, and a revision on the board:
//!
//! * Per-task revision is the compare-and-set token. A caller reads a task,
//!   decides what to do, then says "apply this assuming revision 3". If someone
//!   else moved the task in between, the write is refused with
//!   [`ErrorKind::Conflict`] and the caller re-reads. That refusal is the whole
//!   point: two workers claiming one task is the failure this crate exists to
//!   prevent, and a plain lock would only serialise the mistake.
//! * Board revision counts every applied transition. It is a cheap way for an
//!   observer (a test, a UI poller) to answer "did anything happen" without
//!   diffing task lists.
//!
//! What the board deliberately does not do: it does not validate the dependency
//! graph. A dependency on an id that does not exist, a task that depends on
//! itself, two tasks waiting on each other — all of those are *accepted* and
//! then reported by [`Board::board_problems`]. Rejecting them at the door would
//! mean an operator cannot build a board incrementally (add B before A exists)
//! and could not see the deadlock it created; the report is what makes the
//! deadlock loud instead of silent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use forge_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};

use crate::{lock, valid_name, write_atomic};

/// The board's file name inside the team root.
pub const BOARD_FILE: &str = "board.json";

/// The only board format this build writes or reads.
///
/// A board written by a newer build carries a newer version; reading it anyway
/// would silently drop the fields this build does not know, and a board with a
/// dropped field is a board that lies about the graph. Refuse, name the file and
/// the versions — the same rule the session log follows.
pub const BOARD_VERSION: u32 = 1;

/// Where a task is in its life. The wire words are frozen in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    InProgress,
    Completed,
}

impl Status {
    /// The stable string form, which is also what the file contains.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::InProgress => "in_progress",
            Status::Completed => "completed",
        }
    }
}

/// One task on the board.
///
/// `revision` starts at 1 and advances on every applied transition, so a task
/// that has never been touched is revision 1 — never 0. Zero would make "no
/// expected revision" indistinguishable from "the first revision", and a caller
/// that forgot the argument would silently pass the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub subject: String,
    #[serde(default)]
    pub description: String,
    pub status: Status,
    #[serde(default)]
    pub owner: Option<String>,
    pub revision: u32,
    #[serde(default, rename = "blockedBy")]
    pub blocked_by: Vec<String>,
    #[serde(default, rename = "writeScopes")]
    pub write_scopes: Vec<String>,
}

/// A task handed to [`Board::add_task`], before the board assigns identity.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    /// Empty means "mint one"; a caller that needs a stable id (a test, a
    /// plan written by a human) supplies its own.
    pub id: String,
    pub subject: String,
    pub description: String,
    pub blocked_by: Vec<String>,
    pub write_scopes: Vec<String>,
}

impl NewTask {
    /// A task with a subject and a minted id.
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            ..Self::default()
        }
    }

    /// Use a specific id instead of a minted one.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Attach a longer description for the worker.
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Declare dependencies. Unknown ids are accepted and later reported.
    pub fn blocked_by<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.blocked_by = ids.into_iter().map(Into::into).collect();
        self
    }

    /// Declare advisory write scopes.
    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.write_scopes.push(scope.into());
        self
    }
}

/// The fields [`Board::edit`] may change. `None` means "leave as it is", which
/// is why the fields are options and not defaults: an edit that cleared an
/// omitted description would be a data loss bug wearing a convenience hat.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub write_scopes: Option<Vec<String>>,
}

impl TaskPatch {
    /// An empty patch, to be filled in by the caller.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the subject.
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Replace the description.
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Replace the whole scope list (not append: an edit that appended could
    /// not express "this scope was wrong").
    pub fn scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.write_scopes = Some(scopes.into_iter().map(Into::into).collect());
        self
    }
}

/// What is wrong with the graph, as opposed to with one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// A `blockedBy` names a task that is not on the board. The dependent task
    /// can never become ready, because nothing will ever complete the phantom.
    DanglingDependency,
    /// A task blocks on itself.
    SelfDependency,
    /// Tasks wait on each other in a ring. Every member waits forever.
    DependencyCycle,
}

impl ProblemKind {
    /// Stable wire word, also used to order the report deterministically.
    pub fn as_str(self) -> &'static str {
        match self {
            ProblemKind::DanglingDependency => "dangling_dependency",
            ProblemKind::SelfDependency => "self_dependency",
            ProblemKind::DependencyCycle => "dependency_cycle",
        }
    }
}

/// One graph defect, named so a caller can branch on it and a human can act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    pub kind: ProblemKind,
    /// The tasks involved, sorted, so equal problems compare equal.
    pub tasks: Vec<String>,
    pub message: String,
}

impl Ord for Problem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.kind, &self.tasks, &self.message).cmp(&(other.kind, &other.tasks, &other.message))
    }
}

impl PartialOrd for Problem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Two in-progress tasks whose write scopes overlap.
///
/// Advisory only. See [`Board::warned_scopes`] for why it must not block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeWarning {
    /// The two task ids, lexicographically ordered.
    pub tasks: Vec<String>,
    /// The two overlapping scope strings, in the same order as `tasks`.
    pub scopes: Vec<String>,
}

impl Ord for ScopeWarning {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.tasks, &self.scopes).cmp(&(&other.tasks, &other.scopes))
    }
}

impl PartialOrd for ScopeWarning {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The file's own shape. Separate from [`Task`] so the envelope can carry the
/// version and the board revision without polluting the per-task record.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BoardFile {
    version: u32,
    revision: u64,
    tasks: Vec<Task>,
}

/// In-memory state; the file is the truth and this is a cache of it.
#[derive(Debug, Clone, Default)]
struct State {
    revision: u64,
    tasks: BTreeMap<String, Task>,
}

impl State {
    fn to_file(&self) -> BoardFile {
        BoardFile {
            version: BOARD_VERSION,
            revision: self.revision,
            tasks: self.tasks.values().cloned().collect(),
        }
    }
}

/// A task board on disk.
///
/// Every method takes `&self`: the mutex inside is what serialises writers, and
/// a caller that needed `&mut` would have to own the board exclusively, which is
/// exactly what concurrent workers cannot do.
///
/// `Debug` prints the in-memory cache, which may lag a concurrent writer's file
/// by nothing (the cache is swapped only after the file is written) — useful in
/// a failing assertion, not a substitute for reading the file.
#[derive(Debug)]
pub struct Board {
    root: PathBuf,
    state: Mutex<State>,
}

impl Board {
    /// Open (or start) the board at `<root>/board.json`.
    ///
    /// A missing file is an empty board, not an error: the first `add_task`
    /// creates it. An unreadable or wrong-version file *is* an error — a board
    /// this build cannot understand must not be silently replaced by an empty
    /// one, because that would erase every claim on it.
    pub fn open(root: impl AsRef<Path>) -> Result<Board> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("team: cannot create team root {}: {error}", root.display()),
            )
        })?;
        let path = root.join(BOARD_FILE);
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => parse(&text, &path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(error) => {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("team: cannot read board {}: {error}", path.display()),
                ))
            }
        };
        Ok(Board {
            root,
            state: Mutex::new(state),
        })
    }

    /// The team root this board lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The board file's absolute path.
    pub fn path(&self) -> PathBuf {
        self.root.join(BOARD_FILE)
    }

    /// Number of applied transitions plus the number of tasks added.
    pub fn revision(&self) -> u64 {
        lock(&self.state).revision
    }

    /// Every task, ordered by id. Ordering is part of the contract: a report
    /// that shuffled its own tasks would make "did anything change" a diff.
    pub fn tasks(&self) -> Vec<Task> {
        lock(&self.state).tasks.values().cloned().collect()
    }

    /// One task, if it exists.
    pub fn task(&self, id: &str) -> Option<Task> {
        lock(&self.state).tasks.get(id).cloned()
    }

    /// Add a task at revision 1 and advance the board revision.
    pub fn add_task(&self, new_task: NewTask) -> Result<Task> {
        let id = if new_task.id.trim().is_empty() {
            mint_id()?
        } else {
            new_task.id.trim().to_string()
        };
        if !valid_name(&id) {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!(
                    "team: task id {id:?} is not a usable id (1..={} chars of [A-Za-z0-9._-], not a Windows device name)",
                    crate::MAX_NAME_LEN
                ),
            ));
        }
        if new_task.subject.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: task {id:?} needs a non-empty subject"),
            ));
        }
        let mut guard = lock(&self.state);
        if guard.tasks.contains_key(&id) {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("team: task {id:?} already exists on the board"),
            ));
        }
        let task = Task {
            id: id.clone(),
            subject: new_task.subject.trim().to_string(),
            description: new_task.description,
            status: Status::Pending,
            owner: None,
            revision: 1,
            blocked_by: new_task.blocked_by,
            write_scopes: new_task.write_scopes,
        };
        let mut next = guard.clone();
        next.tasks.insert(id, task.clone());
        next.revision += 1;
        self.commit(&mut guard, next)?;
        Ok(task)
    }

    /// Claim a pending task for `owner`, assuming the caller saw `expected`.
    ///
    /// A task already owned by someone else is a conflict even when the caller's
    /// revision was current: two owners on one task is precisely the state this
    /// method refuses to create.
    pub fn claim(&self, id: &str, expected_revision: u32, owner: &str) -> Result<Task> {
        if owner.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                format!("team: cannot claim task {id:?} for an empty owner"),
            ));
        }
        self.transition(id, expected_revision, |task| {
            match task.status {
                Status::Pending => {}
                Status::InProgress => {
                    return Err(Error::new(
                        ErrorKind::Conflict,
                        format!(
                            "team: task {id:?} is already in_progress for {:?}",
                            task.owner.as_deref().unwrap_or("")
                        ),
                    ))
                }
                Status::Completed => {
                    return Err(Error::new(
                        ErrorKind::Conflict,
                        format!("team: task {id:?} is already completed"),
                    ))
                }
            }
            task.status = Status::InProgress;
            task.owner = Some(owner.trim().to_string());
            Ok(())
        })
    }

    /// Claim whatever the board says right now, without a caller-supplied
    /// revision.
    ///
    /// This is the dispatcher's convenience, and it is not a hole in the CAS:
    /// the read and the write happen under the same mutex, so it compares
    /// against the revision that is current *at the moment of the write*. The
    /// strict [`Board::claim`] exists for callers that made a decision earlier
    /// (a model reading a board snapshot) and must not act on a stale one.
    pub fn try_claim(&self, id: &str, owner: &str) -> Result<Task> {
        let revision = self
            .task(id)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("team: no task {id:?} on the board"),
                )
            })?
            .revision;
        self.claim(id, revision, owner)
    }

    /// Mark a task completed. The owner is kept: "who did it" is the fact a
    /// report is usually asked for afterwards.
    pub fn complete(&self, id: &str, expected_revision: u32) -> Result<Task> {
        self.transition(id, expected_revision, |task| {
            if task.status == Status::Completed {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!("team: task {id:?} is already completed"),
                ));
            }
            task.status = Status::Completed;
            Ok(())
        })
    }

    /// Put a task back to pending and drop its owner.
    pub fn reopen(&self, id: &str, expected_revision: u32) -> Result<Task> {
        self.transition(id, expected_revision, |task| {
            if task.status == Status::Pending && task.owner.is_none() {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!("team: task {id:?} is already pending and unowned"),
                ));
            }
            task.status = Status::Pending;
            task.owner = None;
            Ok(())
        })
    }

    /// Change the prose (and scopes) of a task without touching its state.
    pub fn edit(&self, id: &str, expected_revision: u32, patch: TaskPatch) -> Result<Task> {
        if let Some(subject) = &patch.subject {
            if subject.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("team: task {id:?} needs a non-empty subject"),
                ));
            }
        }
        self.transition(id, expected_revision, |task| {
            if let Some(subject) = patch.subject {
                task.subject = subject.trim().to_string();
            }
            if let Some(description) = patch.description {
                task.description = description;
            }
            if let Some(scopes) = patch.write_scopes {
                task.write_scopes = scopes;
            }
            Ok(())
        })
    }

    /// Replace the whole dependency list. Unknown ids are accepted; they show up
    /// in [`Board::board_problems`] instead of being refused here.
    pub fn set_dependencies(
        &self,
        id: &str,
        expected_revision: u32,
        blocked_by: Vec<String>,
    ) -> Result<Task> {
        let mut deduped: Vec<String> = Vec::new();
        for dependency in blocked_by {
            if !deduped.contains(&dependency) {
                deduped.push(dependency);
            }
        }
        self.transition(id, expected_revision, |task| {
            if task.status == Status::Completed && !deduped.is_empty() {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!("team: task {id:?} is completed; completing it again through a dependency change would be a lie"),
                ));
            }
            task.blocked_by = deduped;
            Ok(())
        })
    }

    /// Hand a task to another owner (or to nobody, with `None`).
    pub fn reassign(
        &self,
        id: &str,
        expected_revision: u32,
        owner: Option<String>,
    ) -> Result<Task> {
        if let Some(owner) = &owner {
            if owner.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::Invalid,
                    format!("team: cannot reassign task {id:?} to an empty owner"),
                ));
            }
        }
        self.transition(id, expected_revision, |task| {
            task.owner = owner.as_ref().map(|owner| owner.trim().to_string());
            Ok(())
        })
    }

    /// Tasks a worker may start now: pending, with every dependency completed.
    ///
    /// A dependency on an id that does not exist is never `completed`, so such a
    /// task is permanently unready — which is correct, and why
    /// [`Board::board_problems`] must name it: readiness alone cannot tell a
    /// caller whether "not ready" means "wait" or "this will never happen".
    pub fn ready(&self) -> Vec<Task> {
        let state = lock(&self.state);
        state
            .tasks
            .values()
            .filter(|task| {
                task.status == Status::Pending
                    && task.blocked_by.iter().all(|dependency| {
                        state
                            .tasks
                            .get(dependency)
                            .map(|other| other.status == Status::Completed)
                            .unwrap_or(false)
                    })
            })
            .cloned()
            .collect()
    }

    /// Tasks someone is working on right now, ordered by id.
    pub fn in_progress(&self) -> Vec<Task> {
        let state = lock(&self.state);
        state
            .tasks
            .values()
            .filter(|task| task.status == Status::InProgress)
            .cloned()
            .collect()
    }

    /// Pairs of in-progress tasks whose `writeScopes` overlap.
    ///
    /// **Advisory, never a lock.** Two workers editing neighbouring files is
    /// often fine and sometimes deliberate (one writes the test, one writes the
    /// code). Refusing a claim because a hint fired would teach every caller to
    /// ignore the hint, and then the one time it matters nobody reads it. So
    /// this is reported and nothing more; the caller decides whether to
    /// serialise, and the report says what it decided about.
    ///
    /// One warning per task pair, even when several scopes overlap: a pair is
    /// the unit a human acts on ("these two are in the same files"), not each
    /// overlapping string.
    pub fn warned_scopes(&self) -> Vec<ScopeWarning> {
        let active = self.in_progress();
        let mut warnings = Vec::new();
        for (index, first) in active.iter().enumerate() {
            for second in active.iter().skip(index + 1) {
                let mut hit: Option<(String, String)> = None;
                'outer: for a in &first.write_scopes {
                    for b in &second.write_scopes {
                        if scopes_overlap(a, b) {
                            hit = Some((a.clone(), b.clone()));
                            break 'outer;
                        }
                    }
                }
                if let Some((a, b)) = hit {
                    let (mut ids, mut scopes) =
                        (vec![first.id.clone(), second.id.clone()], vec![a, b]);
                    if ids[1] < ids[0] {
                        ids.swap(0, 1);
                        scopes.swap(0, 1);
                    }
                    warnings.push(ScopeWarning { tasks: ids, scopes });
                }
            }
        }
        warnings.sort();
        warnings
    }

    /// Everything wrong with the dependency graph, named.
    ///
    /// Three defects, all of which leave tasks permanently unready with no
    /// error anywhere: a dependency on a phantom, a self-dependency, a ring.
    /// A board that reported none of them would look perfectly healthy while a
    /// worker waited forever, which is the worst kind of failure to debug.
    pub fn board_problems(&self) -> Vec<Problem> {
        let state = lock(&self.state);
        let mut problems = Vec::new();
        for task in state.tasks.values() {
            for dependency in &task.blocked_by {
                if *dependency == task.id {
                    problems.push(Problem {
                        kind: ProblemKind::SelfDependency,
                        tasks: vec![task.id.clone()],
                        message: format!(
                            "task {:?} blocks on itself, so it can never become ready",
                            task.id
                        ),
                    });
                } else if !state.tasks.contains_key(dependency) {
                    problems.push(Problem {
                        kind: ProblemKind::DanglingDependency,
                        tasks: vec![task.id.clone(), dependency.clone()],
                        message: format!(
                            "task {:?} blocks on {:?}, which is not on the board",
                            task.id, dependency
                        ),
                    });
                }
            }
        }
        problems.extend(cycles(&state));
        problems.sort();
        problems
    }

    /// Apply one transition under the mutex: read, check the revision, mutate a
    /// copy, persist, then swap it in.
    ///
    /// Persist-before-swap is what keeps memory and disk from diverging: if the
    /// write fails, the caller gets the error and the board still describes the
    /// file.
    fn transition<F>(&self, id: &str, expected_revision: u32, apply: F) -> Result<Task>
    where
        F: FnOnce(&mut Task) -> Result<()>,
    {
        let mut guard = lock(&self.state);
        let mut next = guard.clone();
        let task = next.tasks.get_mut(id).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("team: no task {id:?} on the board"),
            )
        })?;
        if task.revision != expected_revision {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "team: conflict on task {id:?}: it is at revision {}, the caller expected {expected_revision}",
                    task.revision
                ),
            ));
        }
        apply(task)?;
        task.revision = task.revision.saturating_add(1);
        next.revision = next.revision.saturating_add(1);
        let result = task.clone();
        self.commit(&mut guard, next)?;
        Ok(result)
    }

    fn commit(&self, cache: &mut State, next: State) -> Result<()> {
        let text = serde_json::to_string_pretty(&next.to_file()).map_err(|error| {
            Error::new(
                ErrorKind::Json,
                format!("team: cannot encode the board: {error}"),
            )
        })?;
        write_atomic(&self.path(), format!("{text}\n").as_bytes())?;
        *cache = next;
        Ok(())
    }
}

/// Parse a board file, refusing a version this build does not know.
fn parse(text: &str, path: &Path) -> Result<State> {
    let file: BoardFile = serde_json::from_str(text).map_err(|error| {
        Error::new(
            ErrorKind::Json,
            format!("team: board {} is not valid JSON: {error}", path.display()),
        )
    })?;
    if file.version != BOARD_VERSION {
        return Err(Error::new(
            ErrorKind::UnsupportedVersion,
            format!(
                "team: board {} is v{}, this build reads v{BOARD_VERSION}",
                path.display(),
                file.version
            ),
        ));
    }
    let mut tasks = BTreeMap::new();
    for task in file.tasks {
        if tasks.insert(task.id.clone(), task).is_some() {
            return Err(Error::new(
                ErrorKind::DuplicateId,
                format!("team: board {} lists one task id twice", path.display()),
            ));
        }
    }
    Ok(State {
        revision: file.revision,
        tasks,
    })
}

/// Every dependency ring, one problem per ring.
///
/// Iterative depth-first search over the dependency edges; a node's colour
/// (0 unvisited, 1 on the current stack, 2 finished) is what turns a back edge
/// into a named cycle. The stack is explicit so a long chain cannot overflow
/// the native stack — a board is user input, and user input is not a depth
/// guarantee.
fn cycles(state: &State) -> Vec<Problem> {
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Grey,
        Black,
    }

    let mut colour: BTreeMap<&str, Color> = state
        .tasks
        .keys()
        .map(|id| (id.as_str(), Color::White))
        .collect();
    let mut found: BTreeSet<Vec<String>> = BTreeSet::new();

    for start in state.tasks.keys() {
        if colour.get(start.as_str()) != Some(&Color::White) {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        let mut path: Vec<&str> = vec![start];
        if let Some(grey) = colour.get_mut(start.as_str()) {
            *grey = Color::Grey;
        }
        while let Some((node, index)) = stack.pop() {
            let dependencies = state
                .tasks
                .get(node)
                .map(|task| task.blocked_by.as_slice())
                .unwrap_or(&[]);
            if index < dependencies.len() {
                let dependency = dependencies[index].as_str();
                stack.push((node, index + 1));
                if dependency == node {
                    // A self-edge is a one-node ring, and reporting it as both
                    // `SelfDependency` and `DependencyCycle` would tell a human
                    // "two problems" about one task. `board_problems` already
                    // named the self-dependency; skip it here.
                    continue;
                }
                let Some(known) = colour.get(dependency).copied() else {
                    // A dangling dependency is reported on its own; it is not a
                    // ring, and treating it as one would double-report it.
                    continue;
                };
                match known {
                    Color::White => {
                        if let Some(grey) = colour.get_mut(dependency) {
                            *grey = Color::Grey;
                        }
                        stack.push((dependency, 0));
                        path.push(dependency);
                    }
                    Color::Grey => {
                        // The ring is the tail of `path` from `dependency`.
                        let at = path
                            .iter()
                            .position(|item| *item == dependency)
                            .unwrap_or(0);
                        let mut ring: Vec<String> =
                            path[at..].iter().map(|id| id.to_string()).collect();
                        ring.sort();
                        ring.dedup();
                        found.insert(ring);
                    }
                    Color::Black => {}
                }
            } else {
                if let Some(black) = colour.get_mut(node) {
                    *black = Color::Black;
                }
                path.pop();
            }
        }
    }

    found
        .into_iter()
        .map(|ring| Problem {
            kind: ProblemKind::DependencyCycle,
            message: format!(
                "tasks {} wait on each other in a ring, so none of them can become ready",
                ring.iter()
                    .map(|id| format!("{id:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            tasks: ring,
        })
        .collect()
}

/// Does one write scope cover the other?
///
/// Component-wise prefix, not string prefix: `src/a` must not be considered to
/// overlap `src/ab`, which is the bug a naive `starts_with` would introduce.
/// Case is folded only on Windows, because on Windows the filesystem folds it
/// and on Linux it does not — a hint that warned on `Src/Core` vs `src/core`
/// everywhere would be wrong on the platform where those are two directories.
fn scopes_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (normalise_scope(a), normalise_scope(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
    long.len() > short.len()
        && long.starts_with(short.as_str())
        && long.as_bytes()[short.len()] == b'/'
}

/// Separators unified, trailing separator dropped, case folded on Windows.
fn normalise_scope(scope: &str) -> String {
    let mut out: String = scope
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string();
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    if out.ends_with('/') || out.is_empty() || out == "." {
        return String::new();
    }
    if cfg!(windows) {
        out = out.to_lowercase();
    }
    out
}

/// Mint a task id: `t` plus 12 hex characters of a fresh random id.
fn mint_id() -> Result<String> {
    let id = forge_core::new_id()?;
    let suffix: String = id
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .take(12)
        .collect();
    if suffix.len() < 12 {
        return Err(Error::new(
            ErrorKind::Io,
            "team: the id source returned fewer than 12 hex characters",
        ));
    }
    Ok(format!("t{suffix}"))
}
