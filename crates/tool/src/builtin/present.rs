//! `present`: hand the operator a file that already exists.
//!
//! Presenting is a handoff, not a transfer. The tool resolves the path inside the
//! workspace the turn was given, proves it is a regular file of a sensible size,
//! and appends one line to `<data_dir>/presented.jsonl`, which is what the user
//! interface watches to offer the file for opening. The file itself is never
//! copied and its bytes never enter the conversation: a binary or a large report
//! would destroy the transcript that is supposed to stay readable.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use forge_llm::ToolSpec;
use serde::Deserialize;
use serde_json::json;

use crate::contract::{
    decode_args, Call, Result, CODE_DENIED, CODE_IO_ERROR, CODE_NOT_FOUND, ERR_BAD_ARGS,
};
use crate::truncate::{effective_limits, truncate, Limits};
use crate::Tool;

/// Code for a path that is not inside the workspace the turn was given.
pub const PRESENT_CODE_OUTSIDE_WORKSPACE: &str = "outside_workspace";
/// Code for a path that resolves but is not a regular file: a directory, a
/// device, a pipe.
pub const PRESENT_CODE_NOT_A_FILE: &str = "not_a_file";
/// Code for a regular file above the handoff cap.
pub const PRESENT_CODE_TOO_LARGE: &str = "too_large";

/// A handoff is a pointer, not an upload. 8 MiB covers reports, screenshots and
/// small archives; anything larger the operator should open in place anyway.
const MAX_PRESENT_BYTES: u64 = 8 << 20;
/// The manifest the user interface reads. One JSON object per line, appended.
const MANIFEST_NAME: &str = "presented.jsonl";
/// Where the manifest goes when no data directory was configured.
const FALLBACK_MANIFEST_DIR: &str = ".forge";
/// The description is one line by contract. Cap it so a runaway model cannot
/// write a multi-megabyte line into the manifest.
const MAX_DESCRIPTION_CHARS: usize = 500;

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    description: String,
}

/// The `present` tool.
pub struct PresentTool {
    limits: Limits,
    data_dir: PathBuf,
}

impl PresentTool {
    pub fn new(limits: Limits, data_dir: PathBuf) -> Self {
        Self { limits, data_dir }
    }

    /// The manifest for calls made in `workspace`, plus whether the fallback
    /// location was used because no data directory was configured.
    fn manifest_path(&self, workspace: &Path) -> (PathBuf, bool) {
        if self.data_dir.as_os_str().is_empty() {
            (
                workspace.join(FALLBACK_MANIFEST_DIR).join(MANIFEST_NAME),
                true,
            )
        } else {
            (self.data_dir.join(MANIFEST_NAME), false)
        }
    }
}

