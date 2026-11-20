//! Whether this machine can do document work, and if not, exactly what is missing.
//!
//! `docs/09-platform-tier.md` makes Office a **skill plus a program probe**: the
//! skill drives `python-docx`, `openpyxl` and `python-pptx` as an external
//! program, and this module is the probe that decides whether that skill can run
//! at all. The crate accordingly ships no `.docx`/`.xlsx`/`.pptx` reader and no
//! writer. A half-written document that opens is worse than a refusal, and the
//! refusal is only honest if it names the absent piece.
//!
//! The rule this module exists to keep: **a probe that cannot answer says
//! `unknown`, never `missing`, and never blocks.** Every child gets a deadline.
//! `forge doctor` is what an operator runs when the machine is already
//! misbehaving, so a probe that waits forever would turn one diagnosis into a
//! second outage. `stdin` is closed and `stderr` discarded, and only a handful of
//! bytes are read from `stdout`, so a dead child cannot fill a pipe and stall the
//! poll loop.
//!
//! The interpreter is *run*, never guessed at: a Windows Store `python.exe` stub
//! exists on many machines and fails at run time, and a `docx` directory next to
//! the script is not the library (the importable module is `docx`, but for
//! `python-pptx` it is `pptx`). Only an import that actually happened counts.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The deadline for one program. Generous enough for a cold interpreter start on
/// a loaded box, short enough that a hung shim is noticed while the operator is
/// still looking at the command they just typed.
const PER_PROBE: Duration = Duration::from_secs(3);

/// The deadline for the whole probe. Doctor runs every check on every
/// invocation, so the sum of the parts is bounded too, not just each part.
const TOTAL_BUDGET: Duration = Duration::from_secs(6);

/// How often the wait loop looks at the child: below human perception, and free
/// next to an interpreter start.
const POLL: Duration = Duration::from_millis(10);

/// The importable modules the skill needs. The module name is not always the
/// capability name: `python-pptx` imports as `pptx`, `python-docx` as `docx`.
const LIBRARIES: [(&str, &str); 3] = [("docx", "docx"), ("xlsx", "openpyxl"), ("pptx", "pptx")];

/// Prints one `python <version>` line and one `module <name>` line per library
/// that imported. Deliberately syntax that runs on Python 2.7 and 3.x alike: a
/// probe must not misreport "missing" because the only interpreter is old.
const PROBE_SCRIPT: &str = concat!(
    "import importlib, sys\n",
    "print('forge-office python %d.%d' % sys.version_info[:2])\n",
    "for name in ('docx', 'openpyxl', 'pptx'):\n",
    "    try:\n",
    "        importlib.import_module(name)\n",
    "        print('forge-office module ' + name)\n",
    "    except Exception:\n",
    "        pass\n",
);

/// What one probe found. `Unknown` is a first-class answer: it is the honest
/// report of a timeout or a launcher that could not start, and it must never be
/// collapsed into `Missing`, which would claim a fact nobody established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// Available; the string names the interpreter or program that provides it.
    Ready(String),
    /// Probed and absent.
    Missing,
    /// Not decided, with the reason (a timeout, a launcher that would not start).
    Unknown(String),
}

impl Capability {
    pub fn is_ready(&self) -> bool {
        matches!(self, Capability::Ready(_))
    }

    fn label(&self) -> String {
        match self {
            Capability::Ready(detail) => detail.clone(),
            Capability::Missing => "missing".to_string(),
            Capability::Unknown(why) => format!("unknown ({why})"),
        }
    }
}

/// What this machine can do, in the four pieces the office skill needs.
#[derive(Debug, Clone)]
pub struct Availability {
    /// The interpreter that answered, or why none did.
    pub python: Capability,
    /// `.docx` via `python-docx`.
    pub docx: Capability,
    /// `.xlsx` via `openpyxl`.
    pub xlsx: Capability,
    /// `.pptx` via `python-pptx`.
    pub pptx: Capability,
    /// Format conversion via LibreOffice (`soffice`).
    pub convert: Capability,
}

impl Availability {
    /// One line for the doctor table: every capability, ready or not.
    pub fn summary(&self) -> String {
        format!(
            "docx={} xlsx={} pptx={} convert={}",
            self.docx.label(),
            self.xlsx.label(),
            self.pptx.label(),
            self.convert.label()
        )
    }

