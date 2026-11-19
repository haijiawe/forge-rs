//! `plugin.json`, and the command line it implies.
//!
//! The manifest is data, not a shell command. A manifest that could name an
//! arbitrary command line would be a manifest that can be edited to run one, so
//! the host derives the command from a closed set of runtimes and one relative
//! entry path. That is also why `entry` is checked for escapes before anything
//! is spawned: `"entry": "../../evil.js"` must fail at parse time, not at the
//! moment the child happens to run.
//!
//! The three refusals a manifest can earn are deliberately three *different*
//! codes, because each sends a different person to a different place:
//!
//! - a name outside `^[a-z0-9][a-z0-9._-]{0,63}$` is `bad_manifest` (the author
//!   mistyped the plugin's identity, which is also its directory);
//! - a runtime this host does not start is `unsupported_runtime` (the operator
//!   installed something built for a newer host, or the author guessed);
//! - a manifest naming no entry is `spawn_failed` (there is no program to run,
//!   so the failure happens when a start is attempted, not when it is read).

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Kind, Result};

/// The runtimes the host knows how to start.
///
/// A closed set on purpose. Adding one is a code change with a test, not a
/// string somebody types into a JSON file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Node,
    Python,
    /// Run the entry itself; it must be executable.
    Exec,
}

impl Runtime {
    pub fn as_str(self) -> &'static str {
        match self {
            Runtime::Node => "node",
            Runtime::Python => "python",
            Runtime::Exec => "exec",
        }
    }

    pub fn parse(text: &str) -> Option<Runtime> {
        match text {
            "node" => Some(Runtime::Node),
            "python" => Some(Runtime::Python),
            "exec" => Some(Runtime::Exec),
            _ => None,
        }
    }

    /// The interpreter this runtime starts, or `""` for a self-executing entry.
    pub fn command(self) -> &'static str {
        match self {
            Runtime::Node => "node",
            Runtime::Python => "python",
            Runtime::Exec => "",
        }
    }

    /// Every accepted value, for the refusal message.
    pub fn accepted() -> &'static str {
        "node, python, exec"
    }
}

/// A parsed `plugin.json`.
///
/// `directory` is kept because every message about this plugin is more useful
/// with the path in it, and because `Host::load` needs it to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: Runtime,
    /// Relative to `directory`. Empty means the manifest named no entry, which
    /// [`Manifest::command_line`] refuses with `spawn_failed`.
    pub entry: String,
    pub directory: PathBuf,
}

/// The on-disk shape, kept separate so a missing field produces a message that
/// names the field rather than serde's default text.
#[derive(Debug, Deserialize, Serialize)]
struct RawManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    runtime: String,
    #[serde(default)]
    entry: String,
}

/// Whether `name` is a legal plugin name: `^[a-z0-9][a-z0-9._-]{0,63}$`.
///
/// The name becomes a directory in the plugin root and a prefix on tool names,
/// so the rule is the one both uses. It is written as bytes rather than with
/// `regex` because this crate must not add a dependency for one anchored
/// character class, and because a byte rule is the same rule on every platform.
pub fn is_valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    })
}

