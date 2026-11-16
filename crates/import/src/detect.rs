//! Read-only discovery of one old-format session log.
//!
//! Nothing here creates, modifies, or touches a file. The layout facts are
//! taken from the retired project's `sessionFormatLogFilename` and the JSONL
//! recorder: `<root>/<project>/<session>/session[.vN].jsonl`, with `.zstd` as
//! the at-rest compression suffix.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use forge_core::{Error, ErrorKind, Result};
use serde_json::Value;

use crate::error::{
    err, with_location, ERR_BAD_HEADER, ERR_BAD_LOG, ERR_COMPRESSED, ERR_NO_DIR, ERR_NO_SESSION,
    ERR_UNSUPPORTED_VERSION,
};
use crate::jsonx::{parse_object, raw_bool, raw_i64, raw_string};

/// Newest old generation this converter understands (catalog currentVersion 4).
pub(crate) const OLD_FORMAT_MAX: i64 = 4;

/// What `Detect` reports about the selected old session log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    /// The directory the caller passed to `Detect`, unchanged.
    pub dir: PathBuf,
    /// Selected generation, e.g. `"forge-session-v4"`.
    pub format: String,
    /// Number of old session directories found under `dir`.
    pub sessions: usize,
    /// Number of event rows in the selected log, header excluded.
    pub events: usize,
    /// Non-fatal findings: extra sessions/generations, compressed siblings,
    /// torn tails, lineage that v1 cannot express.
    pub warnings: Vec<String>,
}

/// Parsed first row of an old log.
#[derive(Debug, Clone)]
pub(crate) struct OldHeader {
    pub version: i64,
    pub id: String,
    pub created_at: i64,
    pub cwd: String,
    pub profile: String,
    // Parsed for forward compatibility with the old envelope, but no v1 event
    // carries them; Go's compiler ignores these too. Kept so the header mirror
    // stays complete for future mapping work.
    #[allow(dead_code)]
    pub origin: String,
    pub parent: String,
    #[allow(dead_code)]
    pub seeded: bool,
    #[allow(dead_code)]
    pub depth: i64,
}

/// One event row of an old log. `seq`/`time` are absent from the recorded
/// snapshot fixtures; real logs carry them.
#[derive(Debug, Clone)]
pub(crate) struct OldRecord {
    pub typ: String,
    pub data: Option<Value>,
    /// Present in real logs, absent from recorded snapshots. Kept in the
    /// mirror; the mapping deliberately derives event ids from content, not
    /// from the old seq.
    #[allow(dead_code)]
    pub seq: i64,
    pub time: i64,
    /// 1-based physical line, used in drop reasons and errors.
    pub line: usize,
}

/// The source plus the parsed rows `MakePlan` and `Write` need.
pub(crate) struct Located {
    pub source: Source,
    pub header: OldHeader,
    pub records: Vec<OldRecord>,
}

/// Inspect `dir` and report the old session log found there. Read-only.
pub fn detect(dir: &Path) -> Result<Source> {
    Ok(locate(dir)?.source)
}

/// Contract alias for [`detect`], kept for callers that spell the API as in
/// `docs/contracts/m4-surface.md` §3.
#[allow(non_snake_case)]
pub fn Detect(dir: &Path) -> Result<Source> {
    detect(dir)
}

