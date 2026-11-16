//! Gate decisions: compare a run with the baseline for the SAME platform.
//!
//! Policy, inherited from the Go oracle:
//! * a missing baseline document or a missing platform record is an honest
//!   SKIP — it exits 0 even under `-strict`;
//! * a regression in report mode exits 0 and is printed;
//! * only `-strict` turns regressions into exit 1;
//! * `-require-baseline` turns a missing document/record into exit 1;
//! * new and removed metrics never fail the gate;
//! * tolerances sit next to values (0.35 for the fsync pair, 0.10 elsewhere),
//!   and `-fail-over` is only a fallback when a row has no tolerance.

use crate::baseline::{self, BaselineSet, MetricRow, Report};
use crate::stats::{fmt_num, higher_is_better};

pub const TOLERANCE_IO: f64 = 0.35;
pub const TOLERANCE_CPU: f64 = 0.10;

pub fn default_tolerance(name: &str) -> f64 {
    if name == "session_append_fsync" || name == "session_append_batch" {
        TOLERANCE_IO
    } else {
        TOLERANCE_CPU
    }
}

pub const STRICT_SKIP_NOTE: &str = "bench: -strict had no record for this platform, so nothing was gated and the run exits 0 by design; pass -require-baseline to make this an error.";

pub fn record_command() -> String {
    "bench: record one with: cargo run -q -p forge-bench --offline -- -out scripts/bench/baseline.json -baseline \"\""
        .to_string()
}

pub fn missing_baseline_line(os: &str, arch: &str) -> String {
    format!(
        "bench: no baseline for {os}/{arch} — gate skipped; {}",
        record_command()
    )
}

