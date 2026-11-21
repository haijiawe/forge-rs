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
                            show_error(&handle, &problem);
                            return;
                        }
                    },
                    Err(problem) => {
                        eprintln!("hearth-desktop: {problem}");
                        show_error(&handle, &problem);
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
            let name = if cfg!(windows) {
                "hearth.exe"
            } else {
                "hearth"
            };
            let sibling = dir.join(name);
            if sibling.is_file() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    "hearth".to_string()
}

/// Show why the server could not be started, in a window that explains it.
///
/// This used to build a `data:text/html` URL. That fails twice over: Tauri
/// refuses `data:` URLs unless the `webview-data-url` feature is compiled in,
/// and the failure happens while building the window — so the one page whose job
/// is to report a problem reported nothing, and the process exited with a blank
/// screen. An initialization script has no such precondition: it runs in the
/// bundled fallback page before that page's own scripts, on every load, so the
/// message cannot arrive before or after the document it describes.
fn show_error(handle: &tauri::AppHandle, problem: &str) {
    let inside = handle.clone();
    let failed = handle.clone();
    let caller = handle.clone();
    let message = problem.to_string();
    let result = caller.run_on_main_thread(move || {
        if let Err(error) = open_error_window(&inside, &message) {
            eprintln!("hearth-desktop: could not show the failure: {error}");
            failed.exit(1);
        }
    });
    if let Err(error) = result {
        eprintln!("hearth-desktop: could not reach the main thread: {error}");
    }
}

/// Build the failure window. Main thread only.
fn open_error_window(handle: &tauri::AppHandle, problem: &str) -> tauri::Result<()> {
    WebviewWindowBuilder::new(handle, "main", WebviewUrl::App("index.html".into()))
        .title("hearth 没能启动")
        .inner_size(900.0, 420.0)
        .min_inner_size(600.0, 300.0)
        .initialization_script(error_script(problem))
        .build()?;
    Ok(())
}

/// The script that replaces the fallback page with the failure message.
///
/// The text is escaped for HTML and then embedded as a JavaScript string
/// literal; both matter, because the argument is whatever the child process
/// printed.
fn error_script(problem: &str) -> String {
    let escaped = problem
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let html = format!(
        "<div style=\"margin:0;min-height:100vh;background:#1a1c21;color:#e8eaee;\
         font:14px/1.6 ui-monospace,monospace;padding:28px;box-sizing:border-box\">\
         <h1 style=\"font-size:16px;margin:0 0 12px\">hearth 没能启动</h1>\
         <p style=\"color:#ff8a3d\">{escaped}</p>\
         <p style=\"color:#8b8f98\">把窗口该用的参数传给它，例如<br>\
         <code>hearth-desktop --model deepseek-chat</code><br>\
         或者把 HEARTH_BIN 指向 hearth 可执行文件。</p></div>"
    );
    let literal = js_string(&html);
    format!(
        "document.addEventListener('DOMContentLoaded', function () {{\
         document.documentElement.innerHTML = {literal};}});"
    )
}

/// Encode one string as a JavaScript string literal.
///
/// Written by hand rather than pulling in a serializer: the only caller is the
/// script above, and JSON's string escaping is a subset of JavaScript's — with
/// the two line-separator characters added, which JSON may emit raw and
/// JavaScript may not.
fn js_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
