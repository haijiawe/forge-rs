//! The on-disk registry: a single JSON file listing every task.
//!
//! A registry is a directory, not a session: schedules outlive the conversation
//! that created them, which is the whole point of persisting them apart from a
//! session log. One file keeps the format inspectable with `cat`, and the
//! replace-by-rename save keeps a reader from ever seeing half a list.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use forge_core::{Error, ErrorKind, Result};

use crate::task::{Due, Task};

const FILE_NAME: &str = "tasks.json";

/// The tasks under one root directory.
#[derive(Debug, Clone)]
pub struct Registry {
    root: PathBuf,
}

impl Registry {
    pub fn open(root: impl Into<PathBuf>) -> Registry {
        Registry { root: root.into() }
    }

    /// The file a registry reads and writes. Reported by the CLI so an operator
    /// can see where their schedules live without knowing the convention.
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every task, in file order. A missing file is an empty registry rather
    /// than an error, because "no schedules yet" is the normal first state.
    pub fn load(&self) -> Result<Vec<Task>> {
        let path = self.path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&path, error)),
        };
        // An empty file is not an empty list: it is a file something truncated
        // or a write that never finished. Reading it as "no tasks" would let
        // the next save erase every schedule silently.
        if text.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::Json,
                format!("schedule: {} is empty, not a task list", path.display()),
            ));
        }
        serde_json::from_str(&text).map_err(|error| {
            Error::new(
                ErrorKind::Json,
                format!("schedule: {} is not a task list: {error}", path.display()),
            )
            .with_source(error)
        })
    }

    /// Replaces the file with `tasks`. The bytes are written to a neighbour of
    /// the target and renamed over it, so a process that dies mid-write leaves
    /// the previous list intact rather than a truncated one.
    pub fn save(&self, tasks: &[Task]) -> Result<()> {
        let path = self.path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
        }
        let mut text = serde_json::to_string_pretty(tasks).map_err(|error| {
            Error::new(
                ErrorKind::Json,
                format!("schedule: cannot encode the task list: {error}"),
            )
            .with_source(error)
        })?;
        text.push('\n');

        let temp = self.temp_path();
        let written = write_all(&temp, text.as_bytes());
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(io_error(&temp, error));
        }
        if let Err(error) = fs::rename(&temp, &path) {
            let _ = fs::remove_file(&temp);
            return Err(io_error(&path, error));
        }
        Ok(())
    }

    /// Adds a task. Two tasks with the same id in one file would make
    /// `mark_run` ambiguous, so the duplicate is refused instead of stacked.
    pub fn add(&self, task: Task) -> Result<()> {
        if task.id.is_empty() {
            return Err(Error::new(
                ErrorKind::Invalid,
                "schedule: the task has no id; a new id could not be generated",
            ));
        }
        let mut tasks = self.load()?;
        if tasks.iter().any(|other| other.id == task.id) {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("schedule: task {} already exists", task.id),
            ));
        }
        tasks.push(task);
        self.save(&tasks)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let mut tasks = self.load()?;
        let before = tasks.len();
        tasks.retain(|task| task.id != id);
        if tasks.len() == before {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("schedule: no task {id}"),
            ));
        }
        self.save(&tasks)
    }

    pub fn get(&self, id: &str) -> Result<Option<Task>> {
        Ok(self.load()?.into_iter().find(|task| task.id == id))
    }

    /// Every task whose next occurrence is at or before `now_ms`, oldest first.
    ///
    /// The next occurrence is searched from the last delivery, so an occurrence
    /// is owed until `mark_run` records it and never twice. A task that slept
    /// through several occurrences owes the oldest one first: delivering the
    /// newest and dropping the rest would silently lose work the operator asked
    /// for, one occurrence at a time.
    pub fn due(&self, now_ms: i64) -> Result<Vec<Due>> {
        let mut due = Vec::new();
        for task in self.load()? {
            // A rule this version cannot read is left alone: it may have been
            // written by a newer version, and the task is still the operator's.
            let Some(at_ms) = task.next_after_ms() else {
                continue;
            };
            if at_ms <= now_ms {
                due.push(Due { task, at_ms });
            }
        }
        due.sort_by(|left, right| {
            left.at_ms
                .cmp(&right.at_ms)
                .then_with(|| left.task.id.cmp(&right.task.id))
        });
        Ok(due)
    }

    /// Records that the occurrence `at_ms` of `id` has been delivered.
    pub fn mark_run(&self, id: &str, at_ms: i64) -> Result<()> {
        let mut tasks = self.load()?;
        let Some(task) = tasks.iter_mut().find(|task| task.id == id) else {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("schedule: no task {id}"),
            ));
        };
        task.last_run_ms = Some(at_ms);
        task.runs = task.runs.saturating_add(1);
        self.save(&tasks)
    }

    /// A temp name beside the target. The process id and a counter make it
    /// unique without reading a clock, which the library is not allowed to do.
    fn temp_path(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        self.root
            .join(format!("{FILE_NAME}.{}-{serial}.tmp", std::process::id()))
    }
}

