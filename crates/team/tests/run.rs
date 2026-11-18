//! The dispatcher's acceptance tests: what a team promises when several workers
//! run at once.
//!
//! Every assertion here is about an observable: the board file on disk, the
//! transcripts, the report structure, or a counter the test double incremented
//! while it was running. None of them assert on elapsed time, because a timing
//! assertion is a flaky test wearing a concurrency costume.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forge_core::ErrorKind;
use forge_team::run::run;
use forge_team::{
    Board, Call, Mailbox, NewTask, Plan, RecordingRunner, TaskCounts, WorkerResult, WorkerRunner,
    WorkerSpec, DEFAULT_MAX_PARALLEL, WORKERS_DIR,
};

fn temp_dir(label: &str) -> std::path::PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    // The nanosecond stamp matters: a process id is recycled by the OS, so a
    // name built from it alone can collide with a directory left behind by an
    // earlier run and hand a test another run's board.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "forge-team-run-{}-{}-{}-{}",
        std::process::id(),
        stamp,
        NEXT.fetch_add(1, Ordering::SeqCst),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A rendezvous the test double passes through, so "four workers were inside at
/// the same time" is measured rather than hoped for.
///
/// It waits at most two seconds, so a bound that is too small makes the test
/// fail with a readable assertion instead of hanging the suite.
struct Rendezvous {
    wanted: usize,
    arrived: Mutex<usize>,
    wake: std::sync::Condvar,
}

impl Rendezvous {
    fn new(wanted: usize) -> Self {
        Self {
            wanted,
            arrived: Mutex::new(0),
            wake: std::sync::Condvar::new(),
        }
    }

    fn arrive(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut arrived = self
            .arrived
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *arrived += 1;
        self.wake.notify_all();
        while *arrived < self.wanted && std::time::Instant::now() < deadline {
            let (guard, _) = self
                .wake
                .wait_timeout(arrived, Duration::from_millis(10))
                .unwrap_or_else(|poison| poison.into_inner());
            arrived = guard;
        }
    }
}

fn board(label: &str) -> (std::path::PathBuf, Board) {
    let dir = temp_dir(label);
    let board = Board::open(&dir).expect("open board");
    (dir, board)
}

fn transcript(dir: &std::path::Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(WORKERS_DIR).join(format!("{name}.jsonl")))
        .expect("read transcript")
}

#[test]
fn two_workers_with_disjoint_scopes_both_complete_and_the_board_moves_by_transitions() {
    let (dir, board) = board("two-disjoint");
    board
        .add_task(
            NewTask::new("board")
                .id("t-board")
                .scope("crates/team/src/board.rs"),
        )
        .expect("add task");
    board
        .add_task(
            NewTask::new("mailbox")
                .id("t-mail")
                .scope("crates/team/src/mailbox.rs"),
        )
        .expect("add task");
    let before = board.revision();

    let plan = Plan::new(&dir)
        .worker(WorkerSpec::new("alpha", "write the board").task("t-board"))
        .worker(WorkerSpec::new("beta", "write the mailbox").task("t-mail"));
    let runner = RecordingRunner::succeeding();
    let report = run(&plan, &board, &runner);

    assert!(report.ok(), "report was not ok: {report:?}");
    assert_eq!(
        report.tasks,
        TaskCounts {
            claimed: 2,
            completed: 2,
            failed: 0
        }
    );
    for id in ["t-board", "t-mail"] {
        assert_eq!(
            board.task(id).expect("task on board").status,
            forge_team::Status::Completed,
            "task {id}"
        );
    }
    // Two claims and two completions: four transitions, no more.
    assert_eq!(
        board.revision() - before,
        4,
        "the board must move exactly once per transition"
    );

    // Deterministic ordering: the report is by name, not by spawn order (the
    // roster above is already sorted, so run one that is not).
    let names: Vec<&str> = report
        .workers
        .iter()
        .map(|worker| worker.name.as_str())
        .collect();
    assert_eq!(names, ["alpha", "beta"]);

    // Disjoint scopes mean no warning at all.
    assert!(
        report.scopes.is_empty(),
        "disjoint scopes warned: {:?}",
        report.scopes
    );
    assert!(
        report.problems.is_empty(),
        "unexpected problems: {:?}",
        report.problems
    );

    // Each worker left its own transcript under <root>/workers/, start then end.
    for (name, task) in [("alpha", "t-board"), ("beta", "t-mail")] {
        let lines = transcript(&dir, name);
        let parsed: Vec<serde_json::Value> = lines
            .lines()
            .map(|line| serde_json::from_str(line).expect("transcript line is JSON"))
            .collect();
        assert_eq!(parsed.len(), 2, "{name} transcript: {lines}");
        assert_eq!(parsed[0]["kind"], "worker/start");
        assert_eq!(parsed[0]["task"], task);
        assert_eq!(parsed[1]["kind"], "worker/end");
        assert_eq!(parsed[1]["ok"], true);
    }
}

