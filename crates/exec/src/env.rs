//! Environment construction for a child process.
//!
//! The policy is deliberately narrow: the harness's own environment can hold
//! API keys and proxy credentials that a tool must not see by default, but a
//! completely empty environment breaks the platform loader (Windows cannot
//! start without `SystemRoot`). Three cases are therefore distinct:
//!
//! * `None` -> the historical minimal whitelist;
//! * `Some(vec![])` -> only the platform loader minimum;
//! * `Some(list)` -> exactly `list` plus the loader minimum Windows needs.

use std::process::Command;

/// Variables a child keeps when the caller does not specify an environment.
const BASELINE: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SHELL",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "LANG",
    "LC_ALL",
    "TZ",
];

/// Variables Windows itself needs to load an executable. These are added even
/// when the caller passes an explicit environment, because without them
/// CreateProcess can fail before the child's own code runs.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const WINDOWS_LOADER: &[&str] = &["SystemRoot", "SystemDrive", "windir", "COMSPEC", "PATHEXT"];

pub(crate) fn apply(command: &mut Command, explicit: Option<&[String]>) {
    let entries = build(explicit);
    // `env_clear` first: if we only overlaid variables, every variable the
    // caller did not mention would leak from the harness.
    command.env_clear();
    for (name, value) in entries {
        command.env(name, value);
    }
}

fn build(explicit: Option<&[String]>) -> Vec<(String, String)> {
    let inherit =
        explicit.is_some_and(|entries| entries.iter().any(|entry| entry == crate::ENV_INHERIT));
    let mut out: Vec<(String, String)> = Vec::new();

    if inherit {
        for (name, value) in std::env::vars() {
            set(&mut out, &name, value);
        }
    }

    match explicit {
        Some(entries) => {
            for entry in entries {
                if entry == crate::ENV_INHERIT {
                    continue;
                }
                if let Some((name, value)) = entry.split_once('=') {
                    set(&mut out, name, value.to_string());
                }
                // An entry without '=' is not a variable; passing it to
                // `Command::env` would panic, so it is ignored rather than
                // aborting the run.
            }
        }
        None => {
            for name in BASELINE {
                if let Ok(value) = std::env::var(name) {
                    set(&mut out, name, value);
                }
            }
        }
    }

    #[cfg(windows)]
    {
        for name in WINDOWS_LOADER {
            if !out
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
            {
                if let Ok(value) = std::env::var(name) {
                    set(&mut out, name, value);
                }
            }
        }
    }

    out
}

/// Insert or replace one variable. Windows environment names are
/// case-insensitive, so `Path` and `PATH` must not both reach the child.
fn set(entries: &mut Vec<(String, String)>, name: &str, value: String) {
    if cfg!(windows) {
        entries.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    } else {
        entries.retain(|(existing, _)| existing != name);
    }
    entries.push((name.to_string(), value));
}
