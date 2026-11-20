//! The workspace as a reader sees it: list a directory, read a file, list the
//! changes git knows about.
//!
//! Three GETs, and none of them can write. The editor UI needs to show the tree
//! the agent is working in, and until this route existed the only way to read a
//! file from a browser was to ask the *model* to read it and parse the tool
//! result — which costs a turn, records a tool call in the log, and answers a
//! question the UI was already able to ask.
//!
//! Two decisions worth stating:
//!
//! * **The workspace comes from the session's own log**, not from the process
//!   or the request. `session/start.workspace` is what the agent was told it may
//!   touch; reading anything else here would give the UI a wider reach than the
//!   tools it is drawing.
//! * **Writing stays out.** A file written by the UI would bypass the policy,
//!   the approval prompt and the `tool/call` record — three things that make a
//!   session reviewable. Edits go through the agent, as they always did.
//!
//! Path checks are the ones a containment boundary needs: no absolute paths, no
//! `..`, no drive letters or UNC prefixes, and — after the path exists — a
//! canonicalised prefix check, because a symlink inside the workspace can point
//! outside it.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use hearth_core::Event;
use serde_json::{json, Value};

/// How much of a file the UI is handed in one response.
///
/// A megabyte is a few hundred pages of source; anything larger is a data file
/// whose bytes an editor would not render anyway, and the response stays small
/// enough that a phone browser can open it.
pub const MAX_TEXT_BYTES: u64 = 1 << 20;

/// What went wrong, in the shape the route layer turns into a status code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The session log is not there.
    NoSession,
    /// The log is there but its bytes did not come back.
    Read,
    /// The path itself is not acceptable (absolute, `..`, a drive letter).
    BadPath,
    /// The path is acceptable but names nothing.
    NotFound,
    /// The path is acceptable but is not a file/directory as asked.
    NotAFile,
    NotADirectory,
}

/// One entry in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: &'static str,
    pub size: Option<u64>,
}

/// What `GET /files` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
}

/// What `GET /file` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBody {
    pub path: String,
    pub size: u64,
    pub text: String,
    pub truncated: bool,
    pub binary: bool,
}

/// One row of `GET /diff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub status: &'static str,
    pub additions: u64,
    pub deletions: u64,
}

/// The workspace a session was started in, read from its own log.
///
/// Read-only in the strict sense: the file is opened for reading and nothing is
/// created, because a GET must not manufacture a session (this is the same
/// reason `goal_route` exists instead of using `Store::open`).
pub fn workspace_of(log_path: &Path) -> Result<String, Problem> {
    let text = match fs::read_to_string(log_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Problem::NoSession)
        }
        Err(_) => return Err(Problem::Read),
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Event = match serde_json::from_str(line) {
            Ok(event) => event,
            // A torn last line is not this route's business; a session whose
            // start record is unreadable simply has no workspace to report.
            Err(_) => continue,
        };
        if event.typ.as_str() != "session/start" {
            continue;
        }
        let Some(payload) = event.payload.as_ref() else {
            continue;
        };
        for key in ["workspace", "cwd"] {
            if let Some(value) = payload.get(key).and_then(Value::as_str) {
                if !value.trim().is_empty() {
                    return Ok(value.to_string());
                }
            }
        }
    }
    // A log with no start record is a shape the writer never produces; saying
    // "no session" is more honest than inventing a workspace from the process.
    Err(Problem::NoSession)
}

/// Split a request path into components, refusing anything that could climb.
///
/// The rules are deliberately absolute rather than clever: no leading `/`, no
/// `\`, no `:`, and no component that is `..`. A caller that wants a file one
/// level up has misunderstood what a workspace is, and guessing which escape was
/// meant is how a containment check becomes decoration.
pub fn components(relative: &str) -> Result<Vec<String>, Problem> {
    let trimmed = relative.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') || trimmed.starts_with("//") {
        return Err(Problem::BadPath);
    }
    if trimmed.contains(':') {
        // `C:` and `file:` both arrive as a colon; neither is a workspace path.
        return Err(Problem::BadPath);
    }
    let mut out = Vec::new();
    for part in trimmed.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return Err(Problem::BadPath),
            other => out.push(other.to_string()),
        }
    }
    Ok(out)
}