#[test]
fn the_report_is_listed_in_name_order_whatever_the_roster_order_is() {
    let (dir, board) = board("name-order");
    let plan = Plan::new(&dir)
        .member("zulu", "last alphabetically")
        .member("alpha", "first alphabetically")
        .member("mike", "in the middle");
    let report = run(&plan, &board, &RecordingRunner::succeeding());
    let names: Vec<&str> = report
        .workers
        .iter()
        .map(|worker| worker.name.as_str())
        .collect();
    assert_eq!(names, ["alpha", "mike", "zulu"]);
}

#[test]
fn a_plan_with_no_workers_reports_that_instead_of_succeeding_quietly() {
    let (dir, board) = board("empty-plan");
    let report = run(&Plan::new(&dir), &board, &RecordingRunner::succeeding());
    assert!(!report.ok());
    assert!(report.workers.is_empty());
    assert_eq!(
        report.problems,
        vec!["plan: the roster has no runnable worker".to_string()]
    );
}

#[test]
fn a_bad_name_is_reported_and_never_becomes_a_file() {
    let (dir, board) = board("bad-name");
    let plan = Plan::new(&dir)
        .member("alpha", "fine")
        .member("../escape", "not fine");
    let report = run(&plan, &board, &RecordingRunner::succeeding());
    assert_eq!(report.workers.len(), 1);
    assert_eq!(report.workers[0].name, "alpha");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.contains("plan: worker \"../escape\" skipped")),
        "problems: {:?}",
        report.problems
    );
    assert!(
        !dir.join("workers").join("..\\escape.jsonl").exists(),
        "a refused name must not create a path"
    );
}

#[test]
fn a_duplicate_name_runs_once_and_is_reported() {
    let (dir, board) = board("duplicate");
    let plan = Plan::new(&dir)
        .member("alpha", "first")
        .member("alpha", "second");
    let runner = RecordingRunner::succeeding();
    let report = run(&plan, &board, &runner);
    assert_eq!(runner.calls(), 1, "the second entry must not run");
    assert_eq!(report.workers.len(), 1);
    assert!(report
        .problems
        .iter()
        .any(|problem| problem.contains("appears twice in the roster")));
}

#[test]
fn two_workers_bound_to_one_task_produce_exactly_one_worker() {
    let (dir, board) = board("same-task");
    board
        .add_task(NewTask::new("one job").id("t1"))
        .expect("add task");
    let plan = Plan::new(&dir)
        .worker(WorkerSpec::new("alpha", "take it").task("t1"))
        .worker(WorkerSpec::new("beta", "take it too").task("t1"));
    let runner = RecordingRunner::succeeding();
    let report = run(&plan, &board, &runner);

    let winners = report.workers.iter().filter(|worker| worker.ok).count();
    assert_eq!(winners, 1, "exactly one worker may complete the task");
    assert_eq!(report.tasks.claimed, 1);
    assert_eq!(report.tasks.completed, 1);
    assert_eq!(report.tasks.failed, 1);
    let loser = report
        .workers
        .iter()
        .find(|worker| !worker.ok)
        .expect("a loser");
    assert!(
        loser.note.contains("could not claim task \"t1\""),
        "loser note: {}",
        loser.note
    );
    assert_eq!(runner.calls(), 1, "the loser must not run at all");
    assert_eq!(
        board.task("t1").expect("task").status,
        forge_team::Status::Completed
    );
}

#[test]
fn a_worker_whose_task_is_missing_fails_without_running() {
    let (dir, board) = board("missing-task");
    let plan = Plan::new(&dir).worker(WorkerSpec::new("alpha", "ghost").task("nope"));
    let runner = RecordingRunner::succeeding();
    let report = run(&plan, &board, &runner);
    assert_eq!(runner.calls(), 0);
    assert_eq!(report.tasks.failed, 1);
    assert_eq!(report.tasks.claimed, 0);
    assert!(!report.workers[0].ok);
    assert!(
        report.workers[0]
            .note
            .contains("could not claim task \"nope\""),
        "note: {}",
        report.workers[0].note
    );
}

