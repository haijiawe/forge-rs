//! The desktop shell: a window around the local server.
//!
//! This crate is a launcher, not an application. It starts `hearth web` as a
//! child process, waits for the server to print the URL it is serving (the
//! server picks the port, so the shell must not choose one), and then opens a
//! window at that URL. Everything in the window is the same web UI a browser
//! would get.
//!
//! Why a child process instead of linking the server in:
//!
//! * The UI talks to the server over HTTP with a bearer token. Running that over
//!   a loopback socket is the same path the browser uses, so a bug in the shell
//!   cannot quietly diverge from a bug in the browser.
//! * The server owns the sandbox, the approval prompt and the event log. A shell
//!   that linked it would have to be trusted with all three, and a window frame
//!   has no business being trusted with anything.
//! * `hearth` stays one binary that runs anywhere; the shell is an extra, and a
//!   machine without a desktop is not made worse by it.
//!
//! The window is created *after* the URL is known, and on the main thread, both
//! on purpose.
//!
//! Waiting for the URL must not block the main thread, because that thread is
//! the one that pumps the window's messages. Creating the window from the
//! waiting thread is worse: the document loads and its DOM is complete (read
//! through WebView2's debugging port: `#root` had 7,819 bytes of markup and an
//! empty error list) while the window paints nothing but black, because the
//! WebView2 controller belongs to the thread that runs the event loop. So the
//! wait happens on a worker thread and the window is built back on the main
//! thread through `run_on_main_thread`.
//!
//! The one thing this crate does *not* do is open a browser: it passes
//! `--no-browser` so the server does not launch Edge behind the window.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// How long to wait for the server to print the URL it is serving.
///
/// Starting the server is milliseconds of work; this bound exists so a wedged
/// start ends with a sentence instead of a process that never shows anything.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// The line `hearth web` writes once it is listening. The prefix, not the whole
/// line, because the URL after it is what the shell needs and the server owns
/// its own spelling of the rest.
const READY_PREFIX: &str = "hearth web: open ";

/// Holds the server child so it can be stopped when the window closes.
struct ServerProcess(Mutex<Option<Child>>);

/// Put the running server where the exit handler can find it.
///
/// A free function rather than an inline `lock()`: the borrow checker is right
/// that a guard taken from a temporary `State` in the same block as the
/// temporary's drop is a use-after-free waiting to happen, and keeping the lock
/// inside one function makes that impossible to reintroduce.
fn remember_child(state: &ServerProcess, child: Child) {
    if let Ok(mut slot) = state.0.lock() {
        *slot = Some(child);
    }
}

/// Take the server out, so it is stopped exactly once.
fn take_child(state: &ServerProcess) -> Option<Child> {
    state.0.lock().ok().and_then(|mut slot| slot.take())
}

fn main() {
    tauri::Builder::default()
        .manage(ServerProcess(Mutex::new(None)))
        .setup(|app| {
            let handle = app.handle().clone();
            let args: Vec<String> = std::env::args().skip(1).collect();
            std::thread::spawn(move || {
                let (outcome, child) = start_server(&args);
                if let Some(child) = child {
                    remember_child(&handle.state::<ServerProcess>(), child);
                }
                let target = match outcome {
                    Ok(url) => match url.parse::<tauri::Url>() {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            // A URL the server itself printed that will not parse
                            // is a defect worth showing, not swallowing.
                            let problem = format!("the server said it was serving {url}, which is not a URL: {error}");
                            eprintln!("hearth-desktop: {problem}");
                            show(&handle, error_url(&problem));
                            return;
                        }
                    },
                    Err(problem) => {
                        eprintln!("hearth-desktop: {problem}");
                        show(&handle, error_url(&problem));
                        return;
                    }
                };
                eprintln!("hearth-desktop: serving {target}");
                show(&handle, target);
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("build the hearth window")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } | RunEvent::Exit = event {
                if let Some(mut child) = take_child(&app.state::<ServerProcess>()) {
                    // The server is a child of this process; killing it here is
                    // what keeps a closed window from leaving a listener behind
                    // on a loopback port.
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        });
}

/// Open the single window of this process, from the main thread.
///
/// Called through `run_on_main_thread`: see the module documentation for what
/// happens when the window is built on the waiting thread instead.
fn show(handle: &tauri::AppHandle, url: tauri::Url) {
    let inside = handle.clone();
    let failed = handle.clone();
    let caller = handle.clone();
    let result = caller.run_on_main_thread(move || {
        if let Err(error) = open_window(&inside, url) {
            eprintln!("hearth-desktop: could not open a window: {error}");
            failed.exit(1);
        }
    });
    if let Err(error) = result {
        eprintln!("hearth-desktop: could not reach the main thread: {error}");
    }
}

/// Build the window. Main thread only.
fn open_window(handle: &tauri::AppHandle, url: tauri::Url) -> tauri::Result<()> {
    WebviewWindowBuilder::new(handle, "main", WebviewUrl::External(url))
        .title("hearth")
        .inner_size(1440.0, 900.0)
        .min_inner_size(900.0, 600.0)
        .build()?;
    Ok(())
}

/// Start `hearth web`, forwarding the shell's own arguments, and return the URL
/// it printed.
///
/// `--no-browser` is appended rather than left to the caller: the whole point of
/// the shell is that the window is the browser.
fn start_server(args: &[String]) -> (Result<String, String>, Option<Child>) {
    let program = server_program();
    let mut command = Command::new(&program);
    command
        .arg("web")
        .args(args)
        .arg("--no-browser")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return (
                Err(format!(
                    "could not start {program}: {error}. Set HEARTH_BIN to the path of the hearth binary."
                )),
                None,
            )
        }
    };

    let stderr = child.stderr.take();
    let lines = match stderr {
        Some(stderr) => {
            let (sender, receiver) = channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
            receiver
        }
        None => {
            let (_sender, receiver) = channel::<String>();
            receiver
        }
    };

    match wait_for_url(&lines) {
        Ok(url) => (Ok(url), Some(child)),
        Err(problem) => {
            let _ = child.kill();
            let _ = child.wait();
            (Err(problem), None)
        }
    }
}