/// Join a checked relative path onto a workspace and prove the result is inside.
///
/// The lexical check above already refuses `..`; this exists for the case that
/// check cannot see: a symlink *inside* the workspace that points out of it.
/// Canonicalising both sides answers that, and it also normalises the case
/// differences Windows hands back.
pub fn resolve(workspace: &str, relative: &str) -> Result<(String, PathBuf), Problem> {
    let parts = components(relative)?;
    let mut path = PathBuf::from(workspace);
    for part in &parts {
        path.push(part);
    }
    let clean = parts.join("/");
    let root = fs::canonicalize(workspace).map_err(|_| Problem::NotFound)?;
    let target = match fs::canonicalize(&path) {
        Ok(target) => target,
        // Nothing there yet: the lexical check is all there is to check, and it
        // already refused every way out.
        Err(_) => return Ok((clean, path)),
    };
    if !target.starts_with(&root) {
        return Err(Problem::BadPath);
    }
    Ok((clean, target))
}

/// List one directory of the workspace.
pub fn list_dir(workspace: &str, relative: &str) -> Result<Listing, Problem> {
    let (clean, path) = resolve(workspace, relative)?;
    let readable = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Problem::NotFound)
        }
        Err(_) => return Err(Problem::Read),
    };
    if !readable.is_dir() {
        return Err(Problem::NotADirectory);
    }
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(_) => return Err(Problem::Read),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Editor and tooling droppings: a tree that lists `.git` is a tree that
        // hides the files someone opened the panel to find.
        if name == ".git" {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let (kind, size) = if meta.is_dir() {
            ("dir", None)
        } else if meta.is_file() {
            ("file", Some(meta.len()))
        } else {
            // Sockets, pipes and device nodes are not editable; showing them as
            // files would offer a click that cannot work.
            continue;
        };
        out.push(Entry { name, kind, size });
    }
    // Directories first, then by name, case-insensitively: the order a file
    // explorer uses, and the order a reader can predict.
    out.sort_by(|left, right| {
        // "dir" sorts before "file", so the plain ascending kind comparison is
        // exactly "directories first".
        let by_kind = left.kind.cmp(right.kind);
        by_kind.then_with(|| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        })
    });
    Ok(Listing {
        path: clean,
        entries: out,
    })
}

/// Read one file of the workspace as text.
///
/// A file that is not UTF-8 is reported as `binary` with empty text rather than
/// as an error: the file is there, and the UI's job is to say so instead of
/// pretending the path was wrong.
pub fn read_file(workspace: &str, relative: &str) -> Result<FileBody, Problem> {
    let (clean, path) = resolve(workspace, relative)?;
    let meta = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Problem::NotFound)
        }
        Err(_) => return Err(Problem::Read),
    };
    if meta.is_dir() {
        return Err(Problem::NotAFile);
    }
    let mut handle = match fs::File::open(&path) {
        Ok(handle) => handle,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Problem::NotFound)
        }
        Err(_) => return Err(Problem::Read),
    };
    let mut bytes = Vec::new();
    let read = std::io::Read::by_ref(&mut handle)
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes);
    if read.is_err() {
        return Err(Problem::Read);
    }
    let truncated = bytes.len() as u64 > MAX_TEXT_BYTES;
    if truncated {
        bytes.truncate(MAX_TEXT_BYTES as usize);
    }
    let binary = bytes.contains(&0) || std::str::from_utf8(&bytes).is_err();
    let text = if binary {
        String::new()
    } else {
        // Safe by the check above; a lossy conversion would be the alternative
        // and it would change bytes the reader then believes it has.
        String::from_utf8(bytes).unwrap_or_default()
    };
    Ok(FileBody {
        path: clean,
        size: meta.len(),
        text,
        truncated,
        binary,
    })
}

