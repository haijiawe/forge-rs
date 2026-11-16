//! Platform selection for process-tree tracking and process-group setup.

#[cfg(windows)]
#[path = "tree_windows.rs"]
mod platform;
#[cfg(unix)]
#[path = "tree_unix.rs"]
mod platform;
#[cfg(not(any(windows, unix)))]
#[path = "tree_unsupported.rs"]
mod platform;

pub(crate) use platform::{configure, ProcessTree};
