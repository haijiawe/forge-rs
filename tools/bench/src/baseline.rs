//! The language-neutral baseline document: parse, select, upsert, write.
//!
//! Shape (Go oracle `scripts/bench/baseline.go`, with `goVersion` renamed to
//! `rustVersion`):
//!
//! ```json
//! {"version":1,"baselines":[{"os":"windows","arch":"amd64",
//!   "rustVersion":"rustc 1.85.0","timestamp":"2026-01-01T00:00:00Z",
//!   "results":[{"name":"version_startup","unit":"ms","value":1.2,
//!     "samples":20,"p50":1.2,"p95":1.4,"tolerance":0.10}]}]}
//! ```
//!
//! A file is per-platform: `select_baseline` returns the newest record for
//! one `os`/`arch`, and `upsert_baseline` replaces that record in place while
//! dropping later duplicates. Records for other platforms are never compared.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::time;

pub const BASELINE_VERSION: u32 = 1;

/// One measured metric. Field order is serialization order and is part of the
/// document contract (stable keys, readable diffs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricRow {
    pub name: String,
    pub unit: String,
    pub value: f64,
    pub samples: usize,
    pub p50: f64,
    pub p95: f64,
    pub tolerance: f64,
}

/// One platform's run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub os: String,
    pub arch: String,
    #[serde(rename = "rustVersion")]
    pub rust_version: String,
    pub timestamp: String,
    pub results: Vec<MetricRow>,
}

/// The whole document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineSet {
    pub version: u32,
    pub baselines: Vec<Report>,
}

impl BaselineSet {
    pub fn empty() -> BaselineSet {
        BaselineSet {
            version: BASELINE_VERSION,
            baselines: Vec::new(),
        }
    }
}

/// Validate the structural rules the Go oracle enforces.
pub fn validate_baseline(set: &BaselineSet) -> Result<(), String> {
    if set.version != BASELINE_VERSION {
        return Err(format!(
            "baseline: unsupported version {} (want {})",
            set.version, BASELINE_VERSION
        ));
    }
    if set.baselines.is_empty() {
        return Err("baseline: document has no records".to_string());
    }
    for (i, record) in set.baselines.iter().enumerate() {
        if record.os.trim().is_empty() {
            return Err(format!("baseline: record {i} has no os"));
        }
        if record.arch.trim().is_empty() {
            return Err(format!("baseline: record {i} has no arch"));
        }
        if record.results.is_empty() {
            return Err(format!("baseline: record {i} has no results"));
        }
    }
    Ok(())
}

/// Parse either the current set shape or the legacy single-report shape.
pub fn parse_baseline(text: &str) -> Result<BaselineSet, String> {
    match serde_json::from_str::<BaselineSet>(text) {
        Ok(set) => {
            validate_baseline(&set)?;
            Ok(set)
        }
        Err(_) => {
            let report: Report = serde_json::from_str(text)
                .map_err(|e| format!("baseline: not a set or a report: {e}"))?;
            let set = BaselineSet {
                version: BASELINE_VERSION,
                baselines: vec![report],
            };
            validate_baseline(&set)?;
            Ok(set)
        }
    }
}

/// `""` and a missing file both mean "no baseline"; an unreadable or
/// unparseable existing file is a hard error so a bad document is never
/// silently overwritten.
pub fn load_baseline_file(path: &str) -> Result<Option<BaselineSet>, String> {
    if path.is_empty() {
        return Ok(None);
    }
    let path_obj = Path::new(path);
    if !path_obj.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path_obj).map_err(|e| format!("baseline: cannot read {path}: {e}"))?;
    Ok(Some(parse_baseline(&text)?))
}

/// Newest record for exactly this platform, or `None`.
pub fn select_baseline(set: &BaselineSet, os: &str, arch: &str) -> Option<Report> {
    let mut best: Option<&Report> = None;
    for record in &set.baselines {
        if record.os == os
            && record.arch == arch
            && best
                .is_none_or(|current| time::timestamp_after(&record.timestamp, &current.timestamp))
        {
            best = Some(record);
        }
    }
    best.cloned()
}

/// Replace the first matching platform record in place, drop later duplicates,
/// and append when the platform was not present.
pub fn upsert_baseline(set: &BaselineSet, report: &Report) -> BaselineSet {
    let mut out = BaselineSet {
        version: BASELINE_VERSION,
        baselines: Vec::with_capacity(set.baselines.len() + 1),
    };
    let mut replaced = false;
    for record in &set.baselines {
        if record.os == report.os && record.arch == report.arch {
            if !replaced {
                out.baselines.push(report.clone());
                replaced = true;
            }
            // Later records for the same platform are duplicates: drop them.
        } else {
            out.baselines.push(record.clone());
        }
    }
    if !replaced {
        out.baselines.push(report.clone());
    }
    out
}