/// The changes git knows about in the workspace.
///
/// `git` is the only honest source: it already knows the ignore rules, the index
/// and the branch. A workspace that is not a repository answers with an empty
/// list, because "not versioned" is a fact and not an error.
pub fn list_diff(workspace: &str) -> Result<Vec<Change>, Problem> {
    let Some(status) = git(workspace, &["status", "--porcelain"]) else {
        return Ok(Vec::new());
    };
    // Counts come from the diff against HEAD when there is one, and from the
    // worktree when there is not (a repository with no commit yet).
    let mut counted = std::collections::BTreeMap::new();
    for args in [
        vec!["diff", "--numstat", "HEAD"],
        vec!["diff", "--numstat"],
        vec!["diff", "--numstat", "--cached"],
    ] {
        if let Some(numstat) = git(workspace, &args) {
            for line in numstat.lines() {
                let mut fields = line.split('\t');
                let Some(additions) = fields.next() else {
                    continue;
                };
                let Some(deletions) = fields.next() else {
                    continue;
                };
                let Some(path) = fields.next() else { continue };
                let entry = counted.entry(path.to_string()).or_insert((0u64, 0u64));
                entry.0 = entry.0.max(additions.parse().unwrap_or(0));
                entry.1 = entry.1.max(deletions.parse().unwrap_or(0));
            }
        }
    }
    let mut out = Vec::new();
    for line in status.lines() {
        // `git status --porcelain` is `XY <path>`; a rename carries ` -> `.
        if line.len() < 4 {
            continue;
        }
        let code = &line[..2];
        let mut path = line[3..].trim().to_string();
        if let Some((_, renamed)) = path.rsplit_once(" -> ") {
            path = renamed.to_string();
        }
        if path.is_empty() {
            continue;
        }
        let status = status_word(code);
        let (additions, deletions) = counted.get(&path).copied().unwrap_or((0, 0));
        out.push(Change {
            path,
            status,
            additions,
            deletions,
        });
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(out)
}

fn status_word(code: &str) -> &'static str {
    let mut chars = code.chars();
    let index = chars.next().unwrap_or(' ');
    let worktree = chars.next().unwrap_or(' ');
    if index == '?' || worktree == '?' {
        return "untracked";
    }
    if index == 'R' || worktree == 'R' {
        return "renamed";
    }
    if index == 'A' || worktree == 'A' {
        return "added";
    }
    if index == 'D' || worktree == 'D' {
        return "deleted";
    }
    if index == 'U' || worktree == 'U' {
        return "conflicted";
    }
    "modified"
}

/// Run one read-only git command in `workspace`; `None` when it cannot run.
fn git(workspace: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("--no-pager")
        .args(args)
        // Never prompt, never take a lock for a read, never page.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

impl Listing {
    pub fn to_json(&self) -> Value {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|entry| {
                let mut row = json!({"name": entry.name, "kind": entry.kind});
                if let Some(size) = entry.size {
                    row["size"] = json!(size);
                }
                row
            })
            .collect();
        json!({"path": self.path, "entries": entries})
    }
}

impl FileBody {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "size": self.size,
            "text": self.text,
            "truncated": self.truncated,
            "binary": self.binary,
        })
    }
}