/// Read and validate `<directory>/plugin.json`.
pub fn read(directory: &Path) -> Result<Manifest> {
    let path = directory.join("plugin.json");
    let bytes = std::fs::read(&path).map_err(|error| {
        Error::new(
            Kind::BadManifest,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    let raw: RawManifest = serde_json::from_slice(&bytes).map_err(|error| {
        Error::new(
            Kind::BadManifest,
            format!("{} is not valid plugin.json: {error}", path.display()),
        )
    })?;
    parse(raw, directory, &path)
}

fn parse(raw: RawManifest, directory: &Path, path: &Path) -> Result<Manifest> {
    if raw.name.trim().is_empty() {
        return Err(Error::new(
            Kind::BadManifest,
            format!("{} has no name", path.display()),
        ));
    }
    if !is_valid_name(&raw.name) {
        return Err(Error::new(
            Kind::BadManifest,
            format!(
                "{} names the plugin {:?}, which is not ^[a-z0-9][a-z0-9._-]{{0,63}}$; the name is a directory and a tool prefix, so it cannot be free text",
                path.display(),
                raw.name
            ),
        ));
    }
    if raw.version.trim().is_empty() {
        return Err(Error::new(
            Kind::BadManifest,
            format!("{} ({}) has no version", path.display(), raw.name),
        ));
    }
    if raw.description.trim().is_empty() {
        return Err(Error::new(
            Kind::BadManifest,
            format!(
                "{} ({}) has no description; it is what the operator reads when choosing between plugins",
                path.display(),
                raw.name
            ),
        ));
    }
    let runtime = Runtime::parse(raw.runtime.trim()).ok_or_else(|| {
        Error::new(
            Kind::UnsupportedRuntime,
            format!(
                "{} ({}) names the runtime {:?}; accepted values are {}",
                path.display(),
                raw.name,
                raw.runtime,
                Runtime::accepted()
            ),
        )
    })?;
    // An empty entry is kept, not refused here: `plugin.json` is still a valid
    // document describing a plugin that cannot run, and the refusal belongs at
    // spawn time where `spawn_failed` is the code an operator acts on.
    if !raw.entry.trim().is_empty() && !entry_stays_inside(&raw.entry) {
        return Err(Error::new(
            Kind::BadManifest,
            format!(
                "{} ({}) points its entry at {:?}, which leaves the plugin directory",
                path.display(),
                raw.name,
                raw.entry
            ),
        ));
    }
    Ok(Manifest {
        name: raw.name,
        version: raw.version,
        description: raw.description,
        runtime,
        entry: raw.entry,
        directory: directory.to_path_buf(),
    })
}

/// A relative path that never climbs out and never becomes absolute.
///
/// The rules are the same on every platform on purpose. A manifest is a
/// portable document: `C:\evil.js` is an absolute path on Windows and a single
/// harmless filename on Unix, so a per-platform reading would make the same
/// bundle mean two different things. `\` is therefore refused everywhere (a
/// manifest writes `/`, and no legitimate entry needs a backslash), and a drive
/// prefix is refused even when the host has no drives.
fn entry_stays_inside(entry: &str) -> bool {
    if entry.contains('\\') {
        return false;
    }
    let bytes = entry.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return false;
    }
    let path = Path::new(entry);
    if path.is_absolute() {
        return false;
    }
    let mut depth = 0i32;
    for component in path.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            // Prefix and RootDir mean an absolute path on Windows.
            Component::Prefix(_) | Component::RootDir => return false,
        }
    }
    depth > 0
}

impl Manifest {
    /// The argv the host spawns, entry path resolved against the directory.
    ///
    /// A manifest with no entry is `spawn_failed` here rather than at parse:
    /// the document is legible, it simply names nothing that can run.
    pub fn command_line(&self) -> Result<Vec<String>> {
        if self.entry.trim().is_empty() {
            return Err(Error::new(
                Kind::SpawnFailed,
                format!(
                    "{} ({}) names no entry, so there is nothing to run; add \"entry\" to plugin.json",
                    self.directory.display(),
                    self.name
                ),
            ));
        }
        let entry = self.directory.join(&self.entry);
        let entry = entry.display().to_string();
        Ok(match self.runtime {
            Runtime::Exec => vec![entry],
            runtime => vec![runtime.command().to_string(), entry],
        })
    }

