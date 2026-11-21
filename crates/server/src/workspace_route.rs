//! The workspace as a reader and as a writer see it: list a directory, read a
//! file, list the changes git knows about, and save an edited buffer.
//!
//! The UI needs the tree the agent is working in. Until these routes existed the
//! only way to read a file from a browser was to ask the *model* to read it and
//! parse the tool result — which costs a turn, records a tool call in the log,
//! and answers a question the UI was already able to ask.
//!
//! Three decisions worth stating:
//!
//! * **The workspace comes from the session's own log**, not from the process
//!   or the request. `session/start.workspace` is what the agent was told it may
//!   touch; reading anything else here would give the UI a wider reach than the
//!   tools it is drawing.
//! * **Saving is a person's act, not the model's**, so it is not a `tool/call`.
//!   The policy and the approval prompt exist to constrain what the *model* may
//!   do without being watched; a person at a keyboard is not that. What a save
//!   needs is to be *reviewable*, and that is what the `file/write` event buys:
//!   a record of which bytes replaced which, so a later disagreement between
//!   "what the agent read" and "what is on disk" has an explanation.
//! * **A refused save records nothing.** A read-only session, an escaping path
//!   and a stale `expectedSha256` all fail before anything is written, and a
//!   line describing something that did not happen is worse than no line.
//!
//! Path checks are the ones a containment boundary needs: no absolute paths, no
//! `..`, no drive letters or UNC prefixes, and — after the path exists — a
//! canonicalised prefix check, because a symlink inside the workspace can point
//! outside it.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use hearth_core::Event;
use serde_json::{json, Value};

/// How much of a file the UI is handed in one response.
///
/// A megabyte is a few hundred pages of source; anything larger is a data file
/// whose bytes an editor would not render anyway, and the response stays small
/// enough that a phone browser can open it.
pub const MAX_TEXT_BYTES: u64 = 1 << 20;

/// The largest buffer a save may carry.
///
/// The same megabyte as the read cap, for the same reason: the editor can only
/// have shown what a read handed it, so accepting more than it can display would
/// be accepting a body nobody reviewed.
pub const MAX_WRITE_BYTES: usize = MAX_TEXT_BYTES as usize;

/// How much of an existing file is inspected before it is overwritten.
///
/// A NUL byte anywhere in the first block means the file is not text, and a
/// buffer the editor never rendered should not be replaced by one it did.
const SNIFF_BYTES: usize = 8 << 10;

/// A counter for temporary file names, so two saves in one process cannot
/// collide even when they land in the same directory within the same second.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    /// A save carried more bytes than a request may.
    TooLarge,
    /// A save would have replaced a file the editor never rendered as text.
    Binary,
    /// The file changed since the reader was given its digest.
    Conflict,
    /// The bytes did not land.
    Write,
    /// A revert was asked for a path git does not have a committed version of.
    ///
    /// An untracked file is the case this exists for: throwing it away is
    /// deletion, and deletion is not a revert — the operator asked to go back to
    /// a version, and there is no version to go back to.
    NotTracked,
    /// The workspace is not inside a git repository (or git could not run).
    NoRepository,
    /// git refused to restore the file, carrying its own explanation.
    Revert(String),
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
    /// The digest of the bytes the reader now holds, or empty when the answer is
    /// not the whole file.
    ///
    /// An editor hands this back on save so a write cannot land on top of bytes
    /// nobody saw. It is empty — rather than the digest of a prefix — when the
    /// body was truncated or binary, because a token that describes only the
    /// first megabyte would let a save replace the rest of a file unseen.
    pub sha256: String,
}

/// What a successful save reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// One row of `GET /diff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub status: &'static str,
    pub additions: u64,
    pub deletions: u64,
}