impl Change {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "status": self.status,
            "additions": self.additions,
            "deletions": self.deletions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir()
            .join("hearth-workspace-route")
            .join(name);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch");
        path
    }

    #[test]
    fn a_relative_path_is_split_and_a_climb_is_refused() {
        assert_eq!(components("").expect("empty"), Vec::<String>::new());
        assert_eq!(components(".").expect("dot"), Vec::<String>::new());
        assert_eq!(
            components("src/main.rs").expect("plain"),
            vec!["src".to_string(), "main.rs".to_string()]
        );
        assert_eq!(
            components("./src//main.rs").expect("noise"),
            vec!["src".to_string(), "main.rs".to_string()]
        );
        assert_eq!(components("/etc/passwd"), Err(Problem::BadPath));
        assert_eq!(components("C:/Windows"), Err(Problem::BadPath));
        assert_eq!(components("..\\..\\secret"), Err(Problem::BadPath));
        assert_eq!(components("src/../../secret"), Err(Problem::BadPath));
    }

    #[test]
    fn a_listing_sorts_directories_first_and_hides_the_repository_metadata() {
        let root = scratch("listing");
        fs::create_dir_all(root.join("zeta")).expect("dir");
        fs::create_dir_all(root.join(".git")).expect("git dir");
        fs::write(root.join("Beta.txt"), "b").expect("file");
        fs::write(root.join("alpha.txt"), "a").expect("file");
        let listing = list_dir(root.to_str().expect("utf8"), "").expect("listing");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["zeta", "alpha.txt", "Beta.txt"]);
        assert_eq!(listing.entries[0].kind, "dir");
        assert_eq!(listing.entries[1].size, Some(1));
        assert_eq!(listing.path, "");
    }

    #[test]
    fn a_path_through_a_symlink_out_of_the_workspace_is_refused() {
        let root = scratch("symlink");
        fs::create_dir_all(root.join("ws")).expect("ws");
        fs::write(root.join("outside.txt"), "secret").expect("outside");
        let link = root.join("ws").join("escape");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(root.join("outside.txt"), &link);
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(root.join("outside.txt"), &link);
        if made.is_err() {
            // Creating a symlink can need a privilege this account lacks; the
            // lexical check is still covered above.
            return;
        }
        let workspace = root.join("ws");
        let refused = read_file(workspace.to_str().expect("utf8"), "escape");
        assert_eq!(refused, Err(Problem::BadPath));
    }

    #[test]
    fn a_file_is_read_and_a_binary_one_is_named_rather_than_guessed() {
        let root = scratch("read");
        fs::write(root.join("note.txt"), "hello\n").expect("file");
        fs::write(root.join("blob.bin"), [0u8, 1, 2, 3]).expect("file");
        let workspace = root.to_str().expect("utf8");
        let text = read_file(workspace, "note.txt").expect("text");
        assert_eq!(text.text, "hello\n");
        assert_eq!(text.size, 6);
        assert!(!text.truncated);
        assert!(!text.binary);

        let blob = read_file(workspace, "blob.bin").expect("blob");
        assert!(blob.binary);
        assert_eq!(blob.text, "");

        assert_eq!(read_file(workspace, "nope.txt"), Err(Problem::NotFound));
        assert_eq!(read_file(workspace, ".."), Err(Problem::BadPath));
    }

    #[test]
    fn a_large_file_is_cut_at_the_cap_and_says_so() {
        let root = scratch("large");
        let body = "x".repeat((MAX_TEXT_BYTES + 32) as usize);
        fs::write(root.join("big.txt"), &body).expect("file");
        let read = read_file(root.to_str().expect("utf8"), "big.txt").expect("read");
        assert!(read.truncated);
        assert_eq!(read.text.len() as u64, MAX_TEXT_BYTES);
        assert_eq!(read.size, MAX_TEXT_BYTES + 32);
    }

    #[test]
    fn a_directory_is_not_a_file_and_a_file_is_not_a_directory() {
        let root = scratch("kinds");
        fs::create_dir_all(root.join("dir")).expect("dir");
        fs::write(root.join("file.txt"), "x").expect("file");
        let workspace = root.to_str().expect("utf8");
        assert_eq!(read_file(workspace, "dir"), Err(Problem::NotAFile));
        assert_eq!(list_dir(workspace, "file.txt"), Err(Problem::NotADirectory));
    }

    #[test]
    fn a_directory_with_no_git_repository_reports_no_changes() {
        let root = scratch("nogit");
        assert_eq!(list_diff(root.to_str().expect("utf8")), Ok(Vec::new()));
    }
}
