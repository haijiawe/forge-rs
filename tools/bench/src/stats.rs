//! Small sample statistics shared by every metric.
//!
//! Each metric runs several phases and records one number per phase. The
//! published `value` comes from the FASTEST phase (minimum for `ms` and
//! `ns/op`, maximum for `events/sec`); `p50`/`p95` are nearest-rank
//! percentiles of the same samples. Outliers far below the median are dropped
//! before summarising, matching the Go oracle's `summarize`.

use std::cmp::Ordering;
use std::thread;
use std::time::{Duration, Instant};

/// Higher throughput is better; lower latency is better.
pub fn higher_is_better(unit: &str) -> bool {
    unit == "events/sec" || unit == "MB/s"
}

/// Nearest-rank percentile on a copy of `samples`.
pub fn percentile(samples: &[f64], pct: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let rank = ((pct / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Drop samples below half the median when there are enough left to still
/// summarise; otherwise trust the raw samples.
pub fn summarize(samples: &[f64]) -> Vec<f64> {
    if samples.len() < 3 {
        return samples.to_vec();
    }
    let cutoff = percentile(samples, 50.0) / 2.0;
    let kept: Vec<f64> = samples.iter().copied().filter(|v| *v >= cutoff).collect();
    if kept.len() >= 2 {
        kept
    } else {
        samples.to_vec()
    }
}

/// Round to four decimal places, like the Go oracle's `round`.
pub fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Human-readable number with the oracle's thresholds.
pub fn fmt_num(value: f64) -> String {
    if value >= 1000.0 {
        format!("{value:.0}")
    } else if value >= 100.0 {
        format!("{value:.1}")
    } else if value >= 1.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.4}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub value: f64,
    pub p50: f64,
    pub p95: f64,
    pub samples: usize,
}

/// Build the published statistic for one metric.
pub fn make_stat(raw_samples: &[f64], unit: &str) -> Stat {
    let kept = summarize(raw_samples);
    let p50 = round4(percentile(&kept, 50.0));
    let p95 = round4(percentile(&kept, 95.0));
    let fastest = if higher_is_better(unit) {
        kept.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    } else {
        kept.iter().copied().fold(f64::INFINITY, f64::min)
    };
    Stat {
        value: round4(fastest),
        p50,
        p95,
        samples: kept.len(),
    }
}

/// Time one phase. A zero elapsed time is retried up to six times; the body
/// may fail, in which case its error is returned unchanged.
pub fn measure_phase<F>(mut body: F) -> Result<f64, String>
where
    F: FnMut() -> Result<(), String>,
{
    const ATTEMPTS: u32 = 6;
    for _ in 0..ATTEMPTS {
        let start = Instant::now();
        body()?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        if elapsed_ms > 0.0 {
            return Ok(elapsed_ms);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(format!("clock did not advance across {ATTEMPTS} attempts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let samples: Vec<f64> = (1..=20).map(|v| v as f64).collect();
        assert_eq!(percentile(&samples, 50.0), 10.0);
        assert_eq!(percentile(&samples, 95.0), 19.0);
        assert_eq!(percentile(&samples, 100.0), 20.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn summarize_drops_low_outliers_but_keeps_small_samples() {
        let mut samples = vec![10.0, 11.0, 12.0, 13.0];
        samples.push(0.5); // < median/2
        let kept = summarize(&samples);
        assert_eq!(kept.len(), 4);
        assert!(!kept.contains(&0.5));
        assert_eq!(summarize(&[1.0, 100.0]), vec![1.0, 100.0]);
        assert_eq!(summarize(&[1.0, 100.0, 0.1]), vec![1.0, 100.0]);
    }

    #[test]
    fn fastest_phase_direction_depends_on_unit() {
        let samples = [10.0, 12.0, 11.0, 9.5];
        let latency = make_stat(&samples, "ms");
        assert_eq!(latency.value, 9.5);
        assert!(latency.p50 >= latency.value && latency.p95 >= latency.p50);
        assert_eq!(latency.samples, 4);

        let throughput = make_stat(&samples, "events/sec");
        assert_eq!(throughput.value, 12.0);
        assert!(higher_is_better("events/sec"));
        assert!(higher_is_better("MB/s"));
        assert!(!higher_is_better("ms"));
        assert!(!higher_is_better("ns/op"));
    }

    #[test]
    fn rounding_and_number_format_match_oracle_thresholds() {
        assert_eq!(round4(1.23456), 1.2346);
        assert_eq!(fmt_num(1235.7), "1236");
        assert_eq!(fmt_num(123.46), "123.5");
        assert_eq!(fmt_num(1.23456), "1.23");
        assert_eq!(fmt_num(0.012345), "0.0123");
    }

    #[test]
    fn measure_phase_reports_body_errors() {
        let err = measure_phase(|| Err("boom".to_string())).unwrap_err();
        assert_eq!(err, "boom");
        let ms = measure_phase(|| Ok(())).unwrap();
        assert!(ms >= 0.0);
    }
}