impl Tool for PresentTool {
    fn spec(&self) -> ToolSpec {
        let description = format!(
            "Hand the operator a file that already exists on disk, so they can open it themselves. \
             Presenting is a handoff, not a transfer: the file is not copied, its contents are not \
             read, and nothing is added to the conversation except the path and the size.\n\n\
             When to use: you produced a report, an image, a document, an archive, a spreadsheet, a \
             log or any other artifact that is meant to be opened with a viewer, and the user should \
             see it or download it. Use it at the end of the job that created the file, after the \
             command or fs_write that produced it succeeded.\n\n\
             When not to use: to show short text, a code snippet, a diff or an explanation — put that \
             in your reply instead. Do not use it to read a file back: it returns no contents. Do not \
             use it for a file outside the workspace: it refuses those rather than copying them in.\n\n\
             What it does: checks that {MANIFEST_NAME} can record the file (inside the data directory, \
             or `<workspace>/.forge` when none is configured), then appends one JSON line \
             {{\"ts\":<unix millis>,\"path\":<absolute path>,\"size\":<bytes>,\"description\":<text>}} \
             to the manifest. A refused call (missing path, a directory, more than 8 MiB) records \
             nothing and says why, so read the error instead of retrying the same arguments.\n\n\
             The path may be relative to the workspace root or absolute, but it must resolve inside \
             the workspace, symlinks and `..` included, and it must be a regular file. Empty files \
             are accepted and called out in the result.\n\n\
             Example arguments:\n\
             {{\"path\":\"reports/q3-summary.md\",\"description\":\"Q3 summary report with the revenue table\"}}"
        );
        forge_llm::ToolSpec {
            name: "present".to_string(),
            description,
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to an existing regular file, relative to the workspace root or absolute. Must resolve inside the workspace. `../` escapes and directories are rejected."
                    },
                    "description": {
                        "type": "string",
                        "description": "One line saying what the file is, shown next to it in the operator's list. Optional; keep it short and concrete."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: Args = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };

        let raw = args.path.trim();
        if raw.is_empty() {
            return Result::error(
                ERR_BAD_ARGS,
                format!(
                    "{}: path is empty; pass the file to hand to the operator, for example {{\"path\":\"reports/q3.md\"}}",
                    call.name
                ),
            );
        }

        let workspace = match workspace_root(&call) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let target = match resolve_in_workspace(&call, &workspace, raw) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let metadata = match fs::metadata(&target) {
            Ok(metadata) => metadata,
            Err(err) => return metadata_error(&call, raw, err),
        };

        let file_type = metadata.file_type();
        let displayed = shown(&target);
        if !file_type.is_file() {
            if file_type.is_dir() {
                let example = shown(&target.join("report.md"));
                return Result::error(
                    PRESENT_CODE_NOT_A_FILE,
                    format!(
                        "{}: {displayed:?} is a directory, not a file; present hands over exactly one file, so pass a file inside it (for example {example:?}) or create the artifact first",
                        call.name
                    ),
                );
            }
            return Result::error(
                PRESENT_CODE_NOT_A_FILE,
                format!(
                    "{}: {displayed:?} is not a regular file (it is a device, a pipe or another special file); present only registers regular files the operator can open in an editor or viewer",
                    call.name
                ),
            );
        }

        let size = metadata.len();
        if size > MAX_PRESENT_BYTES {
            return Result::error(
                PRESENT_CODE_TOO_LARGE,
                format!(
                    "{}: {displayed:?} is {size} bytes ({}), above the {MAX_PRESENT_BYTES}-byte (8 MiB) handoff cap; present registers a path and does not upload or copy, so point the operator at a smaller artifact, split the file, or mention its path in the reply instead",
                    call.name,
                    human_size(size)
                ),
            )
            .with_meta("size", json!(size))
            .with_meta("maxSize", json!(MAX_PRESENT_BYTES));
        }

        let mut description = args.description.trim().replace(['\r', '\n'], " ");
        let mut description_cut = false;
        if description.chars().count() > MAX_DESCRIPTION_CHARS {
            description = description.chars().take(MAX_DESCRIPTION_CHARS).collect();
            description.push('…');
            description_cut = true;
        }

        let (manifest, fell_back) = self.manifest_path(&workspace);
        let record = json!({
            "ts": unix_millis(),
            "path": displayed,
            "size": size,
            "description": description,
        });
        if let Err(err) = append_manifest(&manifest, &record) {
            return Result::error(
                CODE_IO_ERROR,
                format!(
                    "{}: the manifest {:?} could not be appended: {err}; the file was not registered, so the operator was not told about {displayed:?}. Check that the directory exists and is writable, then call present again",
                    call.name,
                    shown(&manifest)
                ),
            );
        }

        let mut text = String::new();
        text.push_str(&format!("presented: {displayed}\n"));
        if !description.is_empty() {
            text.push_str(&format!("description: {description}\n"));
        }
        text.push_str(&format!("size: {} ({size} bytes)\n", human_size(size)));
        text.push_str(&format!(
            "registered: one JSON line appended to {:?}; the operator can open the file itself, and nothing was copied or read into the conversation\n",
            shown(&manifest)
        ));
        if size == 0 {
            text.push_str(
                "[note] the file is empty (0 bytes); it was registered anyway, but an empty report, image or document usually means the step that wrote it failed — check the file before telling the user it is ready\n",
            );
        }
        if fell_back {
            text.push_str(&format!(
                "[note] no data directory is configured, so the manifest fell back to {:?}; an interface that watches the configured data directory will not see this entry\n",
                shown(&manifest)
            ));
        }
        if description_cut {
            text.push_str(&format!(
                "[note] description was longer than {MAX_DESCRIPTION_CHARS} characters and was truncated in the manifest\n"
            ));
        }

        let (output, truncated) = truncate(&text, effective_limits(self.limits));
        let mut result = Result::ok(output)
            .with_meta("path", json!(displayed))
            .with_meta("size", json!(size))
            .with_meta("manifest", json!(shown(&manifest)));
        result.truncated = truncated;
        if fell_back {
            result = result.with_meta("fallbackToWorkspace", json!(true));
        }
        if size == 0 {
            result = result.with_meta("empty", json!(true));
        }
        if !description.is_empty() {
            result = result.with_meta("description", json!(description));
        }
        result
    }
}

