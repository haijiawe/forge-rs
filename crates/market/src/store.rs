//! `<root>/installed.json` — the record that makes an uninstall possible.
//!
//! Why a record at all: the tarball says which files arrive, but nothing in the
//! filesystem says which of those files a user then edited, or which arrived
//! before the bundle. Without a record, uninstall has exactly two strategies —
//! delete the directory (and lose whatever the user put there) or delete
//! nothing. The record buys the third: delete exactly what this crate created,
//! and only after proving each file still holds the bytes it was installed with.
//!
//! **Every recorded file carries its own digest**, not just the bundle's. That
//! departs from a "files: [relative paths]" shape on purpose: a list of names
//! cannot tell "the operator edited `index.js`" from "the operator has not
//! touched anything", and the whole uninstall-refusal rule hangs on that
//! distinction. The extra digest is what makes the refusal a fact rather than a
//! policy.
//!
//! Saves are atomic (write a sibling temporary, then rename over the record), so
//! a process that dies mid-write leaves the previous record intact rather than a
//! truncated file that would strand every installed bundle.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hearth_core::Result;
use serde::{Deserialize, Serialize};

use crate::error::{
    conflict_with, invalid_with, unsupported_with, ERR_RECORD_CORRUPT, ERR_RECORD_VERSION,
};
use crate::hash::{is_sha256_hex, sha256_hex};
use crate::index::{is_safe_name, is_safe_version};

/// The only record generation this build reads.
pub const RECORD_VERSION: u32 = 1;
/// The record's file name, relative to the market root.
pub const RECORD_NAME: &str = "installed.json";
/// Where installed bundles live, relative to the market root.
pub const BUNDLES_DIR: &str = "bundles";
/// Where an install is assembled before it is moved into place.
pub const STAGING_DIR: &str = ".staging";

/// One file a bundle installed, with the digest it had at install time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFile {
    /// Relative to the bundle directory, `/`-separated.
    pub path: String,
    pub sha256: String,
}

/// One installed bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledRecord {
    pub name: String,
    pub version: String,
    /// The tarball's digest, as the index declared it.
    pub sha256: String,
    /// When it was installed, RFC 3339 in UTC.
    pub installed_at: String,
    pub files: Vec<InstalledFile>,
}

impl InstalledRecord {
    /// The paths this record owns, in the order they were written.
    pub fn paths(&self) -> Vec<&str> {
        self.files.iter().map(|file| file.path.as_str()).collect()
    }

    /// The digest recorded for one path, if the record lists it.
    pub fn digest_of(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.sha256.as_str())
    }
}

/// The whole record document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSet {
    pub version: u32,
    #[serde(default)]
    pub bundles: Vec<InstalledRecord>,
}

impl Default for InstalledSet {
    fn default() -> Self {
        InstalledSet {
            version: RECORD_VERSION,
            bundles: Vec::new(),
        }
    }
}

impl InstalledSet {
    /// The record for `name`, if any two records cannot share a name.
    pub fn find(&self, name: &str) -> Option<&InstalledRecord> {
        self.bundles.iter().find(|record| record.name == name)
    }

    /// Insert or replace a record; replacing is how an upgrade works.
    pub fn put(&mut self, record: InstalledRecord) {
        match self
            .bundles
            .iter_mut()
            .find(|existing| existing.name == record.name)
        {
            Some(existing) => *existing = record,
            None => self.bundles.push(record),
        }
    }

    /// Drop a record by name, returning it.
    pub fn remove(&mut self, name: &str) -> Option<InstalledRecord> {
        let position = self.bundles.iter().position(|record| record.name == name)?;
        Some(self.bundles.remove(position))
    }