#[test]
fn a_failed_worker_reopens_its_task_so_the_board_is_not_left_occupied() {
    let (dir, board) = board("failed-reopens");
    board
        .add_task(NewTask::new("doomed").id("t1"))
        .expect("add task");
    let plan = Plan::new(&dir).worker(WorkerSpec::new("alpha", "fail").task("t1"));
    let report = run(&plan, &board, &RecordingRunner::failing("the build broke"));

    let task = board.task("t1").expect("task");
    assert_eq!(task.status, forge_team::Status::Pending);
    assert_eq!(task.owner, None);
    assert!(task.revision > 1);
    assert!(!report.ok());
    assert_eq!(report.tasks.completed, 0);
    assert_eq!(report.tasks.failed, 1);
    assert_eq!(report.workers[0].note, "the build broke");
}

#[test]
fn a_panicking_runner_becomes_one_failed_worker_and_the_others_still_report() {
    let (dir, board) = board("panic");
    board
        .add_task(NewTask::new("doomed").id("t1"))
        .expect("add task");
    let runner = RecordingRunner::from_fn(|call: &Call<'_>| -> forge_core::Result<WorkerResult> {
        if call.name() == "beta" {
            panic!("the runner exploded");
        }
        Ok(WorkerResult::ok("finished"))
    });
    let plan = Plan::new(&dir)
        .worker(WorkerSpec::new("beta", "explode").task("t1"))
        .member("alpha", "keep working")
        .member("gamma", "keep working");
    let report = run(&plan, &board, &runner);

    assert_eq!(report.workers.len(), 3, "a panic must not lose the roster");
    let beta = report.worker("beta").expect("beta reported");
    assert!(!beta.ok);
    assert_eq!(beta.note, "panicked: the runner exploded");
    assert!(report.worker("alpha").expect("alpha reported").ok);
    assert!(report.worker("gamma").expect("gamma reported").ok);
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem == "worker \"beta\" panicked: the runner exploded"),
        "problems: {:?}",
        report.problems
    );
    // The task is not stranded in_progress by the unwind.
    assert_eq!(
        board.task("t1").expect("task").status,
        forge_team::Status::Pending
    );
    // The team is still usable afterwards: the board is not poisoned.
    let again = run(
        &Plan::new(&dir).member("delta", "after the panic"),
        &board,
        &runner,
    );
    assert!(again.ok(), "re-run after a panic: {again:?}");
}

#[test]
fn a_runner_that_returns_an_error_fails_only_its_worker() {
    let (dir, board) = board("runner-error");
    let runner = RecordingRunner::from_fn(|call: &Call<'_>| {
        if call.name() == "alpha" {
            return Err(forge_core::Error::new(
                ErrorKind::Refused,
                "no provider configured",
            ));
        }
        Ok(WorkerResult::ok("fine"))
    });
    let plan = Plan::new(&dir).member("alpha", "one").member("beta", "two");
    let report = run(&plan, &board, &runner);
    assert_eq!(report.workers.len(), 2);
    assert!(!report.worker("alpha").expect("alpha").ok);
    assert_eq!(
        report.worker("alpha").expect("alpha").note,
        "runner failed: no provider configured"
    );
    assert!(report.worker("beta").expect("beta").ok);
}

#[test]
fn concurrency_never_exceeds_the_bound_and_does_reach_it() {
    let (dir, board) = board("bounded");
    // Two workers must meet inside the runner; the other three pass straight
    // through. Meeting is a rendezvous, not a sleep: if the bound were violated
    // the extra workers would arrive and the count below would say so, and if
    // the bound were never reached the meeting would time out instead of the
    // test passing by luck.
    let meeting = Arc::new(Rendezvous::new(2));
    let arrivals = Arc::new(AtomicUsize::new(0));
    let (meeting_in, arrivals_in) = (Arc::clone(&meeting), Arc::clone(&arrivals));
    let runner = RecordingRunner::from_fn(move |_call: &Call<'_>| {
        if arrivals_in.fetch_add(1, Ordering::SeqCst) < 2 {
            meeting_in.arrive();
        }
        Ok(WorkerResult::ok("done"))
    });
    let mut plan = Plan::new(&dir);
    for index in 0..5 {
        plan = plan.member(format!("w{index}"), "work");
    }
    let report = run(&plan.max_parallel(2), &board, &runner);

    assert_eq!(report.workers.len(), 5);
    assert!(report.ok());
    // Measured, not inferred: the double counted its own entries.
    assert_eq!(
        runner.max_inside(),
        2,
        "at most two workers may be inside the runner, and two must overlap"
    );
    assert_eq!(runner.calls(), 5, "every worker ran exactly once");
    assert_eq!(runner.current_inside(), 0, "every worker left the runner");
}

