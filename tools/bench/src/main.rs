use std::path::Path;
use std::process::exit;

use hearth_bench::args;
use hearth_bench::baseline::{self, Report};
use hearth_bench::gate::{self, CompareResult};
use hearth_bench::metrics;
use hearth_bench::platform;
use hearth_bench::stats::fmt_num;
use hearth_bench::time;

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let opts = match args::parse_args(&raw) {
        Ok(opts) => opts,
        Err(error) => {
            eprintln!("{error}");
            eprint!("{}", args::usage());
            exit(2);
        }
    };
    if opts.help {
        print!("{}", args::usage());
        return;
    }

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => fail(&format!("bench: cannot read current directory: {error}")),
    };
    let root = match platform::discover_root(&cwd) {
        Ok(root) => root,
        Err(error) => fail(&error),
    };
    let baseline_set = match baseline::load_baseline_file(&opts.baseline) {
        Ok(set) => set,
        Err(error) => fail(&error),
    };
    let report = match build_report(&root) {
        Ok(report) => report,
        Err(error) => fail(&error),
    };
    if report.results.is_empty() {
        fail("bench: no metrics completed; nothing to report or record");
    }

    let plan = gate::plan_gate(
        baseline_set.as_ref(),
        &report,
        &opts.baseline,
        opts.fail_over,
        opts.strict,
        opts.require_baseline,
    );
    for note in &plan.notes {
        println!("{note}");
    }

    println!(
        "bench: platform {}/{} rustVersion={} timestamp={}",
        report.os, report.arch, report.rust_version, report.timestamp
    );
    let compare_to = if plan.compare {
        baseline_set
            .as_ref()
            .and_then(|set| baseline::select_baseline(set, &report.os, &report.arch))
    } else {
        None
    };
    print_rows(&report, compare_to.as_ref(), opts.fail_over);

    if let Some(error) = plan.error {
        fail(&error);
    }
    if !opts.out.is_empty() {
        if let Err(error) = baseline::write_baseline(&opts.out, &report) {
            fail(&error);
        }
        println!(
            "bench: recorded {} metric(s) for {}/{} in {}",
            report.results.len(),
            report.os,
            report.arch,
            opts.out
        );
    }
}

fn build_report(root: &Path) -> Result<Report, String> {
    Ok(Report {
        os: platform::os().to_string(),
        arch: platform::arch(),
        rust_version: platform::rust_version(),
        timestamp: time::utc_now_rfc3339(),
        results: metrics::run_all(root)?,
    })
}

fn print_rows(report: &Report, baseline: Option<&Report>, fail_over_pct: f64) {
    for current in &report.results {
        let verdict: Option<CompareResult> = baseline.and_then(|base| {
            base.results
                .iter()
                .find(|row| row.name == current.name)
                .map(|row| gate::compare(current, row, fail_over_pct))
        });
        let suffix = verdict
            .as_ref()
            .map(|result| format!("  {}", result.verdict))
            .unwrap_or_default();
        println!(
            "bench: {:<22} {} {}  p50={} p95={} samples={} tol={:.0}%{suffix}",
            current.name,
            fmt_num(current.value),
            current.unit,
            fmt_num(current.p50),
            fmt_num(current.p95),
            current.samples,
            current.tolerance * 100.0
        );
        if let Some(result) = verdict {
            println!("bench:   {}", result.detail);
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    exit(1);
}
