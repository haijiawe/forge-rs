//! Five filesystem builtins, ported from `archive/forge-go/internal/tool/builtin/fs.go`.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::contract::{
    decode_args, Call, Result, CODE_DENIED, CODE_IO_ERROR, CODE_NOT_FOUND, CODE_OUTSIDE_WORKSPACE,
    ERR_BAD_ARGS,
};
use crate::truncate::{truncate, DefaultLimits};
use crate::Tool;

pub const CODE_BINARY: &str = "binary";
pub const CODE_AMBIGUOUS: &str = "ambiguous";

const BINARY_SNIFF_BYTES: usize = 8 << 10;
const GLOB_MAX_RESULTS: usize = 500;
const GREP_DEFAULT_MAX: usize = 100;
const GREP_MAX_FILE_BYTES: u64 = 2 << 20;
const OUTPUT_RESERVE: usize = 256;
const SKIP_DIRS: [&str; 7] = [
    ".git",
    ".gocache",
    ".gopath",
    ".gotmp",
    ".cache",
    "node_modules",
    "bin",
];

pub struct FsRead;
pub struct FsWrite;
pub struct FsEdit;
pub struct FsGlob;
pub struct FsGrep;

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    limit: i64,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    include: String,
    #[serde(default)]
    max_results: i64,
}

impl Tool for FsRead {
    fn spec(&self) -> forge_llm::ToolSpec {
        forge_llm::ToolSpec {
            name: "fs_read".into(),
            description: "Read a text file from the workspace and return its raw contents. offset and limit are 1-based inclusive line numbers. Binary files are refused. Example: {\"path\":\"src/main.rs\",\"offset\":200,\"limit\":60}".into(),
            schema: json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string","description":"File path relative to the workspace root."},
                    "offset":{"type":"integer","minimum":1,"description":"First line, 1-based. Default 1."},
                    "limit":{"type":"integer","minimum":1,"description":"Number of lines. Default: all remaining."}
                },
                "required":["path"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: ReadArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if args.path.is_empty() {
            return Result::error(
                ERR_BAD_ARGS,
                "fs_read: path is required (a path relative to the workspace root)",
            );
        }
        if args.offset < 0 || args.limit < 0 {
            return Result::error(
                ERR_BAD_ARGS,
                format!("fs_read: offset and limit are 1-based positive line numbers (got offset={} limit={})", args.offset, args.limit),
            );
        }

        let absolute = match resolve_path(&call, &args.path) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let rel = display_path(&call, &absolute);
        let metadata = match fs::metadata(&absolute) {
            Ok(meta) => meta,
            Err(err) => return path_error("fs_read", &rel, err),
        };
        if metadata.is_dir() {
            return Result::error(
                CODE_NOT_FOUND,
                format!(
                    "fs_read: {rel:?} is a directory, not a file; use fs_glob to list a directory"
                ),
            );
        }
        let (size, binary) = match sniff_binary(&absolute) {
            Ok(value) => value,
            Err(err) => return path_error("fs_read", &rel, err),
        };
        if binary {
            return Result::error(
                CODE_BINARY,
                format!("fs_read: {rel:?} is a binary file ({size} bytes) with a NUL byte in its first {BINARY_SNIFF_BYTES} bytes; refusing to dump binary data into the context. Use the shell tool to inspect it."),
            )
            .with_meta("bytes", json!(size));
        }

        let first = if args.offset == 0 {
            1
        } else {
            args.offset as usize
        };
        let last = if args.limit > 0 {
            first.saturating_add(args.limit as usize).saturating_sub(1)
        } else {
            usize::MAX
        };
        let budget = (DefaultLimits.max_bytes as usize).saturating_sub(OUTPUT_RESERVE);
        let (text, total, cut, cut_at) = match read_range(&absolute, first, last, budget) {
            Ok(value) => value,
            Err(err) => return path_error("fs_read", &rel, err),
        };

        let mut output = if text.is_empty() {
            if total == 0 {
                format!("({rel} is empty: 0 lines)")
            } else if first > total {
                format!("(offset {first} is past the end of {rel}, which has {total} lines)")
            } else {
                format!("(no lines selected: {rel} has {total} lines)")
            }
        } else {
            text
        };
        if cut {
            output.push_str(&format!("\n\n[truncated: {budget}-byte output budget reached at line {cut_at}; read a narrower range with offset/limit]"));
        }
        let (output, truncated) = truncate(&output, DefaultLimits);
        Result::ok(output)
            .with_meta("totalLines", json!(total))
            .with_meta("bytes", json!(size))
            .with_meta("startLine", json!(first))
            .with_truncated(cut || truncated)
    }
}

impl Tool for FsWrite {
    fn spec(&self) -> forge_llm::ToolSpec {
        forge_llm::ToolSpec {
            name: "fs_write".into(),
            description: "Create or overwrite a file with exactly the content given. Parent directories are created. Use fs_edit for a small change. Example: {\"path\":\"notes/todo.md\",\"content\":\"# TODO\\n\"}".into(),
            schema: json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string"},
                    "content":{"type":"string"}
                },
                "required":["path","content"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: WriteArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if args.path.is_empty() {
            return Result::error(
                ERR_BAD_ARGS,
                "fs_write: path is required (a path relative to the workspace root)",
            );
        }
        let absolute = match resolve_path(&call, &args.path) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let rel = display_path(&call, &absolute);
        if let Ok(meta) = fs::metadata(&absolute) {
            if meta.is_dir() {
                return Result::error(
                    CODE_IO_ERROR,
                    format!("fs_write: {rel:?} is a directory; pass a file path"),
                );
            }
        }
        if let Some(parent) = absolute.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                return path_error("fs_write", &rel, err);
            }
        }
        let bytes = args.content.as_bytes();
        if let Err(err) = fs::write(&absolute, bytes) {
            return path_error("fs_write", &rel, err);
        }
        Result::ok(format!("wrote {} bytes to {rel}", bytes.len()))
            .with_meta("bytes", json!(bytes.len()))
    }
}