#[test]
fn the_default_bound_is_four_and_zero_is_treated_as_one() {
    let (dir, board) = board("default-bound");
    assert_eq!(Plan::new(&dir).parallelism(), DEFAULT_MAX_PARALLEL);
    assert_eq!(Plan::new(&dir).max_parallel(0).parallelism(), 1);
    assert_eq!(Plan::new(&dir).max_parallel(9).parallelism(), 9);

    // Four workers, no `max_parallel` call: all four must be inside at once, and
    // the rendezvous only releases when the fourth arrives.
    let meeting = Arc::new(Rendezvous::new(4));
    let runner = {
        let meeting = Arc::clone(&meeting);
        RecordingRunner::from_fn(move |_call: &Call<'_>| {
            meeting.arrive();
            Ok(WorkerResult::ok("done"))
        })
    };
    let mut plan = Plan::new(&dir);
    for index in 0..4 {
        plan = plan.member(format!("w{index}"), "work");
    }
    run(&plan, &board, &runner);
    assert_eq!(runner.max_inside(), 4, "the default bound is four");
}

/// (worker name, the texts of the mail it was handed), appended from inside the
/// runner, so the assertion is about what the dispatcher delivered.
type MailSink = Arc<Mutex<Vec<(String, Vec<String>)>>>;

#[test]
fn mail_sent_before_the_worker_starts_is_handed_to_the_runner() {
    let (dir, board) = board("mail");
    let mailbox = Mailbox::open(&dir).expect("open mailbox");
    mailbox
        .send("lead", "alpha", "the schema moved")
        .expect("send");
    mailbox.send("lead", "alpha", "second note").expect("send");
    mailbox.send("lead", "beta", "not for alpha").expect("send");

    let received: MailSink = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let runner = RecordingRunner::from_fn(move |call: &Call<'_>| {
        let texts = call.mail.iter().map(|mail| mail.text.clone()).collect();
        sink.lock()
            .expect("lock sink")
            .push((call.name().to_string(), texts));
        Ok(WorkerResult::ok("done"))
    });
    let plan = Plan::new(&dir)
        .member("alpha", "read your mail")
        .member("beta", "read yours")
        .mailbox(mailbox.clone());
    let report = run(&plan, &board, &runner);
    assert!(report.ok());

    let mut received = received.lock().expect("lock received").clone();
    received.sort();
    assert_eq!(
        received,
        vec![
            (
                "alpha".to_string(),
                vec!["the schema moved".to_string(), "second note".to_string()]
            ),
            ("beta".to_string(), vec!["not for alpha".to_string()]),
        ],
        "each worker sees its own mail, in send order"
    );
    // Delivery consumed the queue: a second dispatch sees nothing new.
    assert!(mailbox.drain("alpha").expect("drain").is_empty());
}

#[test]
fn a_mailbox_that_cannot_be_read_is_reported_and_the_worker_still_runs() {
    let (dir, board) = board("mail-broken");
    let mailbox = Mailbox::open(&dir).expect("open mailbox");
    // A directory where the inbox file should be: readable as a path, not as a
    // file. The worker must still run, with a problem recorded.
    std::fs::create_dir_all(mailbox.path("alpha")).expect("create directory in place of inbox");
    let plan = Plan::new(&dir)
        .member("alpha", "work anyway")
        .mailbox(mailbox);
    let report = run(&plan, &board, &RecordingRunner::succeeding());
    assert!(report.ok(), "the worker ran: {report:?}");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.contains("could not read its mailbox")),
        "problems: {:?}",
        report.problems
    );
}

#[test]
fn overlapping_write_scopes_are_reported_without_stopping_the_run() {
    let (dir, board) = board("scopes");
    board
        .add_task(NewTask::new("one").id("t1").scope("crates/team/src"))
        .expect("add task");
    board
        .add_task(
            NewTask::new("two")
                .id("t2")
                .scope("crates/team/src/board.rs"),
        )
        .expect("add task");
    let runner = RecordingRunner::succeeding().with_delay(Duration::from_millis(30));
    let plan = Plan::new(&dir)
        .worker(WorkerSpec::new("alpha", "one").task("t1"))
        .worker(WorkerSpec::new("beta", "two").task("t2"))
        .max_parallel(2);
    let report = run(&plan, &board, &runner);

    assert!(report.ok(), "the hint must not block: {report:?}");
    assert_eq!(report.scopes.len(), 1, "scopes: {:?}", report.scopes);
    assert_eq!(report.scopes[0].tasks, ["t1", "t2"]);
    assert_eq!(
        report.scopes[0].scopes,
        ["crates/team/src", "crates/team/src/board.rs"]
    );
}