/// The workspace as an absolute, canonical path.
///
/// `Call.workspace` is absolute by contract, but a relative value (or an empty
/// one) is accepted against the current directory instead of panicking: a tool
/// call is data and must not take the process down.
fn workspace_root(call: &Call) -> std::result::Result<PathBuf, Result> {
    // Only a whitespace-only workspace means "not configured"; spaces inside a
    // real path are left alone.
    let configured = call.workspace.as_str();
    let raw = if configured.trim().is_empty() {
        ""
    } else {
        configured
    };
    let absolute = if raw.is_empty() {
        current_dir(&call.name)?
    } else {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            normalize(&path)
        } else {
            normalize(&current_dir(&call.name)?.join(path))
        }
    };
    match fs::canonicalize(&absolute) {
        Ok(real) => Ok(real),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Err(Result::error(
            CODE_NOT_FOUND,
            format!(
                "{}: workspace {:?} does not exist, so no file inside it can be presented",
                call.name,
                shown(&absolute)
            ),
        )),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => Err(Result::error(
            CODE_DENIED,
            format!(
                "{}: workspace {:?} cannot be resolved: permission denied",
                call.name,
                shown(&absolute)
            ),
        )),
        Err(err) => Err(Result::error(
            CODE_IO_ERROR,
            format!(
                "{}: workspace {:?} cannot be resolved: {err}",
                call.name,
                shown(&absolute)
            ),
        )),
    }
}

fn current_dir(tool: &str) -> std::result::Result<PathBuf, Result> {
    std::env::current_dir().map_err(|err| {
        Result::error(
            CODE_IO_ERROR,
            format!(
                "{tool}: the call has no workspace and the current directory is unavailable: {err}"
            ),
        )
    })
}