/// Resolve and read the log both the plan and the writer will use.
pub(crate) fn locate(dir: &Path) -> Result<Located> {
    let (candidates, mut warnings) = discover(dir)?;
    if candidates.is_empty() {
        return Err(err(
            ErrorKind::NotFound,
            ERR_NO_SESSION,
            format!(
                "{} holds no session.jsonl or session.v<N>.jsonl",
                dir.display()
            ),
        ));
    }
    let sessions = candidates.len();
    let chosen_index = select_session(&candidates, dir, &mut warnings)?;
    let chosen = &candidates[chosen_index];

    let (header, records, read_warnings) = read_log(&chosen.chosen.path)?;
    warnings.extend(read_warnings);

    let uncompressed: Vec<&LogFile> = chosen.logs.iter().filter(|lf| !lf.compressed).collect();
    if uncompressed.len() > 1 {
        warnings.push(format!(
            "session {} holds {} committed generations ({}); importing the newest, v{}",
            chosen.dir.display(),
            uncompressed.len(),
            join_versions(&chosen.logs),
            chosen.chosen.version
        ));
    }
    for log in &chosen.logs {
        if log.compressed {
            warnings.push(format!(
                "ignoring compressed log {}: zstd decoding needs a dependency this build does not carry",
                log.path.display()
            ));
        }
    }
    if !header.parent.is_empty() {
        warnings.push(format!(
            "old header names parent session {:?}; v1 has no parent member, the lineage is not imported",
            header.parent
        ));
    }

    Ok(Located {
        source: Source {
            dir: dir.to_path_buf(),
            format: format!("forge-session-v{}", header.version),
            sessions,
            events: records.len(),
            warnings,
        },
        header,
        records,
    })
}

#[derive(Clone)]
struct LogFile {
    path: PathBuf,
    version: i64,
    compressed: bool,
}

struct Candidate {
    dir: PathBuf,
    /// Sorted by version, then uncompressed before compressed.
    logs: Vec<LogFile>,
    /// Newest generation; a compressed newest makes the candidate unusable.
    chosen: LogFile,
    created_at: i64,
    error: Option<Error>,
}

fn discover(dir: &Path) -> Result<(Vec<Candidate>, Vec<String>)> {
    let meta = match fs::metadata(dir) {
        Ok(meta) => meta,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(err(ErrorKind::NotFound, ERR_NO_DIR, dir.display()))
        }
        Err(source) => {
            return Err(err(
                ErrorKind::NotFound,
                ERR_NO_DIR,
                format_args!("cannot read {}: {}", dir.display(), source),
            ))
        }
    };
    if !meta.is_dir() {
        return Err(err(
            ErrorKind::NotFound,
            ERR_NO_DIR,
            format_args!("{} is not a directory", dir.display()),
        ));
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    if !session_logs(dir)?.is_empty() {
        // A single session directory (the snapshot fixture layout).
        dirs.push(dir.to_path_buf());
    } else {
        // A data root: production writes <root>/<project>/<session>/.
        for child in read_dir_sorted(dir)? {
            if !child.is_dir() {
                continue;
            }
            if !session_logs(&child)?.is_empty() {
                dirs.push(child);
                continue;
            }
            for grandchild in read_dir_sorted(&child)? {
                if !grandchild.is_dir() {
                    continue;
                }
                if !session_logs(&grandchild)?.is_empty() {
                    dirs.push(grandchild);
                }
            }
        }
    }

    let mut candidates = Vec::new();
    for session_dir in dirs {
        let logs = session_logs(&session_dir)?;
        // session_logs never returns an empty list for a dir we discovered.
        let newest = logs
            .last()
            .cloned()
            .ok_or_else(|| err(ErrorKind::NotFound, ERR_NO_SESSION, session_dir.display()))?;
        let mut candidate = Candidate {
            dir: session_dir,
            logs,
            chosen: newest,
            created_at: 0,
            error: None,
        };
        if candidate.chosen.compressed {
            candidate.error = Some(err(
                ErrorKind::Invalid,
                ERR_COMPRESSED,
                candidate.chosen.path.display(),
            ));
            candidates.push(candidate);
            continue;
        }
        match read_log(&candidate.chosen.path) {
            Ok((header, _, _)) => candidate.created_at = header.created_at,
            Err(source) => candidate.error = Some(source),
        }
        candidates.push(candidate);
    }
    Ok((candidates, Vec::new()))
}