    /// Check a document read from disk before anything acts on it.
    ///
    /// The path check is not decoration: uninstall joins each recorded path onto
    /// the bundle directory and deletes the result, so a record with
    /// `../../notes.txt` in it would turn "uninstall" into "delete something
    /// outside the market". A record is a file a user can edit, so it gets the
    /// same suspicion as a tarball.
    pub fn validate(&mut self) -> Result<()> {
        if self.version != RECORD_VERSION {
            return Err(unsupported_with(
                ERR_RECORD_VERSION,
                format!(
                    "got version {}, this build reads {RECORD_VERSION}",
                    self.version
                ),
            ));
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.bundles.len());
        for (position, record) in self.bundles.iter().enumerate() {
            if !is_safe_name(&record.name) {
                return Err(invalid_with(
                    ERR_RECORD_CORRUPT,
                    format!(
                        "bundles[{position}].name {:?} is not a plain name",
                        record.name
                    ),
                ));
            }
            if !is_safe_version(&record.version) {
                return Err(invalid_with(
                    ERR_RECORD_CORRUPT,
                    format!(
                        "bundles[{position}].version {:?} is not plain",
                        record.version
                    ),
                ));
            }
            if !is_sha256_hex(&record.sha256) {
                return Err(invalid_with(
                    ERR_RECORD_CORRUPT,
                    format!("bundles[{position}].sha256 {:?}", record.sha256),
                ));
            }
            if record.installed_at.is_empty() {
                return Err(invalid_with(
                    ERR_RECORD_CORRUPT,
                    format!("bundles[{position}].installedAt is empty"),
                ));
            }
            if record.files.is_empty() {
                return Err(invalid_with(
                    ERR_RECORD_CORRUPT,
                    format!("bundles[{position}] records no files"),
                ));
            }
            if seen.contains(&record.name.as_str()) {
                return Err(invalid_with(
                    ERR_RECORD_CORRUPT,
                    format!("bundles[{position}] repeats the name {:?}", record.name),
                ));
            }
            seen.push(&record.name);
            for (index, file) in record.files.iter().enumerate() {
                if !is_safe_relative_path(&file.path) {
                    return Err(invalid_with(
                        ERR_RECORD_CORRUPT,
                        format!(
                            "bundles[{position}].files[{index}] {:?} is not a relative path under the bundle",
                            file.path
                        ),
                    ));
                }
                if !is_sha256_hex(&file.sha256) {
                    return Err(invalid_with(
                        ERR_RECORD_CORRUPT,
                        format!("bundles[{position}].files[{index}].sha256 is not hex",),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Is `path` a plain relative path that stays under the directory it is joined
/// onto? Pure text: no filesystem access, so it is safe to call while parsing.
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 1024 || path.starts_with('/') {
        return false;
    }
    if path.contains('\\') || path.contains(':') || path.contains('\0') {
        return false;
    }
    // A Windows device name arrives as a plain component and is not a path
    // component at all; `CON`, `NUL` and friends cannot be created or deleted.
    let reserved = ["con", "prn", "aux", "nul", "com1", "lpt1"];
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return false;
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        if reserved.contains(&stem.as_str()) {
            return false;
        }
        if part.ends_with(' ') || part.ends_with('.') {
            return false;
        }
    }
    true
}

/// A market root: where the record lives and where bundles are extracted.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/installed.json`.
    pub fn record_path(&self) -> PathBuf {
        self.root.join(RECORD_NAME)
    }

    /// `<root>/bundles`.
    pub fn bundles_dir(&self) -> PathBuf {
        self.root.join(BUNDLES_DIR)
    }

    /// `<root>/.staging` — where installs are assembled.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(STAGING_DIR)
    }

    /// `<root>/bundles/<name>`.
    pub fn bundle_dir(&self, name: &str) -> PathBuf {
        self.bundles_dir().join(name)
    }

    /// Read the record. A root with no record is an empty set, not an error: a
    /// market that has never installed anything is the normal starting state.
    pub fn load(&self) -> Result<InstalledSet> {
        let path = self.record_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(InstalledSet::default())
            }
            Err(err) => {
                return Err(hearth_core::Error::new(
                    hearth_core::ErrorKind::Io,
                    format!("market: cannot read {}: {err}", path.display()),
                ))
            }
        };
        // As with the index: the generation is checked before the shape, so a v2
        // record is refused for its version rather than for whichever member this
        // build's struct does not happen to know.
        let tree: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|err| invalid_with(ERR_RECORD_CORRUPT, format!("not JSON: {err}")))?;
        match tree.get("version").and_then(serde_json::Value::as_u64) {
            Some(version) if version == u64::from(RECORD_VERSION) => {}
            Some(version) => {
                return Err(unsupported_with(
                    ERR_RECORD_VERSION,
                    format!("got version {version}, this build reads {RECORD_VERSION}"),
                ))
            }
            None => {
                return Err(unsupported_with(
                    ERR_RECORD_VERSION,
                    format!("{} has no numeric \"version\" member", path.display()),
                ))
            }
        }
        let mut set: InstalledSet = serde_json::from_value(tree)
            .map_err(|err| invalid_with(ERR_RECORD_CORRUPT, format!("unexpected shape: {err}")))?;
        set.validate()?;
        Ok(set)
    }