/// Resolve `raw` inside `workspace` and prove it did not escape.
///
/// Containment is decided twice. Lexically first, on the component-normalised
/// path, so `..` is settled without touching the filesystem and an escape that
/// points at a missing file still says `outside_workspace` rather than a
/// misleading `not_found`. Then again on the canonical path, which is the only
/// thing that sees through a symlink.
fn resolve_in_workspace(
    call: &Call,
    workspace: &Path,
    raw: &str,
) -> std::result::Result<PathBuf, Result> {
    let requested = Path::new(raw);
    let candidate = if requested.is_absolute() {
        normalize(requested)
    } else {
        normalize(&workspace.join(requested))
    };

    if !requested.is_absolute() && !contained(workspace, &candidate) {
        return Err(outside_error(call, raw, workspace));
    }

    match fs::canonicalize(&candidate) {
        Ok(real) => {
            if !contained(workspace, &real) {
                return Err(outside_error(call, raw, workspace));
            }
            Ok(real)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // The file is missing. When the nearest existing ancestor is
            // already outside the workspace the intent was an escape, so say
            // that instead of blaming a missing file.
            let Some(ancestor) = deepest_existing(&candidate) else {
                return Err(missing_error(call, raw, workspace));
            };
            match fs::canonicalize(&ancestor) {
                Ok(real_ancestor) => {
                    if contained(workspace, &real_ancestor) {
                        Err(missing_error(call, raw, workspace))
                    } else {
                        Err(outside_error(call, raw, workspace))
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    Err(missing_error(call, raw, workspace))
                }
                // Containment cannot be proven, so nothing is handed over.
                Err(err) => Err(Result::error(
                    CODE_IO_ERROR,
                    format!(
                        "{}: cannot tell whether {raw:?} is inside the workspace because {:?} cannot be resolved: {err}",
                        call.name,
                        shown(&ancestor)
                    ),
                )),
            }
        }
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => Err(Result::error(
            CODE_DENIED,
            format!(
                "{}: {raw:?} cannot be resolved: permission denied on the path or one of its parent directories",
                call.name
            ),
        )),
        Err(err) => Err(Result::error(
            CODE_IO_ERROR,
            format!("{}: {raw:?} cannot be resolved: {err}", call.name),
        )),
    }
}

fn outside_error(call: &Call, raw: &str, workspace: &Path) -> Result {
    let root = shown(workspace);
    Result::error(
        PRESENT_CODE_OUTSIDE_WORKSPACE,
        format!(
            "{}: path {raw:?} is outside the workspace {root:?}; present only hands over files that already exist inside the workspace the turn was given, and it does not copy files in. Move the file under {root:?} and pass a path relative to it",
            call.name
        ),
    )
}

fn missing_error(call: &Call, raw: &str, workspace: &Path) -> Result {
    Result::error(
        CODE_NOT_FOUND,
        format!(
            "{}: {raw:?} does not exist under the workspace {:?}; present only hands over a file that is already on disk. Write or generate it first (fs_write, or the command that produces it), then call present with the path that now exists",
            call.name,
            shown(workspace)
        ),
    )
}

/// A metadata error after resolution, which is rare: the file was canonicalized
/// a moment ago and has since been removed, or cannot be stat()ed.
fn metadata_error(call: &Call, raw: &str, err: io::Error) -> Result {
    match err.kind() {
        io::ErrorKind::NotFound => Result::error(
            CODE_NOT_FOUND,
            format!(
                "{}: {raw:?} disappeared after it was resolved; create the file, then call present again",
                call.name
            ),
        ),
        io::ErrorKind::PermissionDenied => Result::error(
            CODE_DENIED,
            format!("{}: {raw:?} cannot be inspected: permission denied", call.name),
        ),
        _ => Result::error(
            CODE_IO_ERROR,
            format!("{}: {raw:?} cannot be inspected: {err}", call.name),
        ),
    }
}

/// Append one JSON object as one line. The manifest is appended, never
/// rewritten: it is a log, and a concurrent reader must see whole lines.
fn append_manifest(manifest: &Path, record: &serde_json::Value) -> io::Result<()> {
    if let Some(parent) = manifest.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut line = serde_json::to_string(record)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(manifest)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Walk up to the deepest path that exists, so a missing file's intended
/// location can still be checked for containment.
fn deepest_existing(path: &Path) -> Option<PathBuf> {
    let mut cursor = Some(path);
    while let Some(current) = cursor {
        if fs::symlink_metadata(current).is_ok() {
            return Some(current.to_path_buf());
        }
        cursor = current
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
    }
    None
}

/// Path component normalisation: drop `.`, fold `..` into the preceding
/// component, and keep a root root. No filesystem access, so this is safe on a
/// path that does not exist.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                // `pop` refuses to remove a root or a drive prefix, so `/..`
                // stays `/` and never becomes a relative `..`.
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// Is `candidate` the workspace root or something below it?
///
/// Both sides are compared as whole components: the separator is added to the
/// workspace before the prefix test, so `C:\work2` is never inside `C:\work`,
/// and Windows paths are folded to lower case, because the filesystem does not
/// distinguish `C:\Work` from `C:\work`.
fn contained(workspace: &Path, candidate: &Path) -> bool {
    let ws = comparison_key(workspace);
    let path = comparison_key(candidate);
    if ws.is_empty() || path.is_empty() {
        return false;
    }
    if ws == "/" {
        return path.starts_with('/');
    }
    path == ws || path.starts_with(&format!("{ws}/"))
}

fn comparison_key(path: &Path) -> String {
    let mut text = shown(path).replace('\\', "/");
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// A path with the Windows verbatim prefix removed, for messages and the
/// manifest: `\\?\C:\work\a.md` reads `C:\work\a.md`.
fn shown(path: &Path) -> String {
    strip_verbatim(&path.to_string_lossy())
}

fn strip_verbatim(text: &str) -> String {
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => text.to_string(),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique directory under the system temp dir, removed when the test ends.
    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            let unique = format!(
                "forge-present-{tag}-{}-{nanos}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).expect("create temp tree");
            Self { path }
        }

        fn dir(&self, name: &str) -> PathBuf {
            let path = self.path.join(name);
            fs::create_dir_all(&path).expect("create temp dir");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn call_in(workspace: &Path, args: serde_json::Value) -> Call {
        Call {
            id: "c1".into(),
            name: "present".into(),
            args,
            workspace: workspace.to_string_lossy().into_owned(),
        }
    }

    fn manifest_records(manifest: &Path) -> Vec<serde_json::Value> {
        let text = fs::read_to_string(manifest).expect("manifest is readable");
        assert!(
            text.ends_with('\n'),
            "every manifest line ends with a newline: {text:?}"
        );
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("manifest line is JSON"))
            .collect()
    }

    #[test]
    fn presents_an_existing_file_and_appends_one_valid_json_line() {
        let tree = TempTree::new("ok");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let content = b"# Quarterly report\nrevenue: 42\n";
        let file = workspace.join("report.md");
        fs::write(&file, content).expect("write file");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(
            &workspace,
            json!({"path": "report.md", "description": "quarterly report"}),
        ));

        assert!(result.is_ok(), "{}", result.output);
        assert!(!result.is_error);
        assert_eq!(result.code, "");
        assert!(!result.truncated);
        assert!(result.output.contains("report.md"), "{}", result.output);
        assert!(
            result.output.contains(&format!("{} bytes", content.len())),
            "{}",
            result.output
        );
        assert!(
            result.output.to_lowercase().contains("registered"),
            "{}",
            result.output
        );
        assert_eq!(result.meta.get("size"), Some(&json!(content.len())));

        let records = manifest_records(&data.join("presented.jsonl"));
        assert_eq!(records.len(), 1, "exactly one line per call: {records:?}");
        let record = &records[0];
        assert!(
            record["ts"].as_i64().unwrap_or_default() > 0,
            "ts is unix millis: {record}"
        );
        let recorded_path = record["path"].as_str().expect("path is a string");
        assert!(
            Path::new(recorded_path).is_absolute(),
            "the manifest records an absolute path: {recorded_path}"
        );
        assert!(
            Path::new(recorded_path).ends_with("report.md"),
            "path is absolute and ends at the file: {recorded_path}"
        );
        assert!(
            !recorded_path.contains("\\\\?\\"),
            "the verbatim prefix is stripped: {recorded_path}"
        );
        assert_eq!(record["size"], json!(content.len()));
        assert_eq!(record["description"], json!("quarterly report"));
        assert_eq!(
            record.as_object().expect("record is an object").len(),
            4,
            "the line carries exactly ts, path, size and description: {record}"
        );
    }

    #[test]
    fn second_call_appends_instead_of_rewriting() {
        let tree = TempTree::new("append");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        fs::write(workspace.join("a.txt"), b"a").expect("write a");
        fs::write(workspace.join("b.txt"), b"b").expect("write b");
        let tool = PresentTool::new(Limits::default(), data.clone());

        assert!(tool
            .call(call_in(&workspace, json!({"path": "a.txt"})))
            .is_ok());
        assert!(tool
            .call(call_in(&workspace, json!({"path": "b.txt"})))
            .is_ok());

        let records = manifest_records(&data.join("presented.jsonl"));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["description"], json!(""));
        assert!(records[0]["path"].as_str().unwrap_or("").ends_with("a.txt"));
        assert!(records[1]["path"].as_str().unwrap_or("").ends_with("b.txt"));
    }

    #[test]
    fn directory_is_refused_with_its_own_code() {
        let tree = TempTree::new("dir");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let folder = workspace.join("out");
        fs::create_dir_all(&folder).expect("create out");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "out"})));

        assert!(result.is_error);
        assert_eq!(result.code, PRESENT_CODE_NOT_A_FILE);
        assert!(result.output.contains("directory"), "{}", result.output);
        assert!(
            result.output.contains("out"),
            "the message names the offending path: {}",
            result.output
        );
        assert!(!data.join("presented.jsonl").exists(), "nothing registered");
    }

    #[test]
    fn parent_escape_is_refused_even_when_the_file_exists() {
        let tree = TempTree::new("escape");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let outside = tree.path.join("escape.txt");
        fs::write(&outside, b"secret").expect("write outside");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "../escape.txt"})));

        assert!(result.is_error);
        assert_eq!(result.code, PRESENT_CODE_OUTSIDE_WORKSPACE);
        assert_eq!(result.code, "outside_workspace");
        assert!(!data.join("presented.jsonl").exists(), "nothing registered");

        // A missing file outside the workspace is still an escape, not a
        // not_found: the intent is what the caller must fix.
        let missing = tool.call(call_in(&workspace, json!({"path": "../not-there.txt"})));
        assert_eq!(missing.code, PRESENT_CODE_OUTSIDE_WORKSPACE);
    }

    #[test]
    fn parent_dir_that_stays_inside_is_allowed() {
        let tree = TempTree::new("inside-dotdot");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let sub = workspace.join("sub");
        fs::create_dir_all(&sub).expect("create sub");
        fs::write(workspace.join("report.md"), b"ok").expect("write report");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "sub/../report.md"})));

        assert!(result.is_ok(), "{}", result.output);
        assert_eq!(manifest_records(&data.join("presented.jsonl")).len(), 1);
    }

    #[test]
    fn missing_file_is_not_found() {
        let tree = TempTree::new("missing");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "nope.txt"})));

        assert!(result.is_error);
        assert_eq!(result.code, CODE_NOT_FOUND);
        assert!(result.output.contains("nope.txt"), "{}", result.output);
        assert!(!data.join("presented.jsonl").exists());
    }

    #[test]
    fn oversize_file_is_refused_before_anything_is_registered() {
        let tree = TempTree::new("big");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let file = workspace.join("big.bin");
        // Sparse on purpose: set_len reserves no data blocks.
        let handle = fs::File::create(&file).expect("create big file");
        handle
            .set_len(MAX_PRESENT_BYTES + 1)
            .expect("set sparse length");
        drop(handle);

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "big.bin"})));

        assert!(result.is_error);
        assert_eq!(result.code, PRESENT_CODE_TOO_LARGE);
        assert!(
            result.output.contains(&(MAX_PRESENT_BYTES + 1).to_string()),
            "the message gives the actual size: {}",
            result.output
        );
        assert!(
            result.output.contains(&MAX_PRESENT_BYTES.to_string()),
            "the message gives the cap: {}",
            result.output
        );
        assert!(result.output.contains("8 MiB"), "{}", result.output);
        assert_eq!(result.meta.get("maxSize"), Some(&json!(MAX_PRESENT_BYTES)));
        assert!(!data.join("presented.jsonl").exists());
    }

    #[test]
    fn empty_file_is_presented_with_a_note() {
        let tree = TempTree::new("empty");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        fs::write(workspace.join("empty.md"), b"").expect("write empty");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "empty.md"})));

        assert!(result.is_ok(), "{}", result.output);
        assert!(result.output.contains("empty"), "{}", result.output);
        assert!(result.output.contains("0 bytes"), "{}", result.output);
        assert_eq!(result.meta.get("empty"), Some(&json!(true)));
        assert_eq!(manifest_records(&data.join("presented.jsonl")).len(), 1);
    }

    #[test]
    fn empty_data_dir_falls_back_to_the_workspace_and_says_so() {
        let tree = TempTree::new("fallback");
        let workspace = tree.dir("ws");
        fs::write(workspace.join("report.md"), b"hello").expect("write report");

        let tool = PresentTool::new(Limits::default(), PathBuf::new());
        let result = tool.call(call_in(&workspace, json!({"path": "report.md"})));

        assert!(result.is_ok(), "{}", result.output);
        assert!(
            result.output.contains(".forge"),
            "the fallback location is named: {}",
            result.output
        );
        assert!(
            result.output.contains("fell back"),
            "the degradation is stated: {}",
            result.output
        );
        assert_eq!(result.meta.get("fallbackToWorkspace"), Some(&json!(true)));
        let manifest = workspace.join(".forge").join("presented.jsonl");
        assert!(manifest.is_file(), "manifest at {}", manifest.display());
        assert_eq!(manifest_records(&manifest).len(), 1);
    }

    #[test]
    fn empty_path_is_bad_args() {
        let tree = TempTree::new("empty-path");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let tool = PresentTool::new(Limits::default(), data.clone());

        for path in ["", "   "] {
            let result = tool.call(call_in(&workspace, json!({"path": path})));
            assert!(result.is_error);
            assert_eq!(result.code, ERR_BAD_ARGS);
            assert!(result.output.contains("path"), "{}", result.output);
        }
        assert!(!data.join("presented.jsonl").exists());
    }

    #[test]
    fn absolute_path_inside_the_workspace_is_accepted() {
        let tree = TempTree::new("absolute");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let file = workspace.join("report.md");
        fs::write(&file, b"hello").expect("write report");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": file.to_string_lossy()})));

        assert!(result.is_ok(), "{}", result.output);
        assert_eq!(manifest_records(&data.join("presented.jsonl")).len(), 1);
    }

    #[test]
    fn spec_requires_a_path_and_describes_the_handoff() {
        let tool = PresentTool::new(Limits::default(), PathBuf::from("."));
        let spec = tool.spec();

        assert_eq!(spec.name, "present");
        let required = spec.schema["required"]
            .as_array()
            .expect("required is an array");
        assert!(required.contains(&json!("path")), "{required:?}");
        assert!(spec.schema["properties"]["path"].is_object());
        assert!(spec.schema["properties"]["description"].is_object());
        assert_eq!(spec.schema["additionalProperties"], json!(false));

        let description = spec.description.to_lowercase();
        assert!(description.contains("operator"), "{}", spec.description);
        assert!(description.contains("not copied"), "{}", spec.description);
        assert!(
            description.contains("when not to use"),
            "{}",
            spec.description
        );
        // The documented call must still be valid JSON after the format! that
        // builds the description: a stray brace would teach the model a bad
        // argument shape.
        let example = "{\"path\":\"reports/q3-summary.md\",\"description\":\"Q3 summary report with the revenue table\"}";
        assert!(
            spec.description.contains(example),
            "the example arguments are documented verbatim: {}",
            spec.description
        );
        serde_json::from_str::<serde_json::Value>(example).expect("the documented example is JSON");
    }

    #[test]
    fn missing_data_dir_is_created() {
        let tree = TempTree::new("mkdir");
        let workspace = tree.dir("ws");
        let data = tree.path.join("nested").join("data");
        assert!(!data.exists());
        fs::write(workspace.join("report.md"), b"hi").expect("write report");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "report.md"})));

        assert!(result.is_ok(), "{}", result.output);
        assert!(data.is_dir(), "the data directory was created");
        assert_eq!(manifest_records(&data.join("presented.jsonl")).len(), 1);
    }

    #[test]
    fn symlink_that_leaves_the_workspace_is_refused() {
        let tree = TempTree::new("symlink");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        let outside = tree.dir("outside");
        fs::write(outside.join("secret.txt"), b"secret").expect("write secret");
        let link = workspace.join("link");
        if !make_dir_link(&outside, &link) {
            // Unprivileged Windows cannot create symlinks; the canonical-path
            // check is exercised on hosts that can, and the lexical check
            // covers `..` everywhere.
            eprintln!("[skip] this host does not allow creating a directory symlink");
            return;
        }

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": "link/secret.txt"})));

        assert!(result.is_error, "{}", result.output);
        assert_eq!(result.code, PRESENT_CODE_OUTSIDE_WORKSPACE);
        assert!(!data.join("presented.jsonl").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_case_insensitively() {
        let tree = TempTree::new("case");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        fs::write(workspace.join("Report.md"), b"hello").expect("write report");
        let shouty = workspace.to_string_lossy().to_ascii_uppercase();
        let path = format!("{shouty}\\REPORT.MD");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(&workspace, json!({"path": path})));

        assert!(result.is_ok(), "{}", result.output);
        assert_eq!(manifest_records(&data.join("presented.jsonl")).len(), 1);
    }

    #[test]
    fn description_newlines_are_folded_into_one_line() {
        let tree = TempTree::new("description");
        let workspace = tree.dir("ws");
        let data = tree.dir("data");
        fs::write(workspace.join("report.md"), b"hi").expect("write report");

        let tool = PresentTool::new(Limits::default(), data.clone());
        let result = tool.call(call_in(
            &workspace,
            json!({"path": "report.md", "description": "first\nsecond\r\nthird"}),
        ));

        assert!(result.is_ok(), "{}", result.output);
        let records = manifest_records(&data.join("presented.jsonl"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["description"], json!("first second  third"));
    }

    fn make_dir_link(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = (target, link);
            false
        }
    }

    #[test]
    fn human_size_reads_like_a_person_wrote_it() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(9), "9 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(MAX_PRESENT_BYTES), "8.0 MiB");
    }

    #[test]
    fn verbatim_prefix_is_stripped_for_display() {
        assert_eq!(strip_verbatim(r"\\?\C:\work\a.md"), r"C:\work\a.md");
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\a.md"),
            r"\\server\share\a.md"
        );
        assert_eq!(strip_verbatim(r"C:\work\a.md"), r"C:\work\a.md");
    }

    #[test]
    fn workspace_sibling_with_a_shared_prefix_is_not_inside() {
        let tree = TempTree::new("sibling");
        let workspace = tree.dir("work");
        let sibling = tree.dir("work2");
        let file = sibling.join("a.txt");
        fs::write(&file, b"x").expect("write");

        // Same guard the real containment check uses, exercised without a
        // symbolic link: "work2" must not count as a child of "work".
        assert!(contained(&workspace, &workspace.join("a.txt")));
        assert!(contained(&workspace, &workspace));
        assert!(!contained(&workspace, &file));
        assert!(!contained(&workspace, &sibling));
    }
}