/// Newest `created_at`, lexicographically first path breaking a tie. Discovery
/// order is deterministic, so a strict comparison keeps the first of equals.
fn select_session(
    candidates: &[Candidate],
    dir: &Path,
    warnings: &mut Vec<String>,
) -> Result<usize> {
    let mut best: Option<usize> = None;
    let mut first_error: Option<&Error> = None;
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(error) = &candidate.error {
            if first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        if best.is_none_or(|previous| candidate.created_at > candidates[previous].created_at) {
            best = Some(index);
        }
    }
    let Some(index) = best else {
        return Err(match first_error {
            Some(source) => Error::new(source.kind(), source.message().to_owned()),
            None => err(ErrorKind::NotFound, ERR_NO_SESSION, dir.display()),
        });
    };
    if candidates.len() > 1 {
        warnings.push(format!(
            "found {} old session directories under {}; importing {} (newest header.createdAt {})",
            candidates.len(),
            dir.display(),
            candidates[index].dir.display(),
            candidates[index].created_at
        ));
    }
    Ok(index)
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).map_err(|source| {
        err(
            ErrorKind::Io,
            "cannot read directory",
            format_args!("{}: {}", dir.display(), source),
        )
    })? {
        let entry = entry.map_err(|source| {
            err(
                ErrorKind::Io,
                "cannot read directory entry",
                format_args!("{}: {}", dir.display(), source),
            )
        })?;
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

fn session_logs(dir: &Path) -> Result<Vec<LogFile>> {
    let mut logs = Vec::new();
    for path in read_dir_sorted(dir)? {
        if path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some((version, compressed)) = parse_log_filename(name) {
            logs.push(LogFile {
                path,
                version,
                compressed,
            });
        }
    }
    logs.sort_by(|left, right| {
        left.version
            .cmp(&right.version)
            .then(left.compressed.cmp(&right.compressed))
    });
    Ok(logs)
}

/// Parse `session.jsonl`, `session.vN.jsonl` and their `.zstd` variants.
/// `v0`, leading zeros, temp files and uppercase names are not canonical and
/// are ignored exactly as the old parser ignores them.
pub(crate) fn parse_log_filename(name: &str) -> Option<(i64, bool)> {
    let rest = name.strip_prefix("session")?;
    let (version, rest) = match rest.strip_prefix(".v") {
        Some(after_v) => {
            let end = after_v.find(".jsonl")?;
            let digits = &after_v[..end];
            if digits.is_empty()
                || !digits.as_bytes()[0].is_ascii_digit()
                || digits.as_bytes()[0] == b'0'
                || !digits.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            (digits.parse::<i64>().ok()?, &after_v[end..])
        }
        None => (0, rest),
    };
    let rest = rest.strip_prefix(".jsonl")?;
    let compressed = if rest.is_empty() {
        false
    } else if rest == ".zstd" {
        true
    } else {
        return None;
    };
    Some((version, compressed))
}

fn join_versions(logs: &[LogFile]) -> String {
    logs.iter()
        .filter(|log| !log.compressed)
        .map(|log| format!("v{}", log.version))
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse the header row and every event row. Never writes.
fn read_log(path: &Path) -> Result<(OldHeader, Vec<OldRecord>, Vec<String>)> {
    let raw = fs::read(path).map_err(|source| {
        Error::new(
            ErrorKind::Io,
            format!("read {}: {}", path.display(), source),
        )
    })?;
    let raw = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&raw);
    let text = std::str::from_utf8(raw).map_err(|source| {
        err(
            ErrorKind::Invalid,
            ERR_BAD_LOG,
            format_args!("{} is not UTF-8: {}", path.display(), source),
        )
    })?;

    let mut warnings = Vec::new();
    let mut lines: Vec<&str> = text.split('\n').collect();
    if let Some(last) = lines.last() {
        if !last.is_empty() {
            // The old scanner treats a final row without a newline as torn and
            // recovers the prefix; the importer must do the same.
            warnings.push(format!(
                "{}: ignored a torn final row (no trailing newline) at line {}",
                path.display(),
                lines.len()
            ));
            lines.pop();
        }
    }

    let mut header: Option<OldHeader> = None;
    let mut records = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let line_no = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        if header.is_none() {
            header =
                Some(parse_header(line).map_err(|source| with_location(source, path, line_no))?);
        } else {
            records.push(
                parse_record(line, line_no)
                    .map_err(|source| with_location(source, path, line_no))?,
            );
        }
    }
    let header = header.ok_or_else(|| {
        err(
            ErrorKind::Invalid,
            ERR_BAD_HEADER,
            format_args!("{} is empty", path.display()),
        )
    })?;
    Ok((header, records, warnings))
}