/// What `GET /diff?path=` answers: one file's unified diff.
///
/// The list route says *which* files changed and by how much; a reviewer needs
/// the lines. Sending the unified diff rather than a parsed hunk structure is a
/// deliberate choice: `git` already writes that format, every diff reader in the
/// world parses it, and a server-side parser would be a second implementation of
/// a format whose edge cases (no newline at end of file, binary markers, rename
/// headers) are the interesting part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffBody {
    pub path: String,
    /// The same words `Change.status` uses, plus `unchanged` for a file whose
    /// bytes match what is committed.
    pub status: &'static str,
    pub text: String,
    pub truncated: bool,
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
        // 编辑器与工具链的垃圾：列出 `.git` 的树，会把读面板的人真正要找的文件挤下去。
        // `target` 与 `node_modules` 同理，而且它们是**大**垃圾——这棵树自己的
        // `target` 有 4 GB，列一个目录要读几万个条目，为的是让 `node_modules`
        // 出现在一份源码清单里。
        if name == ".git" || name == "target" || name == "node_modules" {
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
    // A digest is only a token the editor can hand back if it describes the
    // whole file; a truncated or binary body has no such token.
    let sha256 = if truncated || binary {
        String::new()
    } else {
        hearth_core::sha256_hex(&bytes)
    };
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
        sha256,
    })
}

/// Save an edited buffer back into the workspace.
///
/// `expected` is the digest the reader was given when it opened the file. When
/// it is present the file must still hash to it: an editor that opened a file
/// ten minutes ago must not silently undo an agent that rewrote it since. When
/// it is present and the file is gone, that is a conflict too — the reader is
/// describing a file that no longer exists.
pub fn write_file(
    workspace: &str,
    relative: &str,
    text: &str,
    expected: Option<&str>,
) -> Result<Written, Problem> {
    let parts = components(relative)?;
    if parts.is_empty() {
        // The workspace root is a directory, and no buffer replaces one.
        return Err(Problem::NotAFile);
    }
    if text.len() > MAX_WRITE_BYTES {
        return Err(Problem::TooLarge);
    }
    let (clean, path) = resolve(workspace, relative)?;
    let parent = match path.parent() {
        Some(parent) => parent.to_path_buf(),
        None => return Err(Problem::BadPath),
    };
    // Saving a new file into a directory that is not there is almost always a
    // typo in the path rather than a request to build a tree; say so.
    match fs::metadata(&parent) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(Problem::NotADirectory),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Problem::NotFound)
        }
        Err(_) => return Err(Problem::Read),
    }

    let wanted = expected.filter(|text| !text.is_empty());
    match fs::metadata(&path) {
        Ok(meta) => {
            if meta.is_dir() {
                return Err(Problem::NotAFile);
            }
            if meta.len() > MAX_TEXT_BYTES {
                // The reader was handed a prefix, so it has no authority over
                // the rest of the file.
                return Err(Problem::TooLarge);
            }
            if smells_binary(&path)? {
                return Err(Problem::Binary);
            }
            if let Some(wanted) = wanted {
                let actual = digest_file(&path)?;
                if actual != wanted {
                    return Err(Problem::Conflict);
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if wanted.is_some() {
                // The buffer claims to be editing a file that is no longer
                // there; writing it would be a create nobody asked for.
                return Err(Problem::Conflict);
            }
        }
        Err(_) => return Err(Problem::Read),
    }

    write_atomically(&parent, &path, text.as_bytes())?;
    Ok(Written {
        path: clean,
        bytes: text.len() as u64,
        sha256: hearth_core::sha256_hex(text.as_bytes()),
    })
}

/// Put a file back to the bytes git has committed for it.
///
/// A revert is not a save. A save writes what a person typed; a revert throws the
/// working-tree change away and asks git for the committed version. Three
/// consequences follow, and all three are failures rather than quiet fallbacks:
///
/// - An untracked file is refused (`NotTracked`) instead of deleted. Removing a
///   file somebody created is not "going back", it is data loss with a
///   reassuring name.
/// - A workspace with no repository is refused (`NoRepository`): there is no
///   committed version anywhere, so there is nothing this route could mean.
/// - `git` refusing is reported with git's own words (`Revert`), because the
///   reason it gives ("local changes would be overwritten") is the only useful
///   thing to tell the operator.
pub fn revert_file(workspace: &str, relative: &str) -> Result<Written, Problem> {
    let parts = components(relative)?;
    if parts.is_empty() {
        // The workspace root is a directory, and git cannot revert one.
        return Err(Problem::NotAFile);
    }
    let (clean, path) = resolve(workspace, relative)?;
    if path.is_dir() {
        return Err(Problem::NotAFile);
    }
    let look = parts.join("/");

    let status =
        git(workspace, &["status", "--porcelain", "--", &look]).ok_or(Problem::NoRepository)?;
    let status = status
        .lines()
        .next()
        .map(|line| status_word(&line[..2.min(line.len())]))
        .unwrap_or("unchanged");
    if status == "untracked" {
        return Err(Problem::NotTracked);
    }

    // `HEAD:<path>` is resolved from the repository root, while this route's
    // paths are relative to the workspace, and the workspace may be a
    // subdirectory. `--show-prefix` is how git itself spells that difference.
    let prefix = git(workspace, &["rev-parse", "--show-prefix"]).unwrap_or_default();
    let at_head = format!("HEAD:{}{look}", prefix.trim());
    if git(workspace, &["cat-file", "-e", &at_head]).is_none() {
        // Tracked, but no committed version: a file added to the index and never
        // committed. There is still nothing to go back to.
        return Err(Problem::NotTracked);
    }

    // `checkout HEAD -- <path>` restores both the index and the working tree, so
    // a staged edit is discarded too. Doing it by half would leave a state where
    // the diff list and the disk disagree.
    if let Err(detail) = git_checked(workspace, &["checkout", "HEAD", "--", &look]) {
        return Err(Problem::Revert(detail));
    }

    let bytes = fs::metadata(&path).map_err(|_| Problem::Read)?.len();
    let sha256 = digest_file(&path)?;
    Ok(Written {
        path: clean,
        bytes,
        sha256,
    })
}

/// Does the first block of this file look like something an editor cannot show?
///
/// Only the first block is inspected: a NUL byte anywhere in it settles the
/// question, and reading a large file to answer "is this text" would charge the
/// whole file for a decision the editor already made on the read side.
fn smells_binary(path: &Path) -> Result<bool, Problem> {
    let mut handle = fs::File::open(path).map_err(|_| Problem::Read)?;
    // Empty, not a pre-sized buffer: `read_to_end` appends, so a zero-filled
    // buffer would put its own zeros in front of the file and every text file
    // would then look binary.
    let mut head = Vec::new();
    std::io::Read::by_ref(&mut handle)
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut head)
        .map_err(|_| Problem::Read)?;
    Ok(head.contains(&0))
}

