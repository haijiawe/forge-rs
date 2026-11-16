//! Platform selection for `Command::cmd_line`.

#[cfg(windows)]
#[path = "cmdline_windows.rs"]
mod platform;
#[cfg(not(windows))]
#[path = "cmdline_unix.rs"]
mod platform;

pub(crate) use platform::apply;