#[test]
fn board_defects_appear_in_the_report_before_anything_runs() {
    let (dir, board) = board("dangling");
    board
        .add_task(NewTask::new("blocked").id("t1").blocked_by(["ghost"]))
        .expect("add task");
    let plan = Plan::new(&dir).member("alpha", "work");
    let report = run(&plan, &board, &RecordingRunner::succeeding());
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.contains("dangling_dependency") && problem.contains("\"ghost\"")),
        "problems: {:?}",
        report.problems
    );
}

#[test]
fn the_report_serializes_to_the_documented_wire_shape() {
    let (dir, board) = board("wire");
    board
        .add_task(NewTask::new("one").id("t1").scope("src"))
        .expect("add task");
    let plan = Plan::new(&dir).worker(WorkerSpec::new("alpha", "work").task("t1"));
    let report = run(&plan, &board, &RecordingRunner::succeeding());
    let value = serde_json::to_value(&report).expect("serialize report");

    assert_eq!(
        value["tasks"],
        serde_json::json!({"claimed": 1, "completed": 1, "failed": 0})
    );
    assert_eq!(value["workers"][0]["name"], "alpha");
    assert_eq!(value["workers"][0]["ok"], true);
    assert_eq!(value["workers"][0]["exit"], 0);
    assert!(value["workers"][0]["note"].is_string());
    assert_eq!(value["scopes"], serde_json::json!([]));
    assert_eq!(value["problems"], serde_json::json!([]));

    // The report round-trips: a caller can hand the JSON to a model and read it
    // back without a second schema.
    let parsed: forge_team::Report = serde_json::from_value(value).expect("deserialize report");
    assert_eq!(parsed, report);
}

/// `run` is a free function, so a caller holding `Box<dyn WorkerRunner>` must be
/// able to dispatch too — this is the shape the CLI's `team` tool uses.
#[test]
fn a_boxed_runner_can_dispatch() {
    let (dir, board) = board("dyn");
    let runner: Box<dyn WorkerRunner> = Box::new(RecordingRunner::succeeding());
    let report = run(
        &Plan::new(&dir).member("alpha", "work"),
        &board,
        runner.as_ref(),
    );
    assert!(report.ok());
}

#[cfg(windows)]
#[test]
fn the_command_runner_shells_out_through_forge_exec() {
    use forge_team::CommandRunner;

    let comspec =
        std::env::var("COMSPEC").unwrap_or_else(|_| "C:\\Windows\\System32\\cmd.exe".to_string());
    let (dir, board) = board("command");

    // Success: the prompt arrives on stdin (unread by `echo`), the child exits 0,
    // and its stdout lands in the transcript.
    let ok_runner = CommandRunner::new(&comspec).args(["/c", "echo", "hello team"]);
    let report = run(
        &Plan::new(&dir).member("alpha", "ignored prompt"),
        &board,
        &ok_runner,
    );
    assert!(report.ok(), "report: {report:?}");
    assert_eq!(report.workers[0].exit, Some(0));
    let lines = transcript(&dir, "alpha");
    assert!(
        lines.contains("hello team"),
        "stdout must reach the transcript: {lines}"
    );

    // A non-zero exit is data: a failed worker, not a failed team.
    let bad_runner = CommandRunner::new(&comspec).args(["/c", "exit", "3"]);
    let report = run(
        &Plan::new(&dir).member("beta", "ignored prompt"),
        &board,
        &bad_runner,
    );
    assert!(!report.workers[0].ok);
    assert_eq!(report.workers[0].exit, Some(3));
    assert_eq!(report.workers[0].note, "exit 3");
}

#[cfg(windows)]
#[test]
fn the_command_runner_refuses_an_empty_program() {
    use forge_team::CommandRunner;

    let (dir, board) = board("command-empty");
    let report = run(
        &Plan::new(&dir).member("alpha", "prompt"),
        &board,
        &CommandRunner::new("  "),
    );
    assert!(!report.workers[0].ok);
    assert!(
        report.workers[0].note.contains("needs a non-empty program"),
        "note: {}",
        report.workers[0].note
    );
}