/// The digest of a whole file, read in blocks so a large one need not be held.
fn digest_file(path: &Path) -> Result<String, Problem> {
    let mut handle = fs::File::open(path).map_err(|_| Problem::Read)?;
    let mut context = hearth_core::Sha256::new();
    let mut chunk = vec![0u8; SNIFF_BYTES];
    loop {
        let read = std::io::Read::read(&mut handle, &mut chunk).map_err(|_| Problem::Read)?;
        if read == 0 {
            break;
        }
        context.update(&chunk[..read]);
    }
    Ok(context.finish())
}

/// Write bytes by way of a temporary file in the same directory, then rename.
///
/// A partial write that is visible under the target name is the one failure
/// mode an editor cannot recover from: the reader would see a file that is
/// half old and half new with no way to tell. `rename` over an existing file is
/// atomic on both platforms this runs on.
fn write_atomically(parent: &Path, path: &Path, bytes: &[u8]) -> Result<(), Problem> {
    let name = match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => return Err(Problem::BadPath),
    };
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{name}.hearth-{}-{counter}.tmp",
        std::process::id()
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        // The rename is the commit point; without this the name can point at
        // bytes the filesystem has not been told about.
        file.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
        return Err(Problem::Write);
    }
    if fs::rename(&temp, path).is_err() {
        let _ = fs::remove_file(&temp);
        return Err(Problem::Write);
    }
    Ok(())
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