fn write_all(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    // The rename is only durable once the bytes are on disk; without the sync a
    // crash can leave the new name pointing at an unflushed file.
    file.sync_all()
}

fn io_error(path: &Path, error: std::io::Error) -> Error {
    Error::new(
        ErrorKind::Io,
        format!("schedule: cannot use {}: {error}", path.display()),
    )
    .with_source(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Task;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join("forge-schedule-unit")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn task(rule: &str, start_ms: i64) -> Task {
        Task::new(rule, "say hello", start_ms, start_ms)
    }

    #[test]
    fn a_missing_file_loads_as_an_empty_list_and_is_not_created() {
        let root = scratch("missing");
        let registry = Registry::open(&root);
        assert_eq!(registry.load().expect("loads"), Vec::new());
        assert!(!registry.path().exists());
    }

    #[test]
    fn save_then_load_round_trips() {
        let root = scratch("roundtrip");
        let registry = Registry::open(&root);
        let mut first = task("FREQ=DAILY", 1_794_873_600_000);
        first.target = Some("s_abc".into());
        let second = task("FREQ=WEEKLY;BYDAY=MO", 1_794_873_600_000);
        registry
            .save(&[first.clone(), second.clone()])
            .expect("save");
        assert_eq!(registry.load().expect("loads"), vec![first, second]);
    }

    #[test]
    fn an_empty_file_is_a_json_error_not_an_empty_list() {
        let root = scratch("emptyfile");
        let registry = Registry::open(&root);
        fs::create_dir_all(&root).expect("dir");
        fs::write(registry.path(), b"  \n").expect("write");
        let error = registry.load().expect_err("refused");
        assert!(error.is(ErrorKind::Json), "{error:?}");
    }

    #[test]
    fn a_duplicate_id_is_a_conflict_and_a_missing_one_is_not_found() {
        let root = scratch("ids");
        let registry = Registry::open(&root);
        let task = task("FREQ=DAILY", 1_794_873_600_000);
        registry.add(task.clone()).expect("first add");
        let error = registry.add(task).expect_err("refused");
        assert!(error.is(ErrorKind::Conflict), "{error:?}");
        let error = registry.remove("nope").expect_err("refused");
        assert!(error.is(ErrorKind::NotFound), "{error:?}");
        let error = registry.mark_run("nope", 0).expect_err("refused");
        assert!(error.is(ErrorKind::NotFound), "{error:?}");
    }

    #[test]
    fn a_task_without_an_id_is_refused() {
        let root = scratch("noid");
        let registry = Registry::open(&root);
        let mut task = task("FREQ=DAILY", 1_794_873_600_000);
        task.id.clear();
        let error = registry.add(task).expect_err("refused");
        assert!(error.is(ErrorKind::Invalid), "{error:?}");
    }

    #[test]
    fn an_unreadable_rule_is_kept_but_never_due() {
        let root = scratch("badrule");
        let registry = Registry::open(&root);
        let task = task("FREQ=DAILY;BYMONTH=3", 1_794_873_600_000);
        registry.add(task.clone()).expect("add still succeeds");
        assert_eq!(registry.due(i64::MAX).expect("due works"), Vec::new());
        assert_eq!(registry.get(&task.id).expect("get").expect("present"), task);
    }

    #[test]
    fn due_is_oldest_first() {
        let root = scratch("order");
        let registry = Registry::open(&root);
        registry
            .add(task("FREQ=DAILY;BYHOUR=18", 1_794_873_600_000))
            .expect("add");
        registry
            .add(task("FREQ=DAILY;BYHOUR=9", 1_794_873_600_000))
            .expect("add");
        let due = registry.due(1_794_992_400_000).expect("due works");
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].at_ms, 1_794_906_000_000, "09:00 comes first");
        assert_eq!(due[1].at_ms, 1_794_938_400_000, "then 18:00");
    }

    #[test]
    fn a_save_leaves_no_temp_file_behind() {
        let root = scratch("atomic");
        let registry = Registry::open(&root);
        registry
            .add(task("FREQ=DAILY", 1_794_873_600_000))
            .expect("add");
        let names: Vec<String> = fs::read_dir(&root)
            .expect("dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![FILE_NAME.to_string()], "{names:?}");
    }
}