    /// The capabilities that were probed and found absent.
    pub fn absent(&self) -> Vec<&'static str> {
        self.named()
            .into_iter()
            .filter(|(_, capability)| matches!(capability, Capability::Missing))
            .map(|(name, _)| name)
            .collect()
    }

    /// The capabilities no probe could decide, with the reason.
    pub fn undecided(&self) -> Vec<(&'static str, String)> {
        self.named()
            .into_iter()
            .filter_map(|(name, capability)| match capability {
                Capability::Unknown(why) => Some((name, why.clone())),
                _ => None,
            })
            .collect()
    }

    /// The four capabilities under the names the report uses.
    pub fn named(&self) -> Vec<(&'static str, &Capability)> {
        vec![
            ("docx", &self.docx),
            ("xlsx", &self.xlsx),
            ("pptx", &self.pptx),
            ("convert", &self.convert),
        ]
    }

    /// The sentence to show the user when something the skill needs is not there.
    ///
    /// `None` means every capability answered ready. Anything else names the
    /// missing piece *and* the undecided one, because a caller that reads
    /// "missing" for something that was never probed would be misled in the one
    /// direction that matters: it would believe the machine was checked.
    /// The remedy names only what would actually change the answer: a machine
    /// with all three libraries and no LibreOffice is told to install LibreOffice,
    /// not to reinstall packages it already imports. Advice that does not match
    /// the state is how a report that was right still wastes the reader's hour.
    pub fn refusal(&self) -> Option<String> {
        let mut needed: Vec<String> = Vec::new();
        let mut libraries: Vec<&'static str> = Vec::new();
        for name in self.absent() {
            match name {
                "docx" => {
                    needed.push("docx (python-docx)".to_string());
                    libraries.push("python-docx");
                }
                "xlsx" => {
                    needed.push("xlsx (openpyxl)".to_string());
                    libraries.push("openpyxl");
                }
                "pptx" => {
                    needed.push("pptx (python-pptx)".to_string());
                    libraries.push("python-pptx");
                }
                "convert" => needed.push("format conversion (LibreOffice soffice)".to_string()),
                other => needed.push(other.to_string()),
            }
        }
        for (name, why) in self.undecided() {
            needed.push(format!("{name} (could not be probed: {why})"));
        }
        if needed.is_empty() {
            return None;
        }
        let mut remedy: Vec<String> = Vec::new();
        if !libraries.is_empty() {
            remedy.push(format!(
                "install the missing librar{} with `python -m pip install {}`",
                if libraries.len() == 1 { "y" } else { "ies" },
                libraries.join(" ")
            ));
        }
        if matches!(self.convert, Capability::Missing) {
            remedy.push("install LibreOffice for format conversion".to_string());
        }
        let remedy = if remedy.is_empty() {
            // Only undecided probes: nothing can be installed for a question
            // that was never answered.
            "retry the probe, or run the command yourself, to find out".to_string()
        } else {
            remedy.join(", or ")
        };
        Some(format!(
            "unavailable on this machine: {}. {}; say so instead of writing an empty file.",
            needed.join(", "),
            sentence_case(&remedy)
        ))
    }
}

/// Capitalises the first character of a sentence assembled from fragments.
///
/// The remedy is built by pushing fragments onto a list, so the sentence it
/// starts is decided by which fragment came first rather than written in one
/// literal. Capitalising at the seam keeps every fragment readable instead of
/// hard-coding a capital letter in one of them and a lower-case twin in another.
fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Probe every capability once. Never panics, and returns within `TOTAL_BUDGET`
/// plus the time it takes to reap one killed child.
pub fn probe() -> Availability {
    let deadline = Instant::now() + TOTAL_BUDGET;
    let (answer, undecided) = probe_interpreter(deadline);

    let (python, docx, xlsx, pptx) = match answer {
        Some(answer) => {
            // The skill runs the interpreter that answered, so "missing" here
            // means "this interpreter cannot import it" — the operative fact,
            // not a claim about a second interpreter nobody will use.
            let library = |module: &str| {
                if answer.modules.iter().any(|found| found == module) {
                    Capability::Ready(answer.label.clone())
                } else {
                    Capability::Missing
                }
            };
            let python = Capability::Ready(answer.label.clone());
            (
                python,
                library("docx"),
                library("openpyxl"),
                library("pptx"),
            )
        }
        None => {
            let absent = match undecided {
                Some(why) => Capability::Unknown(why),
                None => Capability::Missing,
            };
            (absent.clone(), absent.clone(), absent.clone(), absent)
        }
    };

    Availability {
        python,
        docx,
        xlsx,
        pptx,
        convert: probe_convert(deadline),
    }
}

