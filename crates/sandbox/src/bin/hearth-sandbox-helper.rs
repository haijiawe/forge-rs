//! Test child for the sandbox integration tests.
//!
//! The test binary is deliberately a separate executable: a test that needs a
//! grandchild has to start one, and `timeout.exe`/`tasklist.exe` are exactly the
//! host tools the sandbox may refuse. Every mode here is a deterministic,
//! self-contained process that writes a file when it reaches a point.

// Grandchildren are intentionally not waited on: they must stay alive so the
// test can prove that closing or aborting the job object is what kills them.
// Reaping them here would make the containment test pass for the wrong reason.
#![allow(clippy::zombie_processes)]

use std::process::Command;
use std::time::Duration;

fn main() {
    // Be the sandbox wrapper when hearth asks this process to be one: the Windows
    // enforcer re-execs itself with a spec in the environment, and whatever binary
    // hosts that must answer here before it parses its own arguments.
    hearth_sandbox::maybe_exec_helper();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        std::process::exit(2);
    }
    match args[1].as_str() {
        "emit" => {
            let marker = &args[2];
            let _ = std::fs::write(marker, std::process::id().to_string());
            sleep_forever();
        }
        "spawn" => {
            let marker = &args[2];
            let grandchild_marker = &args[3];
            let child = Command::new(std::env::current_exe().expect("current exe"))
                .args(["grandchild", grandchild_marker])
                .spawn()
                .expect("spawn grandchild");
            let pid = child.id().to_string();
            let _ = std::fs::write(marker, &pid);
            let _ = std::fs::write(format!("{marker}.grandchild"), &pid);
            sleep_forever();
        }
        "grandchild" => {
            std::thread::sleep(Duration::from_millis(2000));
            let _ = std::fs::write(&args[2], b"alive");
            sleep_forever();
        }
        "spawn-many" => {
            let count: usize = args[2].parse().expect("count");
            let marker = &args[3];
            let mut spawned = 0usize;
            for _ in 0..count {
                if Command::new(std::env::current_exe().expect("current exe"))
                    .args(["sleep", "60000"])
                    .spawn()
                    .is_ok()
                {
                    spawned += 1;
                }
            }
            let _ = std::fs::write(marker, spawned.to_string());
            sleep_forever();
        }
        "allocate" => {
            let megabytes: usize = args[2].parse().expect("megabytes");
            let bytes = megabytes * 1024 * 1024;
            let data = vec![0u8; bytes];
            // Touch one byte per page so the commit charge is real, not just a
            // reservation that a lazy allocator could hand out for free.
            let mut checksum = 0u64;
            for chunk in data.chunks(4096) {
                checksum = checksum.wrapping_add(chunk[0] as u64);
            }
            println!("{checksum}");
        }
        "sleep" => {
            let milliseconds: u64 = args[2].parse().expect("milliseconds");
            std::thread::sleep(Duration::from_millis(milliseconds));
        }
        "write" => {
            // The file-backed half of containment: this exits instead of sleeping,
            // so a test can ask "did the write succeed" from the exit status alone.
            let path = &args[2];
            if let Err(error) = std::fs::write(path, b"written") {
                eprintln!("write {path}: {error}");
                std::process::exit(3);
            }
        }
        "connect" => {
            // Network containment is only observable from inside: the test listens
            // on loopback, and this child reports whether the connect was allowed.
            let address = &args[2];
            let marker = &args[3];
            let outcome: String = match std::net::TcpStream::connect_timeout(
                &address.parse().expect("socket address"),
                Duration::from_secs(3),
            ) {
                Ok(_) => "connected".to_string(),
                Err(error) => format!("refused: {}", error.kind()),
            };
            let _ = std::fs::write(marker, outcome);
        }
        "read" => {
            // Read containment is only observable from inside: the test grants one
            // path and withholds another, and this child reports which one it could
            // open.
            let target = &args[2];
            let marker = &args[3];
            let outcome = match std::fs::read_to_string(target) {
                Ok(text) => format!("read: {}", text.trim()),
                Err(error) => format!("denied: {}", error.kind()),
            };
            let _ = std::fs::write(marker, outcome);
        }
        other => {
            eprintln!("unknown helper mode {other:?}");
            std::process::exit(2);
        }
    }
}

fn sleep_forever() -> ! {
    std::thread::sleep(Duration::from_secs(120));
    std::process::exit(0)
}