/// One file's unified diff, for a reviewer who needs the lines.
///
/// `git` writes the format and the frontend reads it; this function only decides
/// *which* diff is the right one to show. That question has three answers:
///
/// * an untracked file has no diff — `git diff` prints nothing for it — so the
///   answer is synthesised from the file's own bytes with every line added. This
///   is what a reviewer of a new file actually wants to see.
/// * a tracked file is compared against `HEAD`, which covers staged and unstaged
///   changes in one answer. Without a commit to compare against (a repository
///   with no history) the comparison falls back to the index.
/// * a binary file is named rather than rendered: a diff of a PNG is a diff of
///   noise, and `git` itself says `Binary files differ`.
pub fn diff_for(workspace: &str, relative: &str) -> Result<DiffBody, Problem> {
    let parts = components(relative)?;
    if parts.is_empty() {
        return Err(Problem::NotAFile);
    }
    let path = parts.join("/");

    let status = match git(workspace, &["status", "--porcelain", "--", &path]) {
        Some(status) => status
            .lines()
            .next()
            .map(|line| status_word(&line[..2.min(line.len())]))
            .unwrap_or("unchanged"),
        // No repository, or git could not answer: the file is not something this
        // route can describe as a change.
        None => "unchanged",
    };

    if status == "untracked" {
        let body = read_file(workspace, &path)?;
        let mut text = String::new();
        text.push_str(&format!("--- /dev/null\n+++ b/{path}\n"));
        if body.binary {
            text.push_str(&format!(
                "Binary file {path} ({} bytes); open it to read the bytes.\n",
                body.size
            ));
        } else {
            for line in body.text.split_inclusive('\n') {
                text.push('+');
                text.push_str(line);
            }
            if !body.text.is_empty() && !body.text.ends_with('\n') {
                text.push_str("\n\\ No newline at end of file\n");
            }
        }
        let (text, truncated) = cut(text, body.truncated);
        return Ok(DiffBody {
            path,
            status,
            text,
            truncated,
        });
    }

    let diff = git(
        workspace,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-U3",
            "HEAD",
            "--",
            &path,
        ],
    )
    .or_else(|| {
        git(
            workspace,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "-U3",
                "--cached",
                "--",
                &path,
            ],
        )
    })
    .unwrap_or_default();

    // `git` prints the path relative to the repository root, which may sit above
    // the workspace; leaving its headers alone keeps the output a valid diff.
    let (text, truncated) = cut(diff, false);
    Ok(DiffBody {
        path,
        status,
        text,
        truncated,
    })
}

/// Cut a body at the read cap, reporting whether anything was dropped.
fn cut(text: String, already: bool) -> (String, bool) {
    if already || text.len() as u64 > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES as usize;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return (text[..end].to_string(), true);
    }
    (text, false)
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

/// Run a git command that changes the tree, keeping its own explanation on
/// failure.
///
/// The read helper above answers `None` because a read that cannot run and a read
/// that found nothing are the same fact. A write is different: git's stderr is
/// the only thing that says *why* it refused, and a route that threw that away
/// would leave the operator with "it did not work".
fn git_checked(workspace: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("--no-pager")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("git exited with {}", output.status)
        } else {
            detail
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
            "sha256": self.sha256,
        })
    }
}

impl Written {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "bytes": self.bytes,
            "sha256": self.sha256,
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

impl DiffBody {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "status": self.status,
            "text": self.text,
            "truncated": self.truncated,
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
        // 构建产物也是垃圾，而且是大垃圾：列它一次要读几万个条目，而它从来不是源码。
        fs::create_dir_all(root.join("target").join("debug")).expect("target dir");
        fs::create_dir_all(root.join("node_modules").join("left-pad")).expect("modules dir");
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

    /// A real repository, or `None` when `git` cannot run here.
    ///
    /// Returning `None` rather than panicking keeps the suites below honest on a
    /// machine without git: they report a skip instead of a failure that names
    /// the wrong thing.
    fn repo(name: &str) -> Option<PathBuf> {
        fn run(dir: &Path, args: &[&str]) -> bool {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false)
        }
        let root = scratch(name);
        if !run(&root, &["init", "--quiet"]) {
            return None;
        }
        Some(root)
    }