/// One interpreter that answered the script.
struct Answer {
    /// What to call it, e.g. `python 3.12` or `python 3.12 via py -3`.
    label: String,
    /// The library modules it imported.
    modules: Vec<String>,
}

/// What running one child produced. Three outcomes, because "it did not answer"
/// and "it is not installed" are different facts and only one of them is
/// evidence that something is missing.
#[derive(Debug)]
enum Run {
    /// The child exited; this is its collected stdout.
    Exited(String),
    /// The child was still running at the deadline and was killed.
    Timeout(Duration),
    /// The child never started.
    Failed(std::io::Error),
}

impl Run {
    /// Whether this outcome leaves the question unanswered.
    ///
    /// A program that is not installed is a *negative* answer, not an undecided
    /// one; anything else (permission denied, a corrupt image, a timeout) means
    /// something is there that could not be asked, and that is `unknown`.
    fn undecided(&self) -> Option<String> {
        match self {
            Run::Exited(_) => None,
            Run::Timeout(limit) => Some(format!("timed out after {}s", limit.as_secs())),
            Run::Failed(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Run::Failed(err) => Some(err.to_string()),
        }
    }
}

/// The launchers to try, in the order an operator would expect them.
///
/// `py -3` is the Windows launcher; elsewhere the spawn fails in microseconds,
/// so listing it unconditionally would only cost a failed spawn.
fn interpreter_candidates() -> Vec<Vec<&'static str>> {
    let mut candidates = vec![vec!["python"], vec!["python3"]];
    #[cfg(windows)]
    candidates.push(vec!["py", "-3"]);
    candidates
}

/// Try the launchers until one answers with every library, keeping the best
/// answer seen.
///
/// One answering interpreter is the toolchain, so the loop stops as soon as one
/// imports all three: three interpreter starts per `forge doctor` would be a real
/// cost on a cold box, and a second answer would only disagree if the operator
/// had deliberately built two unrelated Pythons. A launcher that answers with
/// *nothing* importable is clearly not the toolchain, so the loop keeps looking.
fn probe_interpreter(deadline: Instant) -> (Option<Answer>, Option<String>) {
    let mut best: Option<Answer> = None;
    let mut undecided: Option<String> = None;
    for candidate in interpreter_candidates() {
        if best
            .as_ref()
            .is_some_and(|answer| answer.modules.len() == LIBRARIES.len())
        {
            break;
        }
        let Some(limit) = budget(deadline) else {
            undecided.get_or_insert_with(|| "no probe time left".to_string());
            break;
        };
        let launcher = candidate.join(" ");
        let mut command = Command::new(candidate[0]);
        command.args(&candidate[1..]).arg("-c").arg(PROBE_SCRIPT);
        match run_capped(command, limit) {
            Run::Exited(text) => {
                // No marker means it ran but is not the interpreter we asked
                // for: a Windows Store stub, a shim, or something else wearing
                // the name. Keep looking rather than believe it.
                let Some(answer) = parse_answer(&launcher, &text) else {
                    continue;
                };
                let better = best
                    .as_ref()
                    .is_none_or(|seen| answer.modules.len() > seen.modules.len());
                if better {
                    best = Some(answer);
                }
            }
            other => {
                if let Some(why) = other.undecided() {
                    undecided.get_or_insert_with(|| format!("{launcher}: {why}"));
                }
            }
        }
    }
    (best, undecided)
}

/// Turn the script's stdout into an answer, or `None` when it never ran.
fn parse_answer(launcher: &str, text: &str) -> Option<Answer> {
    let mut version: Option<String> = None;
    let mut modules: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("forge-office ") else {
            continue;
        };
        if let Some(found) = rest.strip_prefix("python ") {
            version = Some(found.trim().to_string());
        } else if let Some(module) = rest.strip_prefix("module ") {
            modules.push(module.trim().to_string());
        }
    }
    let version = version?;
    // The launcher matters: on a box where `python` is a Store stub, `py -3`
    // answering means the skill must run `py -3`, so the label has to say so.
    // The label always names the language, because it is pasted into a table
    // cell of the form `docx=python 3.12` and a bare `docx=3.12` reads like a
    // version of the library.
    let label = if launcher == "python" {
        format!("python {version}")
    } else {
        format!("python {version} via {launcher}")
    };
    Some(Answer { label, modules })
}