fn parse_header(line: &str) -> Result<OldHeader> {
    let obj = parse_object(line).ok_or_else(|| {
        err(
            ErrorKind::Invalid,
            ERR_BAD_HEADER,
            "header row is not a JSON object",
        )
    })?;
    let typ = raw_string(&obj, "type").unwrap_or_default();
    if typ != "session" {
        return Err(err(
            ErrorKind::Invalid,
            ERR_BAD_HEADER,
            format_args!("first row has type {typ:?}, want \"session\""),
        ));
    }
    let version = raw_i64(&obj, "version").ok_or_else(|| {
        err(
            ErrorKind::Invalid,
            ERR_BAD_HEADER,
            "header has no numeric version",
        )
    })?;
    if version < 0 {
        return Err(err(
            ErrorKind::Invalid,
            ERR_BAD_HEADER,
            format_args!("header version {version} is negative"),
        ));
    }
    if version > OLD_FORMAT_MAX {
        return Err(err(
            ErrorKind::UnsupportedVersion,
            ERR_UNSUPPORTED_VERSION,
            format_args!("v{version} is newer than this build speaks (v0..v{OLD_FORMAT_MAX})"),
        ));
    }
    let id = raw_string(&obj, "id").unwrap_or_default();
    if id.is_empty() {
        return Err(err(
            ErrorKind::Invalid,
            ERR_BAD_HEADER,
            "header has no string id",
        ));
    }
    let created_at = raw_i64(&obj, "createdAt").unwrap_or(-1);
    if created_at < 0 {
        return Err(err(
            ErrorKind::Invalid,
            ERR_BAD_HEADER,
            "header has no non-negative createdAt",
        ));
    }
    Ok(OldHeader {
        version,
        id,
        created_at,
        cwd: raw_string(&obj, "cwd").unwrap_or_default(),
        profile: raw_string(&obj, "agentPreset").unwrap_or_default(),
        origin: raw_string(&obj, "origin").unwrap_or_default(),
        parent: raw_string(&obj, "parentSession").unwrap_or_default(),
        seeded: raw_bool(&obj, "isSeeded").unwrap_or(false),
        depth: raw_i64(&obj, "delegationDepth").unwrap_or(0),
    })
}

fn parse_record(line: &str, line_no: usize) -> Result<OldRecord> {
    let obj = parse_object(line)
        .ok_or_else(|| err(ErrorKind::Invalid, ERR_BAD_LOG, "row is not a JSON object"))?;
    let typ = raw_string(&obj, "type").unwrap_or_default();
    if typ.is_empty() {
        return Err(err(
            ErrorKind::Invalid,
            ERR_BAD_LOG,
            format_args!("row has no string type at line {line_no}"),
        ));
    }
    let seq = raw_i64(&obj, "seq").filter(|value| *value > 0).unwrap_or(0);
    let time = raw_i64(&obj, "time")
        .filter(|value| *value > 0)
        .unwrap_or(0);
    Ok(OldRecord {
        typ,
        data: obj.get("data").cloned(),
        seq,
        time,
        line: line_no,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_log_filename;

    #[test]
    fn parse_log_filename_table() {
        let cases = [
            ("session.jsonl", Some((0, false))),
            ("session.v4.jsonl", Some((4, false))),
            ("session.v12.jsonl", Some((12, false))),
            ("session.v4.jsonl.zstd", Some((4, true))),
            ("session.v0.jsonl", None),
            ("session.v04.jsonl", None),
            ("session.v4.jsonl.tmp", None),
            ("SESSION.V4.JSONL", None),
            ("session-1.jsonl", None),
            ("other.jsonl", None),
        ];
        for (name, want) in cases {
            assert_eq!(parse_log_filename(name), want, "filename {name}");
        }
    }
}
