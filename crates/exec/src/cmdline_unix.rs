//! Raw command lines do not exist outside Windows.
//!
//! `validate_cmd_line` rejects a non-empty `cmd_line` on this platform before
//! the command is built, so this function is only reachable with `None` or an
//! empty line. Silently ignoring a non-empty line would run a different command
//! than the caller wrote.

pub(crate) fn apply(
    _command: &mut std::process::Command,
    _line: Option<&str>,
) -> hearth_core::Result<()> {
    Ok(())
}