/// LibreOffice as the conversion fallback, probed by running it.
fn probe_convert(deadline: Instant) -> Capability {
    let Some(limit) = budget(deadline) else {
        return Capability::Unknown("no probe time left".to_string());
    };
    let mut undecided: Option<String> = None;
    for candidate in soffice_candidates() {
        let display = candidate.display().to_string();
        let mut command = Command::new(&candidate);
        // `--version` and nothing else: a bare `soffice` opens the GUI and waits
        // for a user who is not there.
        command.arg("--version");
        match run_capped(command, limit) {
            Run::Exited(text) => match text.lines().map(str::trim).find(|line| !line.is_empty()) {
                Some(first) => return Capability::Ready(first.to_string()),
                None => {
                    undecided.get_or_insert_with(|| format!("{display}: printed no version"));
                }
            },
            other => {
                if let Some(why) = other.undecided() {
                    undecided.get_or_insert_with(|| format!("{display}: {why}"));
                }
            }
        }
    }
    match undecided {
        Some(why) => Capability::Unknown(why),
        None => Capability::Missing,
    }
}

/// `soffice` on `PATH`, then the conventional install locations on platforms
/// whose installers do not add it there.
///
/// This is the one place a path is guessed, and it is only a *candidate*: it is
/// run before it is believed, so a guessed path that is wrong costs a failed
/// spawn and nothing else.
fn soffice_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("soffice")];
    #[cfg(windows)]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(variable) {
                candidates.push(
                    PathBuf::from(base)
                        .join("LibreOffice")
                        .join("program")
                        .join("soffice.exe"),
                );
            }
        }
    }
    #[cfg(target_os = "macos")]
    candidates.push(PathBuf::from(
        "/Applications/LibreOffice.app/Contents/MacOS/soffice",
    ));
    candidates
}

/// The timeout left for one probe, or `None` when the budget is spent.
fn budget(deadline: Instant) -> Option<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left < Duration::from_millis(50) {
        None
    } else {
        Some(left.min(PER_PROBE))
    }
}