impl Tool for FsEdit {
    fn spec(&self) -> forge_llm::ToolSpec {
        forge_llm::ToolSpec {
            name: "fs_edit".into(),
            description: "Replace exact text inside an existing file. old is byte-for-byte; pass replaceAll:true when it occurs more than once. Example: {\"path\":\"src/main.rs\",\"old\":\"return nil\",\"new\":\"return Ok(())\"}".into(),
            schema: json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string"},
                    "old":{"type":"string"},
                    "new":{"type":"string"},
                    "replaceAll":{"type":"boolean"}
                },
                "required":["path","old","new"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: EditArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if args.path.is_empty() {
            return Result::error(
                ERR_BAD_ARGS,
                "fs_edit: path is required (a path relative to the workspace root)",
            );
        }
        if args.old.is_empty() {
            return Result::error(ERR_BAD_ARGS, "fs_edit: old is empty; nothing to match. Use fs_write to create or replace a whole file.");
        }
        let absolute = match resolve_path(&call, &args.path) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let rel = display_path(&call, &absolute);
        let metadata = match fs::metadata(&absolute) {
            Ok(meta) => meta,
            Err(err) => return path_error("fs_edit", &rel, err),
        };
        if metadata.is_dir() {
            return Result::error(
                CODE_NOT_FOUND,
                format!("fs_edit: {rel:?} is a directory, not a file"),
            );
        }
        let raw = match fs::read(&absolute) {
            Ok(raw) => raw,
            Err(err) => return path_error("fs_edit", &rel, err),
        };
        let content = String::from_utf8_lossy(&raw).into_owned();
        let count = content.matches(&args.old).count();
        if count == 0 {
            return edit_not_found(&rel, &content, &args.old);
        }
        if count > 1 && !args.replace_all {
            let (lines, total) = occurrences(&content, &args.old, 20);
            return Result::error(
                CODE_AMBIGUOUS,
                format!("fs_edit: old text occurs {total} times in {rel} (at lines {}); pass replaceAll:true to replace every occurrence, or include more surrounding lines so old is unique", format_lines(&lines, total)),
            );
        }
        let updated = if args.replace_all {
            content.replace(&args.old, &args.new)
        } else {
            content.replacen(&args.old, &args.new, 1)
        };
        let replacements = if args.replace_all { count } else { 1 };
        if let Err(err) = fs::write(&absolute, updated.as_bytes()) {
            return path_error("fs_edit", &rel, err);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(
                &absolute,
                fs::Permissions::from_mode(metadata.permissions().mode()),
            );
        }
        Result::ok(format!("replaced {replacements} occurrence(s) in {rel}"))
            .with_meta("replacements", json!(replacements))
    }
}

impl Tool for FsGlob {
    fn spec(&self) -> forge_llm::ToolSpec {
        forge_llm::ToolSpec {
            name: "fs_glob".into(),
            description: "Find files by name or path pattern. A pattern with no slash matches base names at any depth; ** crosses directories. Sorted, at most 500 matches. Example: {\"pattern\":\"**/*_test.go\",\"path\":\"src\"}".into(),
            schema: json!({
                "type":"object",
                "properties":{
                    "pattern":{"type":"string"},
                    "path":{"type":"string"}
                },
                "required":["pattern"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: GlobArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        let pattern = normalize_pattern(&args.pattern);
        if pattern.is_empty() {
            return Result::error(
                ERR_BAD_ARGS,
                "fs_glob: pattern is required, e.g. \"**/*.go\"",
            );
        }
        if let Err(err) = validate_glob(&pattern) {
            return Result::error(
                ERR_BAD_ARGS,
                format!("fs_glob: invalid pattern {:?}: {err}", args.pattern),
            );
        }
        let root = match resolve_path(&call, &args.path) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let root_rel = display_path(&call, &root);
        let metadata = match fs::metadata(&root) {
            Ok(meta) => meta,
            Err(err) => return path_error("fs_glob", &root_rel, err),
        };
        let mut matches = Vec::new();
        if metadata.is_dir() {
            let mut files = Vec::new();
            if let Err(err) = collect_files(&root, &mut files) {
                return path_error("fs_glob", &root_rel, err);
            }
            for file in files {
                let rel = file
                    .strip_prefix(&root)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if glob_match(&pattern, &rel) {
                    matches.push(display_path(&call, &file));
                }
            }
        } else if glob_match(&pattern, &root_rel) {
            matches.push(root_rel.clone());
        }
        matches.sort();
        matches.dedup();
        let truncated = matches.len() > GLOB_MAX_RESULTS;
        matches.truncate(GLOB_MAX_RESULTS);
        if matches.is_empty() {
            return Result::ok(format!(
                "no files matched {:?} under {}",
                args.pattern, root_rel
            ))
            .with_meta("matches", json!(0));
        }
        let mut output = matches.join("\n");
        if truncated {
            output.push_str(&format!("\n[truncated: showing the first {GLOB_MAX_RESULTS} matches; narrow the pattern or path]"));
        }
        let (output, cut) = truncate(&output, DefaultLimits);
        Result::ok(output)
            .with_truncated(truncated || cut)
            .with_meta("matches", json!(matches.len()))
    }
}

impl Tool for FsGrep {
    fn spec(&self) -> forge_llm::ToolSpec {
        forge_llm::ToolSpec {
            name: "fs_grep".into(),
            description: "Search file contents line by line and return matches as path:line: text. RE2 syntax is supported (no lookaround or backreferences). Binary files and files over 2 MB are skipped. Example: {\"pattern\":\"func Test\",\"include\":\"*_test.go\"}".into(),
            schema: json!({
                "type":"object",
                "properties":{
                    "pattern":{"type":"string"},
                    "path":{"type":"string"},
                    "include":{"type":"string"},
                    "maxResults":{"type":"integer","minimum":1}
                },
                "required":["pattern"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: GrepArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if args.pattern.is_empty() {
            return Result::error(
                ERR_BAD_ARGS,
                "fs_grep: pattern is required (a Go/RE2 regular expression)",
            );
        }
        let regex = match Regex::new(&args.pattern) {
            Ok(regex) => regex,
            Err(err) => return Result::error(ERR_BAD_ARGS, format!("fs_grep: invalid regexp {:?}: {err} (Go uses RE2 syntax: lookahead/lookbehind and backreferences are not supported)", args.pattern)),
        };
        if args.max_results < 0 {
            return Result::error(
                ERR_BAD_ARGS,
                format!(
                    "fs_grep: maxResults must be positive, got {}",
                    args.max_results
                ),
            );
        }
        let max_results = if args.max_results == 0 {
            GREP_DEFAULT_MAX
        } else {
            args.max_results as usize
        };
        if !args.include.is_empty() {
            let include = normalize_pattern(&args.include);
            if let Err(err) = validate_glob(&include) {
                return Result::error(
                    ERR_BAD_ARGS,
                    format!("fs_grep: invalid include {:?}: {err}", args.include),
                );
            }
        }

        let root = match resolve_path(&call, &args.path) {
            Ok(path) => path,
            Err(result) => return result,
        };
        let root_rel = display_path(&call, &root);
        let metadata = match fs::metadata(&root) {
            Ok(meta) => meta,
            Err(err) => return path_error("fs_grep", &root_rel, err),
        };
        let budget = (DefaultLimits.max_bytes as usize).saturating_sub(OUTPUT_RESERVE);
        let mut files = Vec::new();
        if metadata.is_dir() {
            if let Err(err) = collect_files(&root, &mut files) {
                return path_error("fs_grep", &root_rel, err);
            }
        } else {
            files.push(root.clone());
        }

        let mut output = String::new();
        let mut count = 0usize;
        let mut cap_reason = String::new();
        'files: for file in files {
            let rel_to_root = file
                .strip_prefix(&root)
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| ".".to_string());
            if !args.include.is_empty()
                && !glob_match(&normalize_pattern(&args.include), &rel_to_root)
            {
                continue;
            }
            let size = match fs::metadata(&file) {
                Ok(meta) if meta.len() <= GREP_MAX_FILE_BYTES => meta.len(),
                Ok(_) => continue,
                Err(_) => continue,
            };
            let text = match read_text_file(&file, size) {
                Some(text) => text,
                None => continue,
            };
            for (index, line) in text.split('\n').enumerate() {
                if !regex.is_match(line.trim_end_matches('\r')) {
                    continue;
                }
                output.push_str(&format!(
                    "{}:{}: {}\n",
                    display_path(&call, &file),
                    index + 1,
                    line
                ));
                count += 1;
                if count >= max_results {
                    cap_reason = format!("maxResults={max_results}");
                    break 'files;
                }
                if output.len() >= budget {
                    cap_reason = "output byte budget".to_string();
                    break 'files;
                }
            }
        }

        if count == 0 {
            return Result::ok(format!(
                "no matches for {:?} under {}",
                args.pattern, root_rel
            ))
            .with_meta("matches", json!(0))
            .with_meta("maxResults", json!(max_results));
        }
        if !cap_reason.is_empty() {
            output.push_str(&format!(
                "[truncated: stopped at {cap_reason}; raise maxResults or narrow path/include]\n"
            ));
        }
        let (output, cut) = truncate(&output, DefaultLimits);
        Result::ok(output)
            .with_truncated(!cap_reason.is_empty() || cut)
            .with_meta("matches", json!(count))
            .with_meta("maxResults", json!(max_results))
    }
}

trait WithTruncated {
    fn with_truncated(self, truncated: bool) -> Self;
}

impl WithTruncated for Result {
    fn with_truncated(mut self, truncated: bool) -> Self {
        self.truncated = truncated;
        self
    }
}

fn resolve_path(call: &Call, raw: &str) -> std::result::Result<PathBuf, Result> {
    match crate::path::resolve(&call.workspace, raw) {
        Ok(path) => Ok(path),
        Err(err) if crate::path::is_outside(&err) => Err(Result::error(
            CODE_OUTSIDE_WORKSPACE,
            format!("{}: path {raw:?} is outside the workspace; use a path relative to the workspace root ({})", call.name, call.workspace),
        )),
        Err(err) => Err(Result::error(CODE_IO_ERROR, format!("{}: path {raw:?}: {err}", call.name))),
    }
}

fn display_path(call: &Call, path: &Path) -> String {
    crate::path::rel(&call.workspace, path)
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"))
}

fn path_error(op: &str, path: &str, err: io::Error) -> Result {
    match err.kind() {
        io::ErrorKind::NotFound => {
            Result::error(CODE_NOT_FOUND, format!("{op}: {path:?} not found"))
        }
        io::ErrorKind::PermissionDenied => {
            Result::error(CODE_DENIED, format!("{op}: {path:?}: permission denied"))
        }
        _ => Result::error(CODE_IO_ERROR, format!("{op}: {path:?}: {err}")),
    }
}

fn sniff_binary(path: &Path) -> io::Result<(u64, bool)> {
    let size = fs::metadata(path)?.len();
    let mut file = File::open(path)?;
    let mut buffer = [0u8; BINARY_SNIFF_BYTES];
    let read = file.read(&mut buffer)?;
    Ok((size, buffer[..read].contains(&0)))
}

fn read_range(
    path: &Path,
    first: usize,
    last: usize,
    budget: usize,
) -> io::Result<(String, usize, bool, usize)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut out = String::new();
    let mut total = 0usize;
    let mut cut = false;
    let mut cut_at = 0usize;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        total += 1;
        if total < first || total > last {
            continue;
        }
        let room = budget.saturating_sub(out.len());
        if room == 0 {
            if !cut {
                cut = true;
                cut_at = total;
            }
            continue;
        }
        if line.len() > room {
            let mut size = room;
            while size > 0 && std::str::from_utf8(&line[..size]).is_err() {
                size -= 1;
            }
            out.push_str(std::str::from_utf8(&line[..size]).unwrap_or(""));
            if !cut {
                cut = true;
                cut_at = total;
            }
            continue;
        }
        out.push_str(&String::from_utf8_lossy(&line));
    }
    Ok((out, total, cut, cut_at))
}

fn read_text_file(path: &Path, _size: u64) -> Option<String> {
    let raw = fs::read(path).ok()?;
    if raw.iter().take(BINARY_SNIFF_BYTES).any(|byte| *byte == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&raw).into_owned())
}

fn edit_not_found(rel: &str, content: &str, old: &str) -> Result {
    if content.contains("\r\n") {
        let normalized = content.replace("\r\n", "\n");
        let old_normalized = old.replace("\r\n", "\n");
        let (lines, total) = occurrences(&normalized, &old_normalized, 20);
        if total > 0 {
            return Result::error(
                CODE_NOT_FOUND,
                format!("fs_edit: old text not found byte-for-byte in {rel}: the file uses CRLF (\\r\\n) line endings and old does not match them. With CRLF normalised the text appears at lines {}. Re-issue the edit with \\r\\n line endings in old, or rewrite the file with fs_write.", format_lines(&lines, total)),
            );
        }
    }
    let (lines, total) = trimmed_occurrences(content, old, 20);
    if total > 0 {
        return Result::error(
            CODE_NOT_FOUND,
            format!("fs_edit: old text not found byte-for-byte in {rel}, but a whitespace-insensitive match was found at lines {} — check indentation (spaces vs tabs) and trailing whitespace. Re-read the file with fs_read and copy the exact bytes.", format_lines(&lines, total)),
        );
    }
    Result::error(CODE_NOT_FOUND, format!("fs_edit: old text not found in {rel}. Read the file with fs_read first and copy the exact bytes (including indentation and line endings), or use fs_write to replace the whole file."))
}

fn occurrences(content: &str, needle: &str, max: usize) -> (Vec<usize>, usize) {
    if needle.is_empty() {
        return (Vec::new(), 0);
    }
    let mut lines = Vec::new();
    let mut total = 0usize;
    for (index, _) in content.match_indices(needle) {
        total += 1;
        if lines.len() < max {
            lines.push(1 + content[..index].matches('\n').count());
        }
    }
    (lines, total)
}

fn trimmed_occurrences(content: &str, old: &str, max: usize) -> (Vec<usize>, usize) {
    let normalized_file = content.replace("\r\n", "\n");
    let file_lines: Vec<&str> = normalized_file.split('\n').collect();
    let normalized_old = old.replace("\r\n", "\n");
    let old_lines: Vec<&str> = normalized_old.split('\n').collect();
    let old_lines: Vec<&str> = old_lines
        .into_iter()
        .take_while(|line| !line.trim().is_empty())
        .collect();
    if old_lines.is_empty() {
        return (Vec::new(), 0);
    }
    let mut starts = Vec::new();
    let mut total = 0usize;
    for start in 0..=file_lines.len().saturating_sub(old_lines.len()) {
        let matched = old_lines
            .iter()
            .enumerate()
            .all(|(index, old_line)| file_lines[start + index].trim() == old_line.trim());
        if matched {
            total += 1;
            if starts.len() < max {
                starts.push(start + 1);
            }
        }
    }
    (starts, total)
}

fn format_lines(lines: &[usize], total: usize) -> String {
    let mut text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if total > lines.len() {
        text.push_str(&format!(" (and {} more)", total - lines.len()));
    }
    text
}

fn normalize_pattern(pattern: &str) -> String {
    let mut text = pattern.trim().replace('\\', "/");
    while let Some(rest) = text.strip_prefix("./") {
        text = rest.to_string();
    }
    while let Some(rest) = text.strip_prefix('/') {
        text = rest.to_string();
    }
    text
}

fn validate_glob(pattern: &str) -> std::result::Result<(), String> {
    for segment in pattern.split('/') {
        if segment.is_empty() || segment == "**" {
            continue;
        }
        let mut chars = segment.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' if chars.peek().is_none() => return Err("syntax error in pattern".into()),
                '[' => {
                    let mut closed = false;
                    for inner in chars.by_ref() {
                        if inner == ']' {
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return Err("syntax error in pattern".into());
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn glob_match(pattern: &str, relative: &str) -> bool {
    if pattern.is_empty() || relative.is_empty() {
        return false;
    }
    if !pattern.contains('/') {
        let base = relative.rsplit('/').next().unwrap_or(relative);
        return match_segment(pattern, base);
    }
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let name_parts: Vec<&str> = relative.split('/').collect();
    match_segments(&pattern_parts, &name_parts)
}

fn match_segments(pattern: &[&str], name: &[&str]) -> bool {
    if pattern.is_empty() {
        return name.is_empty();
    }
    if pattern[0] == "**" {
        return (0..=name.len()).any(|index| match_segments(&pattern[1..], &name[index..]));
    }
    if name.is_empty() {
        return false;
    }
    match_segment(pattern[0], name[0]) && match_segments(&pattern[1..], &name[1..])
}

fn match_segment(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    match_chars(&pattern, &name)
}

fn match_chars(pattern: &[char], name: &[char]) -> bool {
    if pattern.is_empty() {
        return name.is_empty();
    }
    match pattern[0] {
        '*' => (0..=name.len()).any(|index| match_chars(&pattern[1..], &name[index..])),
        '?' => !name.is_empty() && match_chars(&pattern[1..], &name[1..]),
        '[' => {
            if name.is_empty() {
                return false;
            }
            let end = match pattern.iter().position(|ch| *ch == ']') {
                Some(end) if end > 1 => end,
                _ => return false,
            };
            let class = &pattern[1..end];
            let negated = class.first() == Some(&'!') || class.first() == Some(&'^');
            let body = if negated { &class[1..] } else { class };
            let matched = class_matches(body, name[0]);
            matched != negated && match_chars(&pattern[end + 1..], &name[1..])
        }
        '\\' => {
            pattern.len() >= 2
                && !name.is_empty()
                && pattern[1] == name[0]
                && match_chars(&pattern[2..], &name[1..])
        }
        literal => !name.is_empty() && literal == name[0] && match_chars(&pattern[1..], &name[1..]),
    }
}

fn class_matches(class: &[char], ch: char) -> bool {
    let mut index = 0;
    while index < class.len() {
        if index + 2 < class.len() && class[index + 1] == '-' {
            if class[index] <= ch && ch <= class[index + 2] {
                return true;
            }
            index += 3;
        } else {
            if class[index] == ch {
                return true;
            }
            index += 1;
        }
    }
    false
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_files(&path, out)?;
        } else if kind.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_supports_anchors_and_quantifiers() {
        let re = Regex::new("^foo.*bar$").expect("valid regex");
        assert!(re.is_match("foo xyz bar"));
        assert!(!re.is_match("xfoo bar"));

        let re = Regex::new("colou?r").expect("valid regex");
        assert!(re.is_match("color"));
        assert!(re.is_match("colour"));
    }

    #[test]
    fn regex_supports_classes() {
        let re = Regex::new(r"^[a-z]+_[0-9]+$").expect("valid regex");
        assert!(re.is_match("alpha_123"));
        assert!(!re.is_match("Alpha_123"));
    }

    #[test]
    fn regex_supports_alternation() {
        let re = Regex::new("foo|bar").expect("valid regex");
        assert!(re.is_match("a foo here"));
        assert!(re.is_match("a bar here"));
        assert!(!re.is_match("baz"));
    }

    #[test]
    fn regex_supports_case_insensitive_flag() {
        let re = Regex::new("(?i)secret").expect("valid regex");
        assert!(re.is_match("SECRET here"));
    }

    #[test]
    fn regex_rejects_lookahead_with_re2_error_text() {
        let opener = "(";
        let marker = "?=";
        let pattern = format!("foo{opener}{marker}bar)");
        let err = Regex::new(&pattern).expect_err("lookahead is not RE2");
        let text = err.to_string();
        assert!(
            text.contains("look-around") || text.contains("lookahead"),
            "{text}"
        );
    }

    #[test]
    fn fs_grep_reports_invalid_pattern_with_re2_note() {
        let call = Call {
            id: "1".into(),
            name: "fs_grep".into(),
            args: json!({"pattern": "foo(?=bar)"}),
            workspace: ".".into(),
        };
        let result = FsGrep.call(call);
        assert_eq!(result.code, ERR_BAD_ARGS);
        assert!(result.output.contains("invalid regexp"));
        assert!(result
            .output
            .contains("lookahead/lookbehind and backreferences are not supported"));
    }
}
