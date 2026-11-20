//! Entry point. All behaviour lives in the library so tests can call it
//! without spawning a process; this file only wires stdio and the exit code.

#![forbid(unsafe_code)]

use std::io::{self, Write};

fn main() {
    // On Linux the Landlock enforcer starts this binary as its own wrapper and
    // expects this call to apply the ruleset, replace the image, and never
    // return. Doing it before args are read is what makes the wrapper
    // indistinguishable from the real command; on Windows/macOS it is a no-op.
    hearth_sandbox::maybe_exec_helper();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    let code = hearth_cli::run(&args, &mut out, &mut err);
    let _ = out.flush();
    let _ = err.flush();
    std::process::exit(code);
}