pub fn rust_version_note(recorded: &str, running: &str) -> String {
    format!("bench: baseline recorded rustVersion={recorded}, running rustVersion={running}; gate still applies.")
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompareResult {
    pub verdict: &'static str,
    pub worse_pct: f64,
    pub pct: f64,
    pub detail: String,
}

/// Compare one current row with its baseline row.
pub fn compare(current: &MetricRow, base: &MetricRow, fail_over_pct: f64) -> CompareResult {
    if base.value == 0.0 {
        return CompareResult {
            verdict: "SKIP",
            worse_pct: 0.0,
            pct: 0.0,
            detail: format!("{}: baseline value is 0", current.name),
        };
    }
    let tolerance = if base.tolerance == 0.0 {
        fail_over_pct / 100.0
    } else {
        base.tolerance
    };
    let pct = (current.value - base.value) / base.value * 100.0;
    let worse_pct = if higher_is_better(&current.unit) {
        -pct
    } else {
        pct
    };
    let verdict = if worse_pct > tolerance * 100.0 {
        "REGRESSED"
    } else if worse_pct < 0.0 {
        "ok (better)"
    } else {
        "ok"
    };
    CompareResult {
        verdict,
        worse_pct,
        pct,
        detail: format!(
            "{}: {} {} -> {} {} ({:+.2}%, worse {:+.2}%, tol {:.0}%)",
            current.name,
            fmt_num(base.value),
            base.unit,
            fmt_num(current.value),
            current.unit,
            pct,
            worse_pct,
            tolerance * 100.0
        ),
    }
}

#[derive(Debug, Clone)]
pub struct GatePlan {
    pub notes: Vec<String>,
    pub compare: bool,
    pub error: Option<String>,
}

impl GatePlan {
    fn skip() -> GatePlan {
        GatePlan {
            notes: Vec::new(),
            compare: false,
            error: None,
        }
    }
}

/// Decide whether and how this run is gated.
pub fn plan_gate(
    set: Option<&BaselineSet>,
    report: &Report,
    baseline_label: &str,
    fail_over_pct: f64,
    strict: bool,
    require_baseline: bool,
) -> GatePlan {
    if baseline_label.is_empty() {
        let mut plan = GatePlan::skip();
        plan.notes
            .push("bench: -baseline is empty; recording this run without comparison.".to_string());
        return plan;
    }

    let Some(set) = set else {
        let mut plan = GatePlan::skip();
        plan.notes.push(format!(
            "No baseline file at {baseline_label}; nothing to compare."
        ));
        plan.notes.push(record_command());
        if require_baseline {
            plan.error = Some(format!(
                "bench: -require-baseline: no baseline file at {baseline_label}"
            ));
        } else if strict {
            plan.notes.push(STRICT_SKIP_NOTE.to_string());
        }
        return plan;
    };

    let Some(base) = baseline::select_baseline(set, &report.os, &report.arch) else {
        let mut plan = GatePlan::skip();
        plan.notes
            .push(missing_baseline_line(&report.os, &report.arch));
        plan.notes.push(format!(
            "bench: this document records: {}",
            baseline::platforms_line(set)
        ));
        if require_baseline {
            plan.error = Some(format!(
                "bench: -require-baseline: no baseline for {}/{}",
                report.os, report.arch
            ));
        } else if strict {
            plan.notes.push(STRICT_SKIP_NOTE.to_string());
        }
        return plan;
    };

    let mut plan = GatePlan {
        notes: Vec::new(),
        compare: true,
        error: None,
    };
    if base.rust_version != report.rust_version {
        plan.notes
            .push(rust_version_note(&base.rust_version, &report.rust_version));
    }

    let mut regressions = 0usize;
    for current in &report.results {
        match base.results.iter().find(|b| b.name == current.name) {
            Some(baseline_row) => {
                let result = compare(current, baseline_row, fail_over_pct);
                if result.verdict == "REGRESSED" {
                    regressions += 1;
                }
            }
            None => {
                plan.notes.push(format!(
                    "bench: {}: NEW metric (no baseline row; never fails gate)",
                    current.name
                ));
            }
        }
    }
    for baseline_row in &base.results {
        if !report.results.iter().any(|r| r.name == baseline_row.name) {
            plan.notes.push(format!(
                "bench: {}: REMOVED metric (no current row; never fails gate)",
                baseline_row.name
            ));
        }
    }

    if regressions > 0 && strict {
        plan.error = Some(format!(
            "{regressions} metric(s) regressed beyond their tolerance (-strict)"
        ));
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, unit: &str, value: f64, tolerance: f64) -> MetricRow {
        MetricRow {
            name: name.to_string(),
            unit: unit.to_string(),
            value,
            samples: 20,
            p50: value,
            p95: value,
            tolerance,
        }
    }

    fn report(os: &str, arch: &str, rust: &str, values: &[(&str, &str, f64, f64)]) -> Report {
        Report {
            os: os.to_string(),
            arch: arch.to_string(),
            rust_version: rust.to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            results: values
                .iter()
                .map(|(n, u, v, t)| row(n, u, *v, *t))
                .collect(),
        }
    }

    fn set(reports: Vec<Report>) -> BaselineSet {
        BaselineSet {
            version: 1,
            baselines: reports,
        }
    }

    #[test]
    fn default_tolerances_match_the_metric_table() {
        assert_eq!(default_tolerance("session_append_fsync"), 0.35);
        assert_eq!(default_tolerance("session_append_batch"), 0.35);
        for name in [
            "version_startup",
            "session_replay_10k",
            "session_projection_10k",
            "event_roundtrip",
        ] {
            assert_eq!(default_tolerance(name), 0.10);
        }
    }

    #[test]
    fn recording_mode_never_compares() {
        let base = set(vec![report(
            "windows",
            "amd64",
            "rustc",
            &[("version_startup", "ms", 1.0, 0.10)],
        )]);
        let current = report(
            "windows",
            "amd64",
            "rustc",
            &[("version_startup", "ms", 99.0, 0.10)],
        );
        let plan = plan_gate(Some(&base), &current, "", 10.0, true, true);
        assert!(!plan.compare);
        assert!(plan.error.is_none());
        assert!(plan.notes[0].contains("without comparison"));
    }

    #[test]
    fn missing_document_skips_even_under_strict_and_fails_only_on_require() {
        let current = report(
            "windows",
            "amd64",
            "rustc",
            &[("version_startup", "ms", 1.0, 0.10)],
        );
        let skipped = plan_gate(
            None,
            &current,
            "scripts/bench/baseline.json",
            10.0,
            true,
            false,
        );
        assert!(!skipped.compare);
        assert!(skipped.error.is_none());
        assert!(skipped
            .notes
            .iter()
            .any(|n| n.contains("No baseline file at")));
        assert!(skipped.notes.iter().any(|n| n.contains(STRICT_SKIP_NOTE)));

        let required = plan_gate(
            None,
            &current,
            "scripts/bench/baseline.json",
            10.0,
            false,
            true,
        );
        assert!(required.error.unwrap().contains("-require-baseline"));
    }

    #[test]
    fn missing_platform_record_skips_with_platform_list_and_record_command() {
        let base = set(vec![report(
            "linux",
            "amd64",
            "rustc",
            &[("version_startup", "ms", 1.0, 0.10)],
        )]);
        let current = report(
            "windows",
            "amd64",
            "rustc",
            &[("version_startup", "ms", 1.0, 0.10)],
        );
        let plan = plan_gate(
            Some(&base),
            &current,
            "scripts/bench/baseline.json",
            10.0,
            true,
            false,
        );
        assert!(!plan.compare);
        assert!(plan.error.is_none());
        assert!(plan
            .notes
            .iter()
            .any(|n| n.contains("no baseline for windows/amd64")));
        assert!(plan.notes.iter().any(|n| n.contains("linux/amd64")));
        assert!(plan
            .notes
            .iter()
            .any(|n| n.contains("cargo run -q -p forge-bench")));

        let required = plan_gate(
            Some(&base),
            &current,
            "scripts/bench/baseline.json",
            10.0,
            false,
            true,
        );
        assert!(required.error.unwrap().contains("windows/amd64"));
    }

    #[test]
    fn rust_version_mismatch_is_noted_but_gate_still_applies() {
        let base = set(vec![report(
            "linux",
            "amd64",
            "rustc 1.84.0",
            &[("version_startup", "ms", 1.0, 0.10)],
        )]);
        let current = report(
            "linux",
            "amd64",
            "rustc 1.85.0",
            &[("version_startup", "ms", 1.0, 0.10)],
        );
        let plan = plan_gate(Some(&base), &current, "baseline.json", 10.0, false, false);
        assert!(plan.compare);
        assert!(plan.notes.iter().any(|n| n.contains("1.84.0")
            && n.contains("1.85.0")
            && n.contains("gate still applies")));
    }

    #[test]
    fn report_mode_regression_exits_zero_strict_exits_one() {
        let base = set(vec![report(
            "linux",
            "amd64",
            "rustc",
            &[("version_startup", "ms", 1.0, 0.10)],
        )]);
        let regressed = report(
            "linux",
            "amd64",
            "rustc",
            &[("version_startup", "ms", 1.5, 0.10)],
        );
        let report_mode = plan_gate(Some(&base), &regressed, "baseline.json", 10.0, false, false);
        assert!(report_mode.compare);
        assert!(report_mode.error.is_none());

        let strict = plan_gate(Some(&base), &regressed, "baseline.json", 10.0, true, false);
        assert_eq!(
            strict.error.as_deref(),
            Some("1 metric(s) regressed beyond their tolerance (-strict)")
        );
    }

    #[test]
    fn new_and_removed_rows_never_fail() {
        let base = set(vec![report(
            "linux",
            "amd64",
            "rustc",
            &[
                ("version_startup", "ms", 1.0, 0.10),
                ("removed_metric", "ms", 1.0, 0.10),
            ],
        )]);
        let current = report(
            "linux",
            "amd64",
            "rustc",
            &[
                ("version_startup", "ms", 1.0, 0.10),
                ("brand_new_metric", "ms", 1.0, 0.10),
            ],
        );
        let plan = plan_gate(Some(&base), &current, "baseline.json", 10.0, true, false);
        assert!(plan.error.is_none());
        assert!(plan.notes.iter().any(|n| n.contains("NEW metric")));
        assert!(plan.notes.iter().any(|n| n.contains("REMOVED metric")));
    }

    #[test]
    fn compare_respects_direction_tolerance_and_fallback() {
        let base = row("throughput", "events/sec", 100.0, 0.0);
        let worse = row("throughput", "events/sec", 88.0, 0.0);
        assert_eq!(compare(&worse, &base, 10.0).verdict, "REGRESSED");
        let better = row("throughput", "events/sec", 112.0, 0.0);
        assert_eq!(compare(&better, &base, 10.0).verdict, "ok (better)");
        let within = row("throughput", "events/sec", 95.0, 0.0);
        assert_eq!(compare(&within, &base, 10.0).verdict, "ok");

        let latency = row("latency", "ms", 100.0, 0.0);
        assert_eq!(
            compare(&row("latency", "ms", 112.0, 0.0), &latency, 10.0).verdict,
            "REGRESSED"
        );
        assert_eq!(
            compare(&row("latency", "ms", 88.0, 0.0), &latency, 10.0).verdict,
            "ok (better)"
        );

        // A row tolerance overrides -fail-over; 10% drift is ok at 0.20.
        let tolerant = row("latency", "ms", 110.0, 0.20);
        assert_eq!(
            compare(&tolerant, &row("latency", "ms", 100.0, 0.10), 10.0).verdict,
            "ok"
        );
        // The comparison reads the BASELINE row's tolerance, not the new one.
        assert_eq!(
            compare(
                &row("latency", "ms", 110.0, 0.90),
                &row("latency", "ms", 100.0, 0.05),
                10.0
            )
            .verdict,
            "REGRESSED"
        );
    }

    #[test]
    fn zero_baseline_value_is_skipped_not_a_regression() {
        let base = row("metric", "ms", 0.0, 0.10);
        let current = row("metric", "ms", 10.0, 0.10);
        let result = compare(&current, &base, 10.0);
        assert_eq!(result.verdict, "SKIP");
        assert!(result.detail.contains("baseline value is 0"));
    }
}