/// Run a child with a deadline, killing it when the deadline passes.
///
/// `Command::output()` waits forever, which is the one thing this module exists
/// not to do.
fn run_capped(mut command: Command, limit: Duration) -> Run {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return Run::Failed(err),
    };
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            // `try_wait` reaps, so the process table does not grow with probes.
            Ok(Some(_)) => {
                let mut bytes = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut bytes);
                }
                return Run::Exited(String::from_utf8_lossy(&bytes).into_owned());
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Run::Timeout(limit);
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Run::Failed(err);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Capability {
        Capability::Ready("python 3.12".to_string())
    }

    fn available(
        docx: Capability,
        xlsx: Capability,
        pptx: Capability,
        convert: Capability,
    ) -> Availability {
        Availability {
            python: ready(),
            docx,
            xlsx,
            pptx,
            convert,
        }
    }

    fn nothing() -> Availability {
        Availability {
            python: Capability::Missing,
            docx: Capability::Missing,
            xlsx: Capability::Missing,
            pptx: Capability::Missing,
            convert: Capability::Missing,
        }
    }

    #[test]
    fn a_complete_machine_has_nothing_to_refuse() {
        let availability = available(ready(), ready(), ready(), ready());
        assert!(availability.refusal().is_none());
        assert!(availability.absent().is_empty());
        assert!(availability.undecided().is_empty());
        assert!(availability.summary().contains("docx=python 3.12"));
        assert!(availability.docx.is_ready());
    }

    #[test]
    fn the_refusal_names_the_library_that_is_missing() {
        let availability = available(
            ready(),
            Capability::Missing,
            Capability::Missing,
            Capability::Missing,
        );
        let refusal = availability.refusal().expect("a refusal");
        assert!(refusal.contains("xlsx (openpyxl)"), "{refusal}");
        assert!(refusal.contains("pptx (python-pptx)"), "{refusal}");
        assert!(refusal.contains("LibreOffice"), "{refusal}");
        // The capability that works is not named as unavailable.
        assert!(!refusal.contains("docx (python-docx)"), "{refusal}");
    }

    #[test]
    fn the_advice_names_only_what_is_actually_missing() {
        // The machine this project is developed on: all three libraries import,
        // LibreOffice is absent. Telling that operator to `pip install` three
        // packages the interpreter already imports is advice that sends them to
        // fix something that is not broken.
        let availability = available(ready(), ready(), ready(), Capability::Missing);
        let refusal = availability.refusal().expect("a refusal");
        assert!(refusal.contains("format conversion"), "{refusal}");
        assert!(refusal.contains("Install LibreOffice"), "{refusal}");
        assert!(!refusal.contains("pip install"), "{refusal}");
        assert!(!refusal.contains("python-docx"), "{refusal}");
    }

    #[test]
    fn an_undecided_probe_is_never_reported_as_missing() {
        let availability = available(
            ready(),
            ready(),
            ready(),
            Capability::Unknown("timed out after 3s".to_string()),
        );
        assert!(availability.absent().is_empty());
        let refusal = availability.refusal().expect("a refusal");
        assert!(refusal.contains("could not be probed"), "{refusal}");
        assert!(refusal.contains("timed out after 3s"), "{refusal}");
        assert!(availability.summary().contains("unknown (timed out"));
    }

    #[test]
    fn a_box_without_an_interpreter_names_every_absent_capability() {
        let availability = nothing();
        assert_eq!(
            availability.absent(),
            vec!["docx", "xlsx", "pptx", "convert"]
        );
        assert!(availability.summary().contains("convert=missing"));
        assert!(!availability.python.is_ready());
    }

    #[test]
    fn the_script_marker_parses_the_version_and_the_modules() {
        let text = "forge-office python 3.12\nforge-office module docx\n";
        let answer = parse_answer("python", text).expect("an answer");
        assert_eq!(answer.label, "python 3.12");
        assert_eq!(answer.modules, vec!["docx"]);
    }

    #[test]
    fn a_launcher_other_than_python_is_named_in_the_label() {
        let text = "forge-office python 3.12\n";
        let answer = parse_answer("py -3", text).expect("an answer");
        assert_eq!(answer.label, "python 3.12 via py -3");
    }

    #[test]
    fn output_without_the_marker_is_not_an_interpreter() {
        // A Windows Store stub prints a complaint and exits; it must not count.
        assert!(parse_answer("python", "Python was not found; run without arguments\n").is_none());
    }

    #[test]
    fn a_program_that_is_not_installed_is_missing_not_unknown() {
        let outcome = run_capped(
            Command::new("forge-definitely-not-a-program-2c9f"),
            Duration::from_secs(1),
        );
        match &outcome {
            Run::Failed(err) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected a failed spawn, got {other:?}"),
        }
        assert!(
            outcome.undecided().is_none(),
            "an uninstalled program is a negative answer, not an undecided one"
        );
    }

    #[test]
    fn the_budget_shrinks_to_the_remaining_time_and_stops_at_zero() {
        assert!(budget(Instant::now() - Duration::from_secs(1)).is_none());
        let far = budget(Instant::now() + Duration::from_secs(3600)).expect("a budget");
        assert_eq!(far, PER_PROBE);
        let near = budget(Instant::now() + Duration::from_millis(200)).expect("a budget");
        assert!(near <= Duration::from_millis(200));
    }

    #[test]
    fn probing_this_machine_terminates_with_an_answer() {
        // The whole point of the module: it returns, and every capability is
        // decided one way or another. Which answer this box gives is not
        // asserted, because that is a fact about the machine, not the code.
        let availability = probe();
        for (name, capability) in availability.named() {
            let label = capability.label();
            assert!(!label.is_empty(), "{name} has no label");
        }
    }

    #[test]
    fn the_probe_script_keeps_its_python_indentation() {
        // A Rust string written with `\`-continued lines silently drops the
        // leading whitespace of every continuation line, which turned the
        // script into an IndentationError that exited 1 and reported all three
        // libraries as missing on a box where all three import. The script is
        // `concat!`ed now; this test keeps it that way by checking the one
        // property that broke: a block opener must be followed by indentation.
        let lines: Vec<&str> = PROBE_SCRIPT.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.trim_end().ends_with(':') {
                continue;
            }
            let next = lines.get(index + 1).copied().unwrap_or_default();
            assert!(
                next.starts_with("    "),
                "line {} opens a block but line {} is not indented: {next:?}",
                index + 1,
                index + 2
            );
        }
    }
}
