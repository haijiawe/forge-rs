//! The six measurements. All session work goes through forge-core and
//! forge-session; everything else is std plus the workspace's JSON door.
//!
//! A "phase" is one timing sample. `value` is the fastest phase, and
//! `p50`/`p95` are computed over all phases (see `stats`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use forge_core::{Error, ErrorKind, Event, EventType};
use forge_session::{Options, Projection, Store};
use serde_json::json;

use crate::baseline::MetricRow;
use crate::gate::default_tolerance;
use crate::stats::{self, Stat};

pub const VERSION_RUNS: usize = 20;
pub const FSYNC_APPENDS: usize = 200;
pub const BATCH_APPENDS: usize = 2000;
pub const BATCH_SYNC_EVERY: usize = 100;
pub const LOG_EVENTS: usize = 10_000;
pub const RECORD_BYTES: usize = 3800;
pub const PHASES: usize = 20;
pub const RT_PHASES: usize = 5;
pub const RT_OPS_PER_PHASE: usize = 1000;
pub const RUNTIME_BUDGET: Duration = Duration::from_secs(60);

fn make_event(session: &str) -> Event {
    let text = "x".repeat(RECORD_BYTES);
    let mut event = Event::new(
        session,
        EventType::USER_MESSAGE,
        Some(json!({ "text": text })),
    );
    // Event::new leaves id/ts for Store::append to fill. The codec benchmark
    // parses a canonical line directly, so it needs an already-valid event.
    event.id = "bench-event".to_string();
    event.ts = 1;
    event
}

fn temp_dir(tag: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("forge-bench-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|e| format!("bench: cannot create temp dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Build `forge-cli` and return the exact executable path cargo reports.
/// Guessing `target/release/forge` would break under a custom CARGO_TARGET_DIR
/// (this repository uses an ASCII target dir outside the source tree).
pub fn find_forge_binary(root: &Path) -> Result<PathBuf, String> {
    let output = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--offline",
            "-p",
            "forge-cli",
            "--message-format=json",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("bench: cannot run cargo build forge-cli: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "bench: cargo build forge-cli failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-artifact") {
            continue;
        }
        if let Some(path) = value.get("executable").and_then(|v| v.as_str()) {
            if !path.is_empty() && Path::new(path).is_file() {
                return Ok(PathBuf::from(path));
            }
        }
    }
    Err("bench: cargo build did not report a forge executable".to_string())
}

/// 20 cold `forge --version` launches, one per phase. Unit: ms (lower better).
pub fn version_startup(root: &Path) -> Result<Stat, String> {
    let binary = find_forge_binary(root)?;
    let mut samples = Vec::with_capacity(VERSION_RUNS);
    for _ in 0..VERSION_RUNS {
        let elapsed_ms = stats::measure_phase(|| {
            let output = Command::new(&binary)
                .arg("--version")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("bench: launch forge --version: {e}"))?;
            if !output.status.success() {
                return Err(format!("bench: forge --version exited {}", output.status));
            }
            Ok(())
        })?;
        samples.push(elapsed_ms);
    }
    Ok(stats::make_stat(&samples, "ms"))
}