    /// The directory the child runs in, so it can read its own files.
    pub fn working_directory(&self) -> &Path {
        &self.directory
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, runtime: &str, entry: &str) -> RawManifest {
        RawManifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            description: "does a thing".to_string(),
            runtime: runtime.to_string(),
            entry: entry.to_string(),
        }
    }

    #[test]
    fn a_complete_manifest_parses_and_yields_its_command_line() {
        let manifest = parse(
            raw("example", "node", "index.js"),
            Path::new("plugins/example"),
            Path::new("plugin.json"),
        )
        .expect("valid");
        assert_eq!(manifest.runtime, Runtime::Node);
        let argv = manifest.command_line().expect("command line");
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0], "node");
        // The separator is the platform's, so compare path components rather
        // than a literal string: a test that hard-codes "/" is a test that
        // fails on the machine this project builds on.
        assert_eq!(
            Path::new(&argv[1]),
            Path::new("plugins/example").join("index.js")
        );
    }

    #[test]
    fn the_three_runtimes_are_the_three_documented_ones() {
        assert_eq!(Runtime::parse("node"), Some(Runtime::Node));
        assert_eq!(Runtime::parse("python"), Some(Runtime::Python));
        assert_eq!(Runtime::parse("exec"), Some(Runtime::Exec));
        assert_eq!(Runtime::parse("deno"), None);
        // The refusal names what is accepted, so a typo is one round trip.
        let error = parse(
            raw("example", "deno", "index.ts"),
            Path::new("."),
            Path::new("plugin.json"),
        )
        .expect_err("unknown runtime");
        assert_eq!(error.kind(), Kind::UnsupportedRuntime);
        assert!(
            error.message().contains("node, python, exec"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_name_that_is_not_a_plain_slug_is_refused() {
        for bad in ["Example", "1 bad", "", "-start", "a/b", "a\\b", ".hidden"] {
            let error = parse(
                raw(bad, "node", "index.js"),
                Path::new("."),
                Path::new("plugin.json"),
            )
            .err()
            .unwrap_or_else(|| panic!("{bad:?} should be refused"));
            assert_eq!(
                error.kind(),
                Kind::BadManifest,
                "{bad:?}: {}",
                error.message()
            );
        }
        assert!(is_valid_name("a"));
        assert!(is_valid_name("my-plugin.v2"));
        assert!(is_valid_name("0day"));
        assert!(!is_valid_name(&"a".repeat(65)));
        assert!(is_valid_name(&"a".repeat(64)));
    }

    #[test]
    fn a_missing_field_names_the_field() {
        let mut missing = raw("example", "node", "index.js");
        missing.version = String::new();
        let error =
            parse(missing, Path::new("."), Path::new("plugin.json")).expect_err("no version");
        assert!(error.message().contains("version"), "{}", error.message());

        let mut missing = raw("example", "node", "index.js");
        missing.description = String::new();
        let error =
            parse(missing, Path::new("."), Path::new("plugin.json")).expect_err("no description");
        assert!(
            error.message().contains("description"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_manifest_with_no_entry_parses_but_cannot_produce_a_command_line() {
        // Three refusals, three codes: a bad name is `bad_manifest`, an unknown
        // runtime is `unsupported_runtime`, and a manifest with nothing to run
        // is `spawn_failed` — the failure to act on is "start", not "read".
        let manifest = parse(
            raw("example", "node", ""),
            Path::new("plugins/example"),
            Path::new("plugin.json"),
        )
        .expect("the document is legible");
        let error = manifest.command_line().expect_err("nothing to run");
        assert_eq!(error.kind(), Kind::SpawnFailed);
        assert!(error.message().contains("entry"), "{}", error.message());
    }

    #[test]
    fn an_entry_that_climbs_out_of_the_directory_is_refused() {
        for bad in [
            "../evil.js",
            "a/../../evil.js",
            "/etc/passwd",
            "C:\\evil.js",
            "C:evil.js",
            "sub\\index.js",
        ] {
            let error = parse(
                raw("example", "node", bad),
                Path::new("."),
                Path::new("plugin.json"),
            )
            .err()
            .unwrap_or_else(|| panic!("{bad:?} should be refused"));
            assert_eq!(error.kind(), Kind::BadManifest, "{bad:?}");
        }
        // A subdirectory is fine, and so is a plain name.
        assert!(entry_stays_inside("dist/index.js"));
        assert!(entry_stays_inside("index.js"));
        assert!(entry_stays_inside("./index.js"));
    }

    #[test]
    fn a_manifest_on_disk_that_is_not_json_is_refused_by_path() {
        let dir = std::env::temp_dir().join("forge-plugin-manifest-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("plugin.json"), b"{ not json").expect("write");
        let error = read(&dir).expect_err("bad json");
        assert_eq!(error.kind(), Kind::BadManifest);
        assert!(
            error.message().contains("plugin.json"),
            "{}",
            error.message()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