    #[test]
    fn a_tracked_change_is_shown_as_the_lines_that_moved() {
        let Some(root) = repo("diff-tracked") else {
            return;
        };
        fs::write(root.join("a.txt"), "one\n").expect("file");
        // Committed, not merely staged: a staged-but-uncommitted file reads as
        // "added", and the point of this test is a *modification*.
        assert!(Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["add", "a.txt"])
            .output()
            .expect("add")
            .status
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["commit", "--quiet", "-m", "first"])
            .output()
            .expect("commit")
            .status
            .success());
        fs::write(root.join("a.txt"), "one\ntwo\n").expect("file");
        let workspace = root.to_str().expect("utf8");
        let diff = diff_for(workspace, "a.txt").expect("diff");
        assert_eq!(diff.path, "a.txt");
        assert_eq!(diff.status, "modified");
        assert!(diff.text.contains("+two"), "text was {:?}", diff.text);
        assert!(!diff.truncated);
    }

    #[test]
    fn an_untracked_file_is_shown_as_every_line_added() {
        let Some(root) = repo("diff-untracked") else {
            return;
        };
        fs::write(root.join("new.txt"), "alpha\nbeta\n").expect("file");
        let workspace = root.to_str().expect("utf8");
        let diff = diff_for(workspace, "new.txt").expect("diff");
        assert_eq!(diff.status, "untracked");
        // `git diff` prints nothing for an untracked path, so the answer is
        // synthesised: without this a reviewer of a brand-new file sees an empty
        // panel and concludes nothing changed.
        assert!(
            diff.text.contains("+++ b/new.txt"),
            "text was {:?}",
            diff.text
        );
        assert!(diff.text.contains("+alpha"));
        assert!(diff.text.contains("+beta"));
    }

    #[test]
    fn a_file_whose_bytes_are_committed_has_no_lines_to_show() {
        let Some(root) = repo("diff-clean") else {
            return;
        };
        fs::write(root.join("same.txt"), "steady\n").expect("file");
        let workspace = root.to_str().expect("utf8");
        let diff = diff_for(workspace, "same.txt").expect("diff");
        assert_eq!(diff.status, "untracked");
        // Once it is committed there is nothing to review, and the empty text is
        // the honest answer rather than a synthesised "everything is new".
        assert!(Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["add", "same.txt"])
            .output()
            .expect("add")
            .status
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["commit", "--quiet", "-m", "first"])
            .output()
            .expect("commit")
            .status
            .success());
        let diff = diff_for(workspace, "same.txt").expect("diff");
        assert_eq!(diff.status, "unchanged");
        assert_eq!(diff.text, "");
    }

    #[test]
    fn a_diff_may_not_climb_out_of_the_workspace() {
        let Some(root) = repo("diff-climb") else {
            return;
        };
        let workspace = root.to_str().expect("utf8");
        assert_eq!(diff_for(workspace, "../outside.txt"), Err(Problem::BadPath));
        assert_eq!(diff_for(workspace, ""), Err(Problem::NotAFile));
    }

    #[test]
    fn a_save_replaces_the_bytes_and_reports_the_new_digest() {
        let root = scratch("write");
        fs::write(root.join("note.txt"), "before\n").expect("file");
        let workspace = root.to_str().expect("utf8");
        let opened = read_file(workspace, "note.txt").expect("read");
        let saved =
            write_file(workspace, "note.txt", "after\n", Some(&opened.sha256)).expect("save");
        assert_eq!(saved.path, "note.txt");
        assert_eq!(saved.bytes, 6);
        assert_eq!(
            fs::read_to_string(root.join("note.txt")).expect("disk"),
            "after\n"
        );
        assert_eq!(saved.sha256, hearth_core::sha256_hex(b"after\n"));
        // The digest the save reported is the one a later read will hand back,
        // which is what makes it usable as a token.
        assert_eq!(
            read_file(workspace, "note.txt").expect("read").sha256,
            saved.sha256
        );
    }

    #[test]
    fn a_save_creates_a_file_that_is_not_there_yet() {
        let root = scratch("create");
        let workspace = root.to_str().expect("utf8");
        let saved = write_file(workspace, "fresh.txt", "hello", None).expect("save");
        assert_eq!(saved.bytes, 5);
        assert_eq!(
            fs::read_to_string(root.join("fresh.txt")).expect("disk"),
            "hello"
        );
    }

    #[test]
    fn a_stale_digest_is_a_conflict_and_the_file_is_left_alone() {
        let root = scratch("conflict");
        fs::write(root.join("note.txt"), "original\n").expect("file");
        let workspace = root.to_str().expect("utf8");
        let stale = hearth_core::sha256_hex(b"something else");
        assert_eq!(
            write_file(workspace, "note.txt", "mine\n", Some(&stale)),
            Err(Problem::Conflict)
        );
        assert_eq!(
            fs::read_to_string(root.join("note.txt")).expect("disk"),
            "original\n"
        );
        // A digest for a file that is gone is the same disagreement.
        assert_eq!(
            write_file(workspace, "gone.txt", "x", Some(&stale)),
            Err(Problem::Conflict)
        );
        assert!(!root.join("gone.txt").exists());
    }

    #[test]
    fn a_save_may_not_climb_out_of_the_workspace_nor_replace_a_directory() {
        let root = scratch("write-bounds");
        fs::create_dir_all(root.join("ws")).expect("ws");
        fs::create_dir_all(root.join("ws").join("dir")).expect("dir");
        fs::write(root.join("inside.txt"), "a").expect("file");
        let workspace = root.join("ws");
        let workspace = workspace.to_str().expect("utf8");
        assert_eq!(
            write_file(workspace, "../inside.txt", "x", None),
            Err(Problem::BadPath)
        );
        assert_eq!(
            write_file(workspace, "C:/Windows/win.ini", "x", None),
            Err(Problem::BadPath)
        );
        assert_eq!(write_file(workspace, "", "x", None), Err(Problem::NotAFile));
        assert_eq!(
            write_file(workspace, "dir", "x", None),
            Err(Problem::NotAFile)
        );
        // The file outside the workspace must still hold its original bytes.
        assert_eq!(
            fs::read_to_string(root.join("inside.txt")).expect("disk"),
            "a"
        );
    }

    #[test]
    fn a_save_refuses_to_overwrite_a_binary_or_oversized_file() {
        let root = scratch("write-guards");
        fs::write(root.join("blob.bin"), [0u8, 1, 2, 3]).expect("file");
        let big = vec![b'x'; (MAX_TEXT_BYTES + 8) as usize];
        fs::write(root.join("big.txt"), &big).expect("file");
        let workspace = root.to_str().expect("utf8");
        assert_eq!(
            write_file(workspace, "blob.bin", "text", None),
            Err(Problem::Binary)
        );
        assert_eq!(
            write_file(workspace, "big.txt", "text", None),
            Err(Problem::TooLarge)
        );
        let oversized = "x".repeat(MAX_WRITE_BYTES + 1);
        assert_eq!(
            write_file(workspace, "note.txt", &oversized, None),
            Err(Problem::TooLarge)
        );
    }

    #[test]
    fn a_save_leaves_no_temporary_file_behind() {
        let root = scratch("write-temp");
        fs::write(root.join("note.txt"), "before").expect("file");
        let workspace = root.to_str().expect("utf8");
        write_file(workspace, "note.txt", "after", None).expect("save");
        let names: Vec<String> = fs::read_dir(&root)
            .expect("dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["note.txt".to_string()]);
    }

    #[test]
    fn a_read_of_a_file_that_is_too_large_to_hold_carries_no_digest() {
        let root = scratch("write-token");
        let big = vec![b'x'; (MAX_TEXT_BYTES + 8) as usize];
        fs::write(root.join("big.txt"), &big).expect("file");
        let read = read_file(root.to_str().expect("utf8"), "big.txt").expect("read");
        assert!(read.truncated);
        // Empty, not the digest of the prefix: a token describing the first
        // megabyte would authorise replacing the rest unseen.
        assert_eq!(read.sha256, "");
    }

    /// Write a file and commit it, so later steps have a committed version.
    fn commit(root: &Path, name: &str, text: &str) {
        fs::write(root.join(name), text).expect("file");
        for args in [vec!["add", name], vec!["commit", "--quiet", "-m", "c"]] {
            let done = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(&args)
                .output()
                .expect("git");
            assert!(done.status.success(), "git {args:?} failed");
        }
    }

    #[test]
    fn a_revert_puts_the_committed_bytes_back_and_reports_them() {
        let Some(root) = repo("revert-basic") else {
            return;
        };
        commit(&root, "a.txt", "committed\n");
        fs::write(root.join("a.txt"), "scribbled over\n").expect("file");
        let workspace = root.to_str().expect("utf8");

        let written = revert_file(workspace, "a.txt").expect("revert");
        assert_eq!(written.path, "a.txt");
        // Compare against what landed, not against the literal: `git checkout`
        // rewrites line endings when the repository asks for it (autocrlf on
        // Windows), so the committed blob "committed\n" arrives as
        // "committed\r\n" and the digest describes those bytes. Asserting the
        // literal would make this test a statement about the developer's git
        // configuration.
        let bytes = fs::read(root.join("a.txt")).expect("read");
        assert_eq!(written.bytes, bytes.len() as u64);
        assert_eq!(written.sha256, hearth_core::sha256_hex(&bytes));
        assert_eq!(
            String::from_utf8(bytes).expect("utf8").trim_end(),
            "committed"
        );
        assert_eq!(
            fs::read_to_string(root.join("a.txt"))
                .expect("read")
                .trim_end(),
            "committed"
        );
    }

    #[test]
    fn a_revert_brings_back_a_file_that_was_deleted() {
        let Some(root) = repo("revert-deleted") else {
            return;
        };
        commit(&root, "a.txt", "committed\n");
        fs::remove_file(root.join("a.txt")).expect("remove");
        let workspace = root.to_str().expect("utf8");

        revert_file(workspace, "a.txt").expect("revert");
        assert_eq!(
            fs::read_to_string(root.join("a.txt"))
                .expect("read")
                .trim_end(),
            "committed"
        );
    }

    #[test]
    fn a_revert_refuses_an_untracked_file_instead_of_deleting_it() {
        let Some(root) = repo("revert-untracked") else {
            return;
        };
        fs::write(root.join("draft.txt"), "mine\n").expect("file");
        let workspace = root.to_str().expect("utf8");
        assert_eq!(
            revert_file(workspace, "draft.txt"),
            Err(Problem::NotTracked)
        );
        // Refused means untouched: a revert is not a delete.
        assert_eq!(
            fs::read_to_string(root.join("draft.txt")).expect("read"),
            "mine\n"
        );
    }

    #[test]
    fn a_revert_refuses_a_file_that_was_never_committed() {
        let Some(root) = repo("revert-staged") else {
            return;
        };
        // Staged but never committed: tracked, and still nothing to go back to.
        fs::write(root.join("staged.txt"), "new\n").expect("file");
        assert!(Command::new("git")
            .arg("-C")
            .arg(&root)
            .arg("add")
            .arg("staged.txt")
            .output()
            .expect("add")
            .status
            .success());
        let workspace = root.to_str().expect("utf8");
        assert_eq!(
            revert_file(workspace, "staged.txt"),
            Err(Problem::NotTracked)
        );
    }

    #[test]
    fn a_revert_in_a_directory_with_no_repository_says_so() {
        let root = scratch("revert-norepo");
        fs::write(root.join("a.txt"), "text\n").expect("file");
        assert_eq!(
            revert_file(root.to_str().expect("utf8"), "a.txt"),
            Err(Problem::NoRepository)
        );
    }

    #[test]
    fn a_revert_may_not_climb_out_of_the_workspace() {
        let Some(root) = repo("revert-climb") else {
            return;
        };
        assert_eq!(
            revert_file(root.to_str().expect("utf8"), "../outside.txt"),
            Err(Problem::BadPath)
        );
    }
}