/// Append-only throughput with a fresh log per phase. A `sync_every` of 1
/// means one fsync per event; 100 batches them. Unit: events/sec (higher).
fn append_throughput(tag: &str, sync_every: usize, count: usize) -> Result<Stat, String> {
    let dir = temp_dir(tag)?;
    let session = format!("bench-{tag}");
    let mut samples = Vec::with_capacity(PHASES);
    for phase in 0..PHASES {
        let path = dir.join(format!("{tag}-{phase}.jsonl"));
        let elapsed_ms = stats::measure_phase(|| {
            let store = Store::open(
                &path,
                Options {
                    sync_every,
                    no_fsync: false,
                },
            )
            .map_err(|e| format!("bench: open {}: {e}", path.display()))?;
            for _ in 0..count {
                store
                    .append(&make_event(&session))
                    .map_err(|e| format!("bench: append: {e}"))?;
            }
            store.close().map_err(|e| format!("bench: close: {e}"))?;
            if store.last_seq() != count as u64 {
                return Err(format!(
                    "bench: expected {count} events, got {}",
                    store.last_seq()
                ));
            }
            Ok(())
        })?;
        samples.push(count as f64 / (elapsed_ms / 1000.0));
        let _ = fs::remove_file(&path);
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(stats::make_stat(&samples, "events/sec"))
}

pub fn session_append_fsync() -> Result<Stat, String> {
    append_throughput("fsync", 1, FSYNC_APPENDS)
}

pub fn session_append_batch() -> Result<Stat, String> {
    append_throughput("batch", BATCH_SYNC_EVERY, BATCH_APPENDS)
}

/// Write one 10k × 4KB log once, then time reopen+replay per phase.
fn make_big_log(path: &Path, session: &str) -> Result<(), String> {
    let store = Store::open(
        path,
        Options {
            sync_every: 1000,
            no_fsync: false,
        },
    )
    .map_err(|e| format!("bench: open big log: {e}"))?;
    for _ in 0..LOG_EVENTS {
        store
            .append(&make_event(session))
            .map_err(|e| format!("bench: append big log: {e}"))?;
    }
    store
        .close()
        .map_err(|e| format!("bench: close big log: {e}"))?;
    if store.last_seq() != LOG_EVENTS as u64 {
        return Err(format!("bench: big log has {} events", store.last_seq()));
    }
    Ok(())
}

fn read_all(path: &Path) -> Result<Vec<Event>, String> {
    let store = Store::open(
        path,
        Options {
            sync_every: usize::MAX,
            no_fsync: true,
        },
    )
    .map_err(|e| format!("bench: reopen {}: {e}", path.display()))?;
    let mut events = Vec::with_capacity(LOG_EVENTS);
    store
        .read_from(0, |event| {
            events.push(event.clone());
            Ok(())
        })
        .map_err(|e| format!("bench: read log: {e}"))?;
    if events.len() != LOG_EVENTS {
        return Err(format!(
            "bench: read {} events, want {LOG_EVENTS}",
            events.len()
        ));
    }
    Ok(events)
}

/// 10,000 events read back through forge-session in phase. Unit: events/sec.
pub fn session_replay_10k() -> Result<Stat, String> {
    let dir = temp_dir("replay")?;
    let path = dir.join("log.jsonl");
    make_big_log(&path, "bench-replay")?;
    let mut samples = Vec::with_capacity(PHASES);
    for _ in 0..PHASES {
        let elapsed_ms = stats::measure_phase(|| {
            let store = Store::open(
                &path,
                Options {
                    sync_every: usize::MAX,
                    no_fsync: true,
                },
            )
            .map_err(|e| format!("bench: reopen replay log: {e}"))?;
            let mut seen = 0usize;
            store
                .read_from(0, |event| {
                    if event.seq != seen as u64 + 1 {
                        return Err(Error::new(
                            ErrorKind::Invalid,
                            format!("bench: seq gap at {seen}"),
                        ));
                    }
                    seen += 1;
                    Ok(())
                })
                .map_err(|e| format!("bench: replay: {e}"))?;
            if seen != LOG_EVENTS {
                return Err(format!("bench: replayed {seen} events"));
            }
            Ok(())
        })?;
        samples.push(LOG_EVENTS as f64 / (elapsed_ms / 1000.0));
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(stats::make_stat(&samples, "events/sec"))
}

/// Same 10k log folded by `Projection::fold`. Rust has no `Project(path)`
/// helper, so the read from disk is explicit and included in the timing.
pub fn session_projection_10k() -> Result<Stat, String> {
    let dir = temp_dir("projection")?;
    let path = dir.join("log.jsonl");
    make_big_log(&path, "bench-projection")?;
    let mut samples = Vec::with_capacity(PHASES);
    for _ in 0..PHASES {
        let elapsed_ms = stats::measure_phase(|| {
            let events = read_all(&path)?;
            let projection = Projection::fold(&events).map_err(|e| format!("bench: fold: {e}"))?;
            let snapshot = projection.snapshot();
            if snapshot.last_seq != LOG_EVENTS as u64
                || snapshot.counters.messages != LOG_EVENTS
                || projection.skipped() != 0
                || projection.duplicates() != 0
            {
                return Err(format!(
                    "bench: projection mismatch (last_seq={}, messages={}, skipped={}, duplicates={})",
                    snapshot.last_seq,
                    snapshot.counters.messages,
                    projection.skipped(),
                    projection.duplicates()
                ));
            }
            Ok(())
        })?;
        samples.push(LOG_EVENTS as f64 / (elapsed_ms / 1000.0));
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(stats::make_stat(&samples, "events/sec"))
}

/// 5 phases × 1,000 canonical marshal + parse operations on one 4KB event.
/// Unit: ns/op (lower better).
pub fn event_roundtrip() -> Result<Stat, String> {
    let event = make_event("bench-roundtrip");
    let mut samples = Vec::with_capacity(RT_PHASES);
    for _ in 0..RT_PHASES {
        let elapsed_ms = stats::measure_phase(|| {
            for _ in 0..RT_OPS_PER_PHASE {
                let line = event
                    .canonical()
                    .map_err(|e| format!("bench: canonical: {e}"))?;
                let back =
                    Event::parse_line(&line).map_err(|e| format!("bench: parse_line: {e}"))?;
                if back.typ != event.typ || back.session != event.session {
                    return Err("bench: roundtrip changed the event".to_string());
                }
            }
            Ok(())
        })?;
        samples.push(elapsed_ms * 1_000_000.0 / RT_OPS_PER_PHASE as f64);
    }
    Ok(stats::make_stat(&samples, "ns/op"))
}

fn row(name: &str, unit: &str, stat: Stat) -> MetricRow {
    MetricRow {
        name: name.to_string(),
        unit: unit.to_string(),
        value: stat.value,
        samples: stat.samples,
        p50: stat.p50,
        p95: stat.p95,
        tolerance: default_tolerance(name),
    }
}

fn maybe_run<F>(
    started: Instant,
    name: &str,
    unit: &str,
    measure: F,
    rows: &mut Vec<MetricRow>,
) -> Result<(), String>
where
    F: FnOnce() -> Result<Stat, String>,
{
    if started.elapsed() >= RUNTIME_BUDGET {
        eprintln!("bench: skip {name}: 60s runtime budget exhausted");
        return Ok(());
    }
    let stat = measure()?;
    rows.push(row(name, unit, stat));
    Ok(())
}

/// Run all six metrics, skipping any that would exceed the 60s budget.
pub fn run_all(root: &Path) -> Result<Vec<MetricRow>, String> {
    let started = Instant::now();
    let mut rows = Vec::with_capacity(6);

    maybe_run(
        started,
        "version_startup",
        "ms",
        || version_startup(root),
        &mut rows,
    )?;
    maybe_run(
        started,
        "session_append_fsync",
        "events/sec",
        session_append_fsync,
        &mut rows,
    )?;
    maybe_run(
        started,
        "session_append_batch",
        "events/sec",
        session_append_batch,
        &mut rows,
    )?;
    maybe_run(
        started,
        "session_replay_10k",
        "events/sec",
        session_replay_10k,
        &mut rows,
    )?;
    maybe_run(
        started,
        "session_projection_10k",
        "events/sec",
        session_projection_10k,
        &mut rows,
    )?;
    maybe_run(
        started,
        "event_roundtrip",
        "ns/op",
        event_roundtrip,
        &mut rows,
    )?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_the_go_oracle() {
        assert_eq!(VERSION_RUNS, 20);
        assert_eq!(FSYNC_APPENDS, 200);
        assert_eq!(BATCH_APPENDS, 2000);
        assert_eq!(BATCH_SYNC_EVERY, 100);
        assert_eq!(LOG_EVENTS, 10_000);
        assert_eq!(RECORD_BYTES, 3800);
        assert_eq!(PHASES, 20);
        assert_eq!(RT_PHASES, 5);
        assert_eq!(RT_OPS_PER_PHASE, 1000);
        assert_eq!(RUNTIME_BUDGET, Duration::from_secs(60));
    }

    #[test]
    fn generated_event_is_roughly_4kb_and_roundtrips() {
        let event = make_event("test");
        let line = event.canonical().unwrap();
        assert!(line.len() >= RECORD_BYTES);
        assert!(line.len() < RECORD_BYTES + 200);
        let back = Event::parse_line(&line).unwrap();
        assert_eq!(back.session, "test");
        assert_eq!(back.typ, EventType::USER_MESSAGE.0);
    }

    #[test]
    fn small_append_run_uses_the_real_session_store() {
        let dir = temp_dir("append-smoke").unwrap();
        let path = dir.join("small.jsonl");
        let store = Store::open(
            &path,
            Options {
                sync_every: 1,
                no_fsync: false,
            },
        )
        .unwrap();
        for _ in 0..5 {
            store.append(&make_event("smoke")).unwrap();
        }
        store.close().unwrap();
        assert_eq!(store.last_seq(), 5);
        let events = read_all_count(&path);
        assert_eq!(events, 5);
        fs::remove_dir_all(dir).unwrap();
    }

    fn read_all_count(path: &Path) -> usize {
        let store = Store::open(
            path,
            Options {
                sync_every: usize::MAX,
                no_fsync: true,
            },
        )
        .unwrap();
        let mut count = 0;
        store
            .read_from(0, |_| {
                count += 1;
                Ok(())
            })
            .unwrap();
        count
    }

    #[test]
    fn row_carries_the_metric_tolerance() {
        let stat = Stat {
            value: 1.0,
            p50: 1.0,
            p95: 1.1,
            samples: 20,
        };
        assert_eq!(
            row("session_append_fsync", "events/sec", stat.clone()).tolerance,
            0.35
        );
        assert_eq!(row("event_roundtrip", "ns/op", stat).tolerance, 0.10);
    }
}