/// Two-space indent, LF only, exactly one trailing newline, stable field order.
pub fn marshal_baseline(set: &BaselineSet) -> Result<String, String> {
    validate_baseline(set)?;
    let mut text = serde_json::to_string_pretty(set)
        .map_err(|e| format!("baseline: cannot marshal document: {e}"))?;
    text.push('\n');
    Ok(text)
}

/// Merge `report` into the document at `path`, creating the file if needed.
/// Refuses to write when an existing document cannot be parsed.
pub fn write_baseline(path: &str, report: &Report) -> Result<(), String> {
    if path.is_empty() {
        return Err("baseline: -out is empty".to_string());
    }
    let existing = load_baseline_file(path)?;
    let base = existing.unwrap_or_else(BaselineSet::empty);
    let merged = upsert_baseline(&base, report);
    let text = marshal_baseline(&merged)?;
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("baseline: cannot create {}: {e}", parent.display()))?;
        }
    }
    fs::write(path, text).map_err(|e| format!("baseline: cannot write {path}: {e}"))
}

/// `os/arch, os/arch` for honest "this document records" messages.
pub fn platforms_line(set: &BaselineSet) -> String {
    let mut names: Vec<String> = set
        .baselines
        .iter()
        .map(|r| format!("{}/{}", r.os, r.arch))
        .collect();
    names.sort();
    names.dedup();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn row(name: &str, unit: &str, value: f64, tolerance: f64) -> MetricRow {
        MetricRow {
            name: name.to_string(),
            unit: unit.to_string(),
            value,
            samples: 20,
            p50: value,
            p95: value * 1.1,
            tolerance,
        }
    }

    fn report(os: &str, arch: &str, rust: &str, timestamp: &str) -> Report {
        Report {
            os: os.to_string(),
            arch: arch.to_string(),
            rust_version: rust.to_string(),
            timestamp: timestamp.to_string(),
            results: vec![
                row("version_startup", "ms", 1.2345, 0.10),
                row("session_append_fsync", "events/sec", 2345.6, 0.35),
            ],
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("forge-bench-base-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_current_set_shape_and_preserves_fields() {
        let original = BaselineSet {
            version: 1,
            baselines: vec![report(
                "windows",
                "amd64",
                "rustc 1.85.0",
                "2026-01-01T00:00:00Z",
            )],
        };
        let text = marshal_baseline(&original).unwrap();
        let parsed = parse_baseline(&text).unwrap();
        assert_eq!(parsed, original);
        assert_eq!(parsed.baselines[0].rust_version, "rustc 1.85.0");
        assert!(parsed.baselines[0].results[0].p95 > parsed.baselines[0].results[0].p50);
    }

    #[test]
    fn parses_legacy_single_report_into_one_record_set() {
        let legacy = serde_json::to_string(&report(
            "linux",
            "amd64",
            "rustc 1.84.0",
            "2025-01-01T00:00:00Z",
        ))
        .unwrap();
        let parsed = parse_baseline(&legacy).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.baselines.len(), 1);
        assert_eq!(parsed.baselines[0].os, "linux");
    }

    #[test]
    fn rejects_unknown_version_and_unusable_documents() {
        let bad_version = r#"{"version":2,"baselines":[{"os":"linux","arch":"amd64","rustVersion":"x","timestamp":"t","results":[{"name":"a","unit":"ms","value":1,"samples":1,"p50":1,"p95":1,"tolerance":0.1}]}]}"#;
        assert!(parse_baseline(bad_version).unwrap_err().contains("version"));

        let empty = r#"{"version":1,"baselines":[]}"#;
        assert!(parse_baseline(empty).is_err());

        let no_arch = r#"{"version":1,"baselines":[{"os":"linux","arch":"","rustVersion":"x","timestamp":"t","results":[{"name":"a","unit":"ms","value":1,"samples":1,"p50":1,"p95":1,"tolerance":0.1}]}]}"#;
        assert!(parse_baseline(no_arch).unwrap_err().contains("arch"));

        let no_results = r#"{"version":1,"baselines":[{"os":"linux","arch":"amd64","rustVersion":"x","timestamp":"t","results":[]}]}"#;
        assert!(parse_baseline(no_results).unwrap_err().contains("results"));

        assert!(parse_baseline("[]").is_err());
        assert!(parse_baseline("{ broken").is_err());
    }

    #[test]
    fn marshal_format_is_stable_two_space_lf_with_one_trailing_newline() {
        let set = BaselineSet {
            version: 1,
            baselines: vec![report("windows", "amd64", "rustc", "2026-01-01T00:00:00Z")],
        };
        let text = marshal_baseline(&set).unwrap();
        assert!(!text.contains('\r'), "CR found");
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        assert!(text.starts_with("{\n  \"version\": 1,\n  \"baselines\": ["));
        let v = text.find("\"version\"").unwrap();
        let b = text.find("\"baselines\"").unwrap();
        let os = text.find("\"os\"").unwrap();
        let name = text.find("\"name\"").unwrap();
        assert!(v < b && b < os && os < name);
        assert_eq!(parse_baseline(&text).unwrap(), set);
    }

    #[test]
    fn select_exact_newest_and_no_match() {
        let set = BaselineSet {
            version: 1,
            baselines: vec![
                report("windows", "amd64", "old", "2025-01-01T00:00:00Z"),
                report("linux", "amd64", "other", "2030-01-01T00:00:00Z"),
                report("windows", "amd64", "new", "2026-01-01T00:00:00+08:00"),
            ],
        };
        let picked = select_baseline(&set, "windows", "amd64").unwrap();
        assert_eq!(picked.rust_version, "new");
        assert!(select_baseline(&set, "darwin", "arm64").is_none());
    }

    #[test]
    fn select_keeps_first_on_timestamp_tie_and_parseable_beats_garbage() {
        let mut first = report("linux", "amd64", "first", "2026-01-01T08:00:00+08:00");
        first.rust_version = "first".to_string();
        let mut second = report("linux", "amd64", "second", "2026-01-01T00:00:00Z");
        second.rust_version = "second".to_string();
        let set = BaselineSet {
            version: 1,
            baselines: vec![first, second],
        };
        assert_eq!(
            select_baseline(&set, "linux", "amd64")
                .unwrap()
                .rust_version,
            "first"
        );

        let mut good = report("linux", "amd64", "good", "2025-01-01T00:00:00Z");
        good.rust_version = "good".to_string();
        let mut bad = report("linux", "amd64", "bad", "not-a-time");
        bad.rust_version = "bad".to_string();
        let set = BaselineSet {
            version: 1,
            baselines: vec![bad, good],
        };
        assert_eq!(
            select_baseline(&set, "linux", "amd64")
                .unwrap()
                .rust_version,
            "good"
        );
    }

    #[test]
    fn upsert_replaces_in_place_appends_and_dedupes() {
        let mut other = report("linux", "amd64", "other", "2026-01-01T00:00:00Z");
        other.rust_version = "other".to_string();
        let mut old = report("windows", "amd64", "old", "2025-01-01T00:00:00Z");
        old.rust_version = "old".to_string();
        let mut dup = report("windows", "amd64", "dup", "2024-01-01T00:00:00Z");
        dup.rust_version = "dup".to_string();
        let set = BaselineSet {
            version: 1,
            baselines: vec![other.clone(), old, dup],
        };
        let fresh = report("windows", "amd64", "fresh", "2026-06-01T00:00:00Z");
        let merged = upsert_baseline(&set, &fresh);
        assert_eq!(merged.baselines.len(), 2);
        assert_eq!(merged.baselines[0], other);
        assert_eq!(merged.baselines[1], fresh);
        assert_eq!(merged.baselines[1].rust_version, "fresh");

        let appended = upsert_baseline(
            &BaselineSet {
                version: 1,
                baselines: vec![other.clone()],
            },
            &fresh,
        );
        assert_eq!(appended.baselines.len(), 2);
        assert_eq!(appended.baselines[1], fresh);
    }

    #[test]
    fn load_missing_or_empty_path_returns_none() {
        assert!(load_baseline_file("").unwrap().is_none());
        let dir = temp_dir("missing");
        let path = dir.join("nope.json");
        assert!(load_baseline_file(path.to_str().unwrap())
            .unwrap()
            .is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_baseline_merges_platforms_and_refuses_garbage() {
        let dir = temp_dir("write");
        let path = dir.join("nested").join("baseline.json");
        let p = path.to_str().unwrap();
        let linux = report("linux", "amd64", "rustc", "2026-01-01T00:00:00Z");
        write_baseline(p, &linux).unwrap();
        let windows = report("windows", "amd64", "rustc", "2026-01-02T00:00:00Z");
        write_baseline(p, &windows).unwrap();
        let set = parse_baseline(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(set.baselines.len(), 2);
        assert_eq!(select_baseline(&set, "linux", "amd64").unwrap(), linux);
        assert_eq!(select_baseline(&set, "windows", "amd64").unwrap(), windows);
        assert_eq!(platforms_line(&set), "linux/amd64, windows/amd64");

        let updated = report("linux", "amd64", "rustc-newer", "2026-02-01T00:00:00Z");
        write_baseline(p, &updated).unwrap();
        let set = parse_baseline(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(set.baselines.len(), 2);
        assert_eq!(
            select_baseline(&set, "linux", "amd64")
                .unwrap()
                .rust_version,
            "rustc-newer"
        );

        fs::write(&path, "{ not json").unwrap();
        let err = write_baseline(p, &windows).unwrap_err();
        assert!(err.contains("baseline:"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
        fs::remove_dir_all(dir).unwrap();
    }
}