/// Read the server's stderr until it says where it is serving.
fn wait_for_url(lines: &Receiver<String>) -> Result<String, String> {
    let deadline = Instant::now() + START_TIMEOUT;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the server did not say where it was serving within {} seconds{}{}",
                START_TIMEOUT.as_secs(),
                if seen.is_empty() { "" } else { "; it said: " },
                seen.join("; ")
            ));
        }
        match lines.recv_timeout(left) {
            Ok(line) => {
                if let Some(url) = line.strip_prefix(READY_PREFIX) {
                    let url = url.trim();
                    if url.is_empty() {
                        return Err("the server printed an empty URL".to_string());
                    }
                    return Ok(url.to_string());
                }
                // Kept for the failure message: a server that refuses to start
                // prints why on the same stream, and that sentence is the whole
                // diagnosis.
                if seen.len() < 8 {
                    seen.push(line.trim().to_string());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!(
                    "the server exited before saying where it was serving{}",
                    if seen.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", seen.join("; "))
                    }
                ))
            }
        }
    }
}

/// Where the hearth binary is.
///
/// `HEARTH_BIN` wins, then a sibling of this executable (the layout `hearth
/// install` produces), then whatever is on `PATH`. A shell that guessed wrong
/// silently would start a *different* build than the one beside it, which is the
/// kind of bug that makes a fixed defect look unfixed.
fn server_program() -> String {
    if let Ok(explicit) = std::env::var("HEARTH_BIN") {
        if !explicit.trim().is_empty() {
            return explicit;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let name = if cfg!(windows) { "hearth.exe" } else { "hearth" };
            let sibling = dir.join(name);
            if sibling.is_file() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    "hearth".to_string()
}

/// A `data:` URL for the page shown when the server could not be started.
///
/// The window is a fresh document rather than a placeholder that gets patched
/// afterwards, so there is no moment where the reader sees the wrong page.
fn error_url(problem: &str) -> tauri::Url {
    let escaped = problem
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let page = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>hearth</title>\
         <body style=\"margin:0;height:100vh;background:#1a1c21;color:#e8eaee;font:14px/1.6 ui-monospace,monospace;padding:28px;box-sizing:border-box\">\
         <h1 style=\"font-size:16px;margin:0 0 12px\">hearth could not start</h1>\
         <p style=\"color:#ff8a3d\">{escaped}</p>\
         <p style=\"color:#8b8f98\">Pass the flags this window should use, for example<br>\
         <code>hearth-desktop --model deepseek-chat</code><br>\
         or set HEARTH_BIN to the hearth binary.</p></body>"
    );
    let mut encoded = String::from("data:text/html;charset=utf-8,");
    for byte in page.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    // The encoding above is by construction a valid URL; falling back to the app
    // page would show a blank window, so a failure here is reported as one.
    encoded
        .parse()
        .unwrap_or_else(|_| "about:blank".parse().expect("about:blank parses"))
}