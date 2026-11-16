//! Raw Windows command lines.
//!
//! `std::process::Command` escapes every argument for CreateProcess's MSVCRT
//! rules, which cmd.exe does not implement: it reads `\"` as a literal
//! backslash followed by a quote character. A tool that needs `cmd.exe /c
//! echo "hello world"` to see `"hello world"` must therefore pass the line
//! through unescaped.

use std::os::windows::process::CommandExt;

pub(crate) fn apply(
    command: &mut std::process::Command,
    line: Option<&str>,
) -> forge_core::Result<()> {
    if let Some(line) = line {
        if !line.is_empty() {
            // `raw_arg` appends the text after the quoted program path without
            // MSVCRT escaping; Path is the executable and Args is empty, which
            // validate_cmd_line checked before we got here.
            command.raw_arg(line);
        }
    }
    Ok(())
}