    /// Replace the record atomically.
    pub fn save(&self, set: &InstalledSet) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let mut ordered = set.clone();
        // A stable order makes the file diffable and two runs comparable: the
        // record is the only evidence a reviewer has of what an install did.
        ordered
            .bundles
            .sort_by(|left, right| left.name.cmp(&right.name));
        let text = serde_json::to_vec_pretty(&ordered)
            .map_err(|err| invalid_with(ERR_RECORD_CORRUPT, format!("cannot encode: {err}")))?;
        let path = self.record_path();
        let temporary = self.root.join(format!("{RECORD_NAME}.tmp"));
        fs::write(&temporary, &text)?;
        // On Windows `rename` replaces an existing file (MoveFileEx with
        // MOVEFILE_REPLACE_EXISTING), so the record is never half-written.
        fs::rename(&temporary, &path).map_err(|err| {
            let _ = fs::remove_file(&temporary);
            hearth_core::Error::new(
                hearth_core::ErrorKind::Io,
                format!("market: cannot replace {}: {err}", path.display()),
            )
        })?;
        Ok(())
    }

    /// Every installed record, sorted by name.
    pub fn list(&self) -> Result<Vec<InstalledRecord>> {
        let mut set = self.load()?;
        set.bundles
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(set.bundles)
    }

    /// One record by name.
    pub fn get(&self, name: &str) -> Result<Option<InstalledRecord>> {
        Ok(self.load()?.find(name).cloned())
    }

    /// Refuse when `name` is already installed, naming the version.
    pub fn ensure_absent(&self, name: &str) -> Result<()> {
        if let Some(record) = self.load()?.find(name) {
            return Err(conflict_with(
                crate::error::ERR_ALREADY_INSTALLED,
                format!(
                    "{name} {} is installed at {}",
                    record.version,
                    self.bundle_dir(name).display()
                ),
            ));
        }
        Ok(())
    }

    /// Refuse when the bundle directory exists but no record lists it.
    ///
    /// That combination is a directory this crate did not create — an abandoned
    /// extraction, or an operator's own copy — and installing over it would
    /// overwrite bytes the record does not know about, so a later uninstall
    /// could not put them back.
    pub fn ensure_no_stray_directory(&self, name: &str) -> Result<()> {
        let dir = self.bundle_dir(name);
        if dir.exists() {
            return Err(conflict_with(
                crate::error::ERR_TARGET_EXISTS,
                format!(
                    "{} exists but {} does not list it; move it away first",
                    dir.display(),
                    self.record_path().display()
                ),
            ));
        }
        Ok(())
    }
}

/// The digest a file would have to have to be untouched.
pub fn digest_of_file(path: &Path) -> Result<String> {
    Ok(sha256_hex(&fs::read(path)?))
}

/// Now, as RFC 3339 in UTC (`2026-11-17T15:04:05Z`).
///
/// Hand-rolled rather than adding a date crate: the record needs a timestamp a
/// human can read and a test can match, and that is one calendar conversion.
pub fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_from_epoch(seconds)
}

