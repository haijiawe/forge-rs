//! The process door for stdio servers. No forge-internal process helper: the
//! MCP contract forbids importing them, and the shape here is streaming rather
//! than one-command-in/one-result-out.

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::{Child, Command, Stdio};

use crate::{Error, Server};

#[cfg(not(windows))]
use crate::process_unix as platform;
#[cfg(not(windows))]
pub(crate) use crate::process_unix::TreeHandle;
#[cfg(windows)]
use crate::process_windows as platform;
#[cfg(windows)]
pub(crate) use crate::process_windows::TreeHandle;

pub(crate) const BASELINE_ENV_NAMES: &[&str] = &[
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

pub(crate) struct SpawnSpec {
    pub path: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<String>,
}

pub(crate) struct Spawned {
    pub child: Child,
    pub tree: TreeHandle,
}

pub(crate) fn spawn(spec: &SpawnSpec, server: &Server) -> Result<Spawned, Error> {
    if spec.path.is_empty() {
        return Err(Error::Config(format!(
            "server {:?}: empty command",
            server.label()
        )));
    }
    let mut command = Command::new(&spec.path);
    command
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_env(&mut command, &spec.env);
    platform::prepare_tree(&mut command);
    let child = command.spawn().map_err(|e| {
        Error::Config(format!(
            "server {:?}: start {:?}: {e}",
            server.label(),
            spec.path
        ))
    })?;
    let tree = {
        #[cfg(windows)]
        {
            platform::new_tree(child.id())
        }
        #[cfg(not(windows))]
        {
            platform::new_tree(child.id())
        }
    };
    Ok(Spawned { child, tree })
}

fn apply_env(command: &mut Command, explicit: &[String]) {
    command.env_clear();
    if explicit.is_empty() {
        for name in BASELINE_ENV_NAMES {
            if let Ok(value) = std::env::var(name) {
                if !value.is_empty() {
                    command.env(name, value);
                }
            }
        }
        return;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut kept: Vec<(String, String)> = Vec::new();
    for entry in explicit {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        let key = key.to_string();
        if let Some(index) = seen.get(&key) {
            kept[*index].1 = value.to_string();
        } else {
            seen.insert(key.clone(), kept.len());
            kept.push((key, value.to_string()));
        }
    }
    for (key, value) in kept {
        command.env(key, value);
    }
    for name in loader_env_names() {
        if seen.contains_key(*name) {
            continue;
        }
        if let Ok(value) = std::env::var(name) {
            if !value.is_empty() {
                command.env(name, value);
            }
        }
    }
}

#[cfg(windows)]
fn loader_env_names() -> &'static [&'static str] {
    platform::LOADER_ENV_NAMES
}

#[cfg(not(windows))]
fn loader_env_names() -> &'static [&'static str] {
    platform::LOADER_ENV_NAMES
}

pub(crate) fn terminate_tree(pid: u32, tree: &TreeHandle) -> Result<(), Error> {
    platform::terminate_tree(pid, tree)
}

pub(crate) fn drop_tree(tree: &mut TreeHandle) {
    platform::drop_tree(tree)
}
