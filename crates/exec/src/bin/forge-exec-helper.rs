//! Test-only child process used by `tests/exec_test.rs`.
//!
//! This is a real binary, not a test-harness trick: `cargo test` builds bin
//! targets and exposes their path through `CARGO_BIN_EXE_*`, so the integration
//! test can spawn a process whose only job is to behave like a child program.
//! Using the test harness itself would make argv and exit-code assertions
//! depend on libtest's own argument parsing.

use std::io::{Read, Write};
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    match mode.as_str() {
        "sleep" => sleep_forever(),
        "stdout" => {
            print!("{}", rest.first().cloned().unwrap_or_default());
            let _ = std::io::stdout().flush();
        }
        "stderr" => {
            eprint!("{}", rest.first().cloned().unwrap_or_default());
            let _ = std::io::stderr().flush();
        }
        "exit" => {
            let code: i32 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            std::process::exit(code);
        }
        "stdin" => {
            let mut text = String::new();
            let _ = std::io::stdin().read_to_string(&mut text);
            print!("got:{}", text);
        }
        "printenv" => {
            for (name, value) in std::env::vars() {
                println!("{}={}", name, value);
            }
        }
        "printargs" => {
            println!("{}", rest.join("|"));
        }
        "pid" => {
            println!("{}", std::process::id());
        }
        "emit" => {
            let bytes: usize = rest.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            emit_stdout(bytes);
        }
        "emit-err" => {
            let bytes: usize = rest.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            emit_stderr(bytes);
        }
        "touch" => {
            if let Some(path) = rest.first() {
                let _ = std::fs::write(path, b"ran");
            }
        }
        "exists" => {
            let exists = rest
                .first()
                .is_some_and(|path| std::path::Path::new(path).exists());
            println!("{}", if exists { "yes" } else { "no" });
        }
        // Spawn a grandchild that writes `sentinel` after a delay, record its
        // pid in `<sentinel>.spawned`, then sleep. The marker lets the test
        // know the grandchild exists before it cancels or times out.
        "spawn-writer" => {
            let sentinel = rest.first().cloned().unwrap_or_default();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("write-after")
                .arg(&sentinel)
                .arg("2000")
                .spawn();
            if let Ok(child) = child {
                let _ = std::fs::write(format!("{}.spawned", sentinel), child.id().to_string());
            }
            sleep_forever();
        }
        "write-after" => {
            let path = rest.first().cloned().unwrap_or_default();
            let millis: u64 = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
            std::thread::sleep(Duration::from_millis(millis));
            let _ = std::fs::write(path, b"survived");
        }
        "write-marker" => {
            if let Some(path) = rest.first() {
                let _ = std::fs::write(path, b"ran");
            }
        }
        _ => {
            eprintln!("forge-exec-helper: unknown mode {mode:?}");
            std::process::exit(2);
        }
    }
}

fn sleep_forever() {
    std::thread::sleep(Duration::from_secs(300));
}

fn emit_stdout(bytes: usize) {
    let chunk = [b'x'; 8192];
    let mut remaining = bytes;
    let mut out = std::io::stdout();
    while remaining > 0 {
        let take = remaining.min(chunk.len());
        if out.write_all(&chunk[..take]).is_err() {
            return;
        }
        remaining -= take;
    }
    let _ = out.flush();
}

fn emit_stderr(bytes: usize) {
    let chunk = [b'y'; 8192];
    let mut remaining = bytes;
    let mut out = std::io::stderr();
    while remaining > 0 {
        let take = remaining.min(chunk.len());
        if out.write_all(&chunk[..take]).is_err() {
            return;
        }
        remaining -= take;
    }
    let _ = out.flush();
}