/// Format a Unix timestamp as RFC 3339 in UTC.
pub fn rfc3339_from_epoch(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rest / 3_600;
    let minute = (rest % 3_600) / 60;
    let second = rest % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days since 1970-01-01 to a civil date (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, files: &[(&str, &[u8])]) -> InstalledRecord {
        InstalledRecord {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            sha256: sha256_hex(b"tarball"),
            installed_at: "2026-11-17T00:00:00Z".to_string(),
            files: files
                .iter()
                .map(|(path, bytes)| InstalledFile {
                    path: (*path).to_string(),
                    sha256: sha256_hex(bytes),
                })
                .collect(),
        }
    }

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hearth-market-store-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn an_empty_root_has_an_empty_record() {
        let store = Store::new(scratch("empty"));
        let set = store.load().expect("load");
        assert!(set.bundles.is_empty());
        assert_eq!(set.version, RECORD_VERSION);
    }

    #[test]
    fn a_saved_record_reads_back_the_same() {
        let root = scratch("roundtrip");
        let store = Store::new(&root);
        let mut set = InstalledSet::default();
        set.put(record("alpha", &[("a.txt", b"a"), ("lib/b.txt", b"b")]));
        store.save(&set).expect("save");
        let read = store.load().expect("load");
        assert_eq!(read, set);
        assert_eq!(read.bundles[0].paths(), vec!["a.txt", "lib/b.txt"]);
        assert_eq!(
            read.bundles[0].digest_of("a.txt"),
            Some(sha256_hex(b"a").as_str())
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_saved_file_is_readable_json_with_a_version() {
        let root = scratch("json");
        let store = Store::new(&root);
        let mut set = InstalledSet::default();
        set.put(record("alpha", &[("a.txt", b"a")]));
        store.save(&set).expect("save");
        let text = fs::read_to_string(store.record_path()).expect("read");
        let tree: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(tree["version"], serde_json::json!(1));
        assert_eq!(tree["bundles"][0]["name"], serde_json::json!("alpha"));
        // The brief's member name, camelCase, is the one on the wire.
        assert!(tree["bundles"][0].get("installedAt").is_some(), "{text}");
        assert!(tree["bundles"][0].get("installed_at").is_none(), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_record_from_another_generation_is_refused_by_version() {
        let root = scratch("version");
        fs::create_dir_all(&root).expect("create");
        fs::write(root.join(RECORD_NAME), br#"{"version":2,"bundles":[]}"#).expect("write");
        let err = Store::new(&root).load().unwrap_err();
        assert!(err.to_string().starts_with(ERR_RECORD_VERSION), "{err}");
        assert_eq!(err.kind(), hearth_core::ErrorKind::UnsupportedVersion);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_record_without_a_version_is_refused() {
        let root = scratch("noversion");
        fs::create_dir_all(&root).expect("create");
        fs::write(root.join(RECORD_NAME), br#"{"bundles":[]}"#).expect("write");
        let err = Store::new(&root).load().unwrap_err();
        assert!(err.to_string().starts_with(ERR_RECORD_VERSION), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_record_that_is_not_json_is_refused() {
        let root = scratch("notjson");
        fs::create_dir_all(&root).expect("create");
        fs::write(root.join(RECORD_NAME), b"<not json>").expect("write");
        let err = Store::new(&root).load().unwrap_err();
        assert!(err.to_string().starts_with(ERR_RECORD_CORRUPT), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_recorded_path_that_escapes_the_bundle_is_refused() {
        let mut set = InstalledSet::default();
        let mut bad = record("alpha", &[("ok.txt", b"x")]);
        bad.files.push(InstalledFile {
            path: "../outside.txt".to_string(),
            sha256: sha256_hex(b"x"),
        });
        set.put(bad);
        let err = set.validate().unwrap_err();
        assert!(err.to_string().starts_with(ERR_RECORD_CORRUPT), "{err}");
        assert!(err.to_string().contains("../outside.txt"), "{err}");
    }

    #[test]
    fn a_record_with_no_files_or_duplicate_names_is_refused() {
        let mut set = InstalledSet::default();
        set.put(record("alpha", &[]));
        let err = set.validate().unwrap_err();
        assert!(err.to_string().starts_with(ERR_RECORD_CORRUPT), "{err}");

        let mut set = InstalledSet::default();
        set.put(record("alpha", &[("a.txt", b"a")]));
        // `put` is a replace, so the duplicate has to be pushed straight onto the
        // list — that is the shape a hand-edited installed.json can have.
        set.bundles.push(record("alpha", &[("b.txt", b"b")]));
        let err = set.validate().unwrap_err();
        assert!(err.to_string().contains("repeats the name"), "{err}");
    }

    #[test]
    fn relative_path_rules_accept_the_ordinary_and_refuse_the_dangerous() {
        for good in ["a.txt", "lib/b.js", "a/b/c/d.json", ".hidden"] {
            assert!(is_safe_relative_path(good), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "C:/x",
            "a\\b",
            "CON",
            "nul.txt",
            "a/./b",
        ] {
            assert!(!is_safe_relative_path(bad), "{bad}");
        }
    }

    #[test]
    fn a_timestamp_is_rfc3339_in_utc() {
        assert_eq!(rfc3339_from_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_epoch(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, which is the one calendar case a naive formula gets wrong.
        assert_eq!(rfc3339_from_epoch(1_709_164_800), "2024-02-29T00:00:00Z");
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(&now[4..5], "-", "{now}");
    }

    #[test]
    fn a_bundle_directory_without_a_record_is_refused() {
        let root = scratch("stray");
        let store = Store::new(&root);
        store.ensure_no_stray_directory("alpha").expect("absent");
        fs::create_dir_all(store.bundle_dir("alpha")).expect("create");
        let err = store.ensure_no_stray_directory("alpha").unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_TARGET_EXISTS),
            "{err}"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
