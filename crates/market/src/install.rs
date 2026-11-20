//! `install`, `uninstall`, `list` — the three operations that make a market a
//! market rather than a downloader.
//!
//! The shape of an install is **verify, extract aside, move, record**, in that
//! order, and every step exists because skipping it corrupts something:
//!
//! 1. the tarball's digest is checked before a single byte is extracted, so a
//!    tampered tarball never reaches the disk as files;
//! 2. extraction happens in `<root>/.staging/<unique>`, not in the bundle
//!    directory, so a tarball that fails halfway — a truncated entry, an unsafe
//!    path in the middle — leaves no half-installed bundle where the loader will
//!    find it;
//! 3. the staging directory is renamed into `bundles/<name>`, which is a single
//!    filesystem operation: either the bundle is entirely there or it is not;
//! 4. the record is written last, and if writing it fails the moved directory is
//!    removed again. The record is what makes the files owned; files without a
//!    record would be un-uninstallable.
//!
//! The shape of an uninstall is **verify everything, then delete**, because the
//! two ordered the other way round cannot be undone: proving every file intact
//! after having deleted half of them is not a proof, it is a report of damage.
//!
//! **Tamper refusal.** If a recorded file's digest no longer matches, uninstall
//! refuses the whole operation and names the file. Deleting a file the user
//! edited is data loss, and the user's edit is worth more than a tidy directory.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use hearth_core::Result;

use crate::error::{
    conflict_with, invalid_with, not_found_with, refuse_with, ERR_EMPTY_BUNDLE, ERR_EXTRA_FILES,
    ERR_FILE_MODIFIED, ERR_NOT_INSTALLED, ERR_UNSAFE_PATH,
};
use crate::gzip::{gunzip, is_gzip};
use crate::hash::sha256_hex;
use crate::index::{is_safe_name, is_safe_version, Bundle};
use crate::store::{digest_of_file, now_rfc3339, InstalledFile, InstalledRecord, Store};
use crate::tar::parse_tar;
use crate::unpack::{unpack, Unpacked};

/// What an install did, for the caller to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub name: String,
    pub version: String,
    /// The directory the files landed in, `<root>/bundles/<name>`.
    pub path: PathBuf,
    /// The relative paths written, in archive order.
    pub files: Vec<String>,
}

/// What an uninstall removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub name: String,
    pub version: String,
    /// The files deleted, relative to the bundle directory.
    pub files: Vec<String>,
    /// Directories removed after their files were, relative to the bundle
    /// directory (`""` is the bundle directory itself).
    pub directories: Vec<String>,
}

/// Extract `tarball` into the store and record it.
///
/// `tarball` must already be on disk: the digest is checked against the bytes in
/// the file, which are the bytes that get unpacked. Downloads should go through
/// [`crate::download::download`], which writes and verifies in the same way.
pub fn install(store: &Store, bundle: &Bundle, tarball: &Path) -> Result<Installed> {
    // The index validated these, but a `Bundle` can also be built by hand (the
    // CLI, a mirror, a test). A name becomes a directory and a version becomes a
    // corner of the record, so both are re-checked where they are used.
    if !is_safe_name(&bundle.name) {
        return Err(refuse_with(
            crate::error::ERR_BUNDLE_NAME,
            format!("{:?} is not a directory name", bundle.name),
        ));
    }
    if !is_safe_version(&bundle.version) {
        return Err(refuse_with(
            crate::error::ERR_BUNDLE_VERSION,
            format!("{:?} is not a plain version", bundle.version),
        ));
    }

    store.ensure_absent(&bundle.name)?;
    store.ensure_no_stray_directory(&bundle.name)?;

    // 1. The bytes, before anything is extracted.
    crate::download::verify_file(tarball, bundle)?;
    let compressed = fs::read(tarball)?;
    let payload = if is_gzip(&compressed) {
        gunzip(&compressed)?
    } else {
        compressed
    };
    let archive = parse_tar(&payload)?;
    if archive.file_count() == 0 {
        return Err(refuse_with(
            ERR_EMPTY_BUNDLE,
            format!("{} {} has no files", bundle.name, bundle.version),
        ));
    }

    // 2. Extract aside. The staging name carries the process id so two installs
    // in the same root cannot collide; it is removed by both the success and the
    // failure path, so a leftover one is a crash, not a normal state.
    let staging = fresh_staging(store, &bundle.name)?;
    let unpacked = match unpack(&archive, &staging) {
        Ok(unpacked) => unpacked,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(err);
        }
    };

    // 3. Move into place.
    let target = store.bundle_dir(&bundle.name);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(err) = fs::rename(&staging, &target) {
        let _ = fs::remove_dir_all(&staging);
        return Err(invalid_with(
            ERR_UNSAFE_PATH,
            format!(
                "cannot move the extracted bundle to {}: {err}",
                target.display()
            ),
        ));
    }

    // 4. Record it. A failed record write is undone: files nobody recorded are
    // files nobody can uninstall, and leaving them is worse than failing.
    let record = InstalledRecord {
        name: bundle.name.clone(),
        version: bundle.version.clone(),
        sha256: bundle.sha256.clone(),
        installed_at: now_rfc3339(),
        files: unpacked
            .iter()
            .map(|file| InstalledFile {
                path: file.path.clone(),
                sha256: file.sha256.clone(),
            })
            .collect(),
    };
    let mut set = store.load()?;
    set.put(record);
    if let Err(err) = store.save(&set) {
        let _ = fs::remove_dir_all(&target);
        return Err(err);
    }

    Ok(Installed {
        name: bundle.name.clone(),
        version: bundle.version.clone(),
        path: target,
        files: unpacked.into_iter().map(|file| file.path).collect(),
    })
}

/// Download-free convenience: install the bytes of a tarball already in memory.
///
/// The bytes are written to a staging file first so that the digest is checked
/// with [`crate::download::verify_file`] against a file, exactly as an install
/// from disk is — one code path, one verification.
pub fn install_bytes(store: &Store, bundle: &Bundle, tarball: &[u8]) -> Result<Installed> {
    let staging = fresh_staging(store, &bundle.name)?;
    fs::create_dir_all(&staging)?;
    let path = staging.join("bundle.tar");
    fs::write(&path, tarball)?;
    let result = install(store, bundle, &path);
    // `install` creates its own staging directory; this one only held the copy.
    let _ = fs::remove_dir_all(&staging);
    result
}

/// A staging directory that does not exist yet.
fn fresh_staging(store: &Store, name: &str) -> Result<PathBuf> {
    fs::create_dir_all(store.staging_dir())?;
    let base = store
        .staging_dir()
        .join(format!("{}-{}", name, std::process::id()));
    let mut candidate = base.clone();
    let mut attempt = 0;
    while candidate.exists() {
        attempt += 1;
        if attempt > 1000 {
            return Err(conflict_with(
                ERR_EMPTY_BUNDLE,
                format!("cannot find a free staging name for {name}"),
            ));
        }
        candidate = base.with_extension(attempt.to_string());
    }
    fs::create_dir_all(&candidate)?;
    Ok(candidate)
}

/// Delete exactly what one install recorded, after proving nothing changed.
///
/// Refusals, all before a single unlink:
///   * the bundle is not recorded — [`ERR_NOT_INSTALLED`];
///   * the directory holds a file the record does not list — [`ERR_EXTRA_FILES`];
///     deleting the directory would destroy it;
///   * a recorded file's digest changed, or the file is gone — [`ERR_FILE_MODIFIED`]
///     naming the path.
pub fn uninstall(store: &Store, name: &str) -> Result<Removed> {
    let mut set = store.load()?;
    let record = match set.find(name) {
        Some(record) => record.clone(),
        None => {
            return Err(not_found_with(
                ERR_NOT_INSTALLED,
                format!("{name:?} is not in {}", store.record_path().display()),
            ))
        }
    };

    let dir = store.bundle_dir(name);
    if !dir.exists() {
        // The directory is gone, so there is nothing to lose and nothing to
        // check. Dropping the record is the only sane outcome: keeping it would
        // make `list` claim a bundle that does not exist, forever.
        set.remove(name);
        store.save(&set)?;
        return Ok(Removed {
            name: record.name,
            version: record.version,
            files: Vec::new(),
            directories: Vec::new(),
        });
    }

    // Everything the record should own, and everything that is actually there.
    let recorded: BTreeSet<&str> = record.files.iter().map(|file| file.path.as_str()).collect();
    let present = walk_files(&dir)?;
    for path in &present {
        if !recorded.contains(path.as_str()) {
            return Err(conflict_with(
                ERR_EXTRA_FILES,
                format!(
                    "{} is not in the record for {name}; move it away or reinstall before uninstalling",
                    dir.join(path).display()
                ),
            ));
        }
    }

    // Prove every file is intact before deleting any of them.
    for file in &record.files {
        let path = join_relative(&dir, &file.path)?;
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(conflict_with(
                    ERR_FILE_MODIFIED,
                    format!("{} is recorded but is missing", path.display()),
                ))
            }
            Err(err) => return Err(err.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(conflict_with(
                ERR_FILE_MODIFIED,
                format!("{} was replaced by a symbolic link", path.display()),
            ));
        }
        let actual = digest_of_file(&path)?;
        if actual != file.sha256 {
            return Err(conflict_with(
                ERR_FILE_MODIFIED,
                format!(
                    "{} changed since it was installed (recorded {}, now {actual}); \
                     refusing to delete it",
                    path.display(),
                    file.sha256
                ),
            ));
        }
    }

    // Delete the files, then only the directories that are now empty.
    let mut removed = Vec::with_capacity(record.files.len());
    for file in &record.files {
        let path = join_relative(&dir, &file.path)?;
        fs::remove_file(&path)?;
        removed.push(file.path.clone());
    }
    let mut directories = Vec::new();
    for relative in empty_dirs_deepest_first(&dir)? {
        fs::remove_dir(dir.join(&relative))?;
        directories.push(relative);
    }
    if dir.exists() {
        fs::remove_dir(&dir)?;
        directories.push(String::new());
    }

    set.remove(name);
    store.save(&set)?;
    Ok(Removed {
        name: record.name,
        version: record.version,
        files: removed,
        directories,
    })
}

/// Every installed bundle, sorted by name.
pub fn list(store: &Store) -> Result<Vec<InstalledRecord>> {
    store.list()
}

/// Which recorded files no longer hold the bytes they were installed with.
///
/// Empty means the bundle is intact. This is the read-only half of the rule that
/// [`uninstall`] enforces destructively, exposed so a `market list --verify` can
/// report damage without touching anything.
pub fn modified_files(store: &Store, name: &str) -> Result<Vec<String>> {
    let set = store.load()?;
    let record = set
        .find(name)
        .ok_or_else(|| not_found_with(ERR_NOT_INSTALLED, format!("{name:?} is not installed")))?;
    let dir = store.bundle_dir(name);
    let mut changed = Vec::new();
    for file in &record.files {
        let path = join_relative(&dir, &file.path)?;
        match fs::read(&path) {
            Ok(bytes) => {
                if sha256_hex(&bytes) != file.sha256 {
                    changed.push(file.path.clone());
                }
            }
            Err(_) => changed.push(file.path.clone()),
        }
    }
    Ok(changed)
}

/// Join a recorded relative path onto a base, refusing anything that escapes.
///
/// The record was validated on load, so this cannot fail on a record this crate
/// wrote. It is here because `uninstall` deletes what it returns: the check costs
/// nothing and the failure mode of trusting it is deleting a stranger's file.
fn join_relative(base: &Path, relative: &str) -> Result<PathBuf> {
    if !crate::store::is_safe_relative_path(relative) {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!(
                "{relative:?} is not a safe relative path under {}",
                base.display()
            ),
        ));
    }
    Ok(base.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

/// Every regular file under `dir`, relative and `/`-separated, sorted.
fn walk_files(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    walk(dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(entry.path())?;
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            // A link is neither a recorded file nor a directory to descend into.
            // Counting it as extra is the safe answer: uninstall then refuses
            // rather than deleting the link's target.
            out.push(relative);
        } else if file_type.is_dir() {
            walk(&entry.path(), &relative, out)?;
        } else {
            out.push(relative);
        }
    }
    Ok(())
}

/// Every directory under `dir` (not including `dir`), deepest first.
fn empty_dirs_deepest_first(dir: &Path) -> Result<Vec<String>> {
    let mut dirs = Vec::new();
    collect_dirs(dir, "", &mut dirs)?;
    // Deepest first, so a child is empty by the time its parent is considered.
    dirs.sort_by(|left, right| {
        right
            .matches('/')
            .count()
            .cmp(&left.matches('/').count())
            .then_with(|| right.cmp(left))
    });
    Ok(dirs)
}

fn collect_dirs(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if !meta.file_type().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        collect_dirs(&entry.path(), &relative, out)?;
        out.push(relative);
    }
    Ok(())
}

/// Unpack a tarball into `dest` without installing it. Used by tests and by a
/// `market inspect` that only wants to look.
pub fn extract_to(tarball: &[u8], dest: &Path) -> Result<Vec<Unpacked>> {
    let payload = if is_gzip(tarball) {
        gunzip(tarball)?
    } else {
        tarball.to_vec()
    };
    let archive = parse_tar(&payload)?;
    unpack(&archive, dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tar::fixture::archive as build_tar;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hearth-market-install-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch");
        path
    }

    fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let entries: Vec<(&str, bool, &[u8])> = files
            .iter()
            .map(|(path, data)| (*path, false, *data))
            .collect();
        build_tar(&entries)
    }

    fn bundle_for(name: &str, version: &str, bytes: &[u8]) -> Bundle {
        Bundle {
            name: name.to_string(),
            version: version.to_string(),
            description: "a test bundle".to_string(),
            author: None,
            keywords: None,
            homepage: None,
            tarball: format!("https://example.invalid/{name}-{version}.tar"),
            sha256: sha256_hex(bytes),
            size: bytes.len() as u64,
            engines: None,
            os: None,
            arch: None,
        }
    }

    /// Write a tarball to disk and return its path.
    fn write_tarball(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("bundle.tar");
        fs::write(&path, bytes).expect("write tarball");
        path
    }

    /// Every file under `root` with its digest, so "byte-identical" is a claim
    /// about content and not only about names.
    fn snapshot(root: &Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if !root.exists() {
            return out;
        }
        fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
            let mut entries: Vec<_> = fs::read_dir(dir)
                .expect("read dir")
                .map(|entry| entry.expect("entry"))
                .collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let relative = path
                    .strip_prefix(base)
                    .expect("under base")
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.is_dir() {
                    out.push((format!("{relative}/"), String::new()));
                    walk(&path, base, out);
                } else {
                    out.push((relative, digest_of_file(&path).expect("digest")));
                }
            }
        }
        walk(root, root, &mut out);
        out
    }

    #[test]
    fn an_install_writes_the_files_and_the_record() {
        let root = scratch("ok");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{\"a\":1}"), ("lib/util.js", b"x=1")]);
        let path = write_tarball(&root, &bytes);
        let bundle = bundle_for("alpha", "1.0.0", &bytes);

        let installed = install(&store, &bundle, &path).expect("install");
        assert_eq!(installed.name, "alpha");
        assert_eq!(installed.version, "1.0.0");
        assert_eq!(installed.files, vec!["plugin.json", "lib/util.js"]);
        assert_eq!(
            fs::read(store.bundle_dir("alpha").join("lib/util.js")).expect("read"),
            b"x=1".to_vec()
        );

        let records = list(&store).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "alpha");
        assert_eq!(records[0].version, "1.0.0");
        assert_eq!(records[0].sha256, bundle.sha256);
        assert_eq!(records[0].paths(), vec!["plugin.json", "lib/util.js"]);
        assert!(records[0].installed_at.ends_with('Z'));
        assert!(store.staging_dir().exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tampered_tarball_is_refused_and_nothing_is_written() {
        let root = scratch("tampered");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{\"trusted\":true}")]);
        let bundle = bundle_for("alpha", "1.0.0", &bytes);
        // The index says one thing; the bytes on disk say another, one byte apart.
        let mut tampered = bytes.clone();
        let position = tampered.len() - 512 - 1;
        tampered[position] ^= 0x01;
        let path = write_tarball(&root, &tampered);

        let err = install(&store, &bundle, &path).unwrap_err();
        assert!(
            err.to_string()
                .starts_with(crate::error::ERR_CHECKSUM_MISMATCH),
            "{err}"
        );
        assert!(!store.bundle_dir("alpha").exists());
        // A refusal that never reached the unpack leaves no staging directory at
        // all; one that did must leave it empty. Both are "nothing to clean up".
        let empty = fs::read_dir(store.staging_dir())
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true);
        assert!(empty, "a refused install left staging files behind");
        assert!(list(&store).expect("list").is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_install_leaves_the_root_byte_identical() {
        let root = scratch("atomic");
        let store = Store::new(&root);
        let good = tarball(&[("plugin.json", b"{}")]);
        let good_path = write_tarball(&root, &good);
        install(&store, &bundle_for("first", "1.0.0", &good), &good_path)
            .expect("install the first bundle");

        // A tarball whose second entry is truncated: the parse succeeds for the
        // first entry and fails for the second, which is the partial-extraction
        // case an install must not leak.
        //
        // It is written *before* the baseline snapshot, because the snapshot is
        // of the whole root: a fixture file created in between would show up as
        // "the failed install changed the root" and the assertion would be
        // testing this test's own scaffolding.
        let mut truncated = tarball(&[("a.txt", b"aaaa"), ("b.txt", b"bbbb")]);
        truncated.truncate(512 + 4 + 512);
        let bad_bundle = bundle_for("second", "2.0.0", &truncated);
        let bad_path = root.join("bad.tar");
        fs::write(&bad_path, &truncated).expect("write");

        let before = snapshot(&root);
        let err = install(&store, &bad_bundle, &bad_path).unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");

        let after = snapshot(&root);
        assert_eq!(before, after, "a failed install changed the root");
        assert!(!store.bundle_dir("second").exists());
        assert!(list(&store).expect("list").len() == 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn installing_the_same_name_twice_is_refused_by_name() {
        let root = scratch("twice");
        let store = Store::new(&root);
        let bytes = tarball(&[("a.txt", b"a")]);
        let path = write_tarball(&root, &bytes);
        install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).expect("install");
        let err = install(&store, &bundle_for("alpha", "2.0.0", &bytes), &path).unwrap_err();
        assert!(
            err.to_string()
                .starts_with(crate::error::ERR_ALREADY_INSTALLED),
            "{err}"
        );
        assert_eq!(list(&store).expect("list")[0].version, "1.0.0");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stray_bundle_directory_blocks_the_install() {
        let root = scratch("stray");
        let store = Store::new(&root);
        fs::create_dir_all(store.bundle_dir("alpha")).expect("create");
        let bytes = tarball(&[("a.txt", b"a")]);
        let path = write_tarball(&root, &bytes);
        let err = install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_TARGET_EXISTS),
            "{err}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_uninstall_removes_the_files_and_the_record() {
        let root = scratch("uninstall");
        let store = Store::new(&root);
        let bytes = tarball(&[
            ("plugin.json", b"{}"),
            ("lib/util.js", b"x"),
            ("lib/deep/a.txt", b"a"),
        ]);
        let path = write_tarball(&root, &bytes);
        install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).expect("install");

        let removed = uninstall(&store, "alpha").expect("uninstall");
        assert_eq!(removed.files.len(), 3);
        assert!(!store.bundle_dir("alpha").exists());
        assert!(!store.bundle_dir("alpha").join("lib").exists());
        assert!(list(&store).expect("list").is_empty());
        assert!(store.record_path().exists());
        let text = fs::read_to_string(store.record_path()).expect("read");
        assert!(text.contains("\"bundles\": []"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The rule the brief calls out: a user's edit is not the market's to delete.
    #[test]
    fn an_uninstall_refuses_when_a_recorded_file_changed_and_names_it() {
        let root = scratch("tamper");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{}"), ("lib/util.js", b"x")]);
        let path = write_tarball(&root, &bytes);
        install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).expect("install");

        let edited = store.bundle_dir("alpha").join("lib/util.js");
        fs::write(&edited, b"x = 2 // the user's change").expect("edit");

        let err = uninstall(&store, "alpha").unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_FILE_MODIFIED),
            "{err}"
        );
        assert!(err.to_string().contains("util.js"), "{err}");
        // Nothing was deleted, including the file that did not change.
        assert!(store.bundle_dir("alpha").join("plugin.json").exists());
        assert!(edited.exists());
        assert_eq!(list(&store).expect("list").len(), 1);
        // And the read-only check agrees, without deleting anything.
        assert_eq!(
            modified_files(&store, "alpha").expect("modified"),
            vec!["lib/util.js"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_uninstall_refuses_a_file_the_record_never_listed() {
        let root = scratch("extra");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{}")]);
        let path = write_tarball(&root, &bytes);
        install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).expect("install");
        fs::write(store.bundle_dir("alpha").join("notes.md"), b"mine").expect("write");

        let err = uninstall(&store, "alpha").unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_EXTRA_FILES),
            "{err}"
        );
        assert!(err.to_string().contains("notes.md"), "{err}");
        assert!(store.bundle_dir("alpha").join("plugin.json").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_uninstall_refuses_a_missing_recorded_file() {
        let root = scratch("missing");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{}"), ("gone.txt", b"g")]);
        let path = write_tarball(&root, &bytes);
        install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).expect("install");
        fs::remove_file(store.bundle_dir("alpha").join("gone.txt")).expect("remove");

        let err = uninstall(&store, "alpha").unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_FILE_MODIFIED),
            "{err}"
        );
        assert!(err.to_string().contains("is missing"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_uninstall_of_something_not_installed_is_not_found() {
        let root = scratch("absent");
        let store = Store::new(&root);
        let err = uninstall(&store, "alpha").unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_NOT_INSTALLED),
            "{err}"
        );
        assert_eq!(err.kind(), hearth_core::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_uninstall_whose_directory_vanished_drops_the_record() {
        let root = scratch("vanished");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{}")]);
        let path = write_tarball(&root, &bytes);
        install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).expect("install");
        fs::remove_dir_all(store.bundle_dir("alpha")).expect("remove");

        let removed = uninstall(&store, "alpha").expect("uninstall");
        assert!(removed.files.is_empty());
        assert!(list(&store).expect("list").is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn installing_from_memory_writes_the_same_thing() {
        let root = scratch("bytes");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{}")]);
        let installed =
            install_bytes(&store, &bundle_for("alpha", "1.0.0", &bytes), &bytes).expect("install");
        assert_eq!(installed.files, vec!["plugin.json"]);
        assert_eq!(
            fs::read(store.bundle_dir("alpha").join("plugin.json")).expect("read"),
            b"{}".to_vec()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tarball_with_no_files_is_refused() {
        let root = scratch("empty");
        let store = Store::new(&root);
        let bytes = tarball(&[]);
        let path = write_tarball(&root, &bytes);
        let err = install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).unwrap_err();
        assert!(
            err.to_string().starts_with(crate::error::ERR_EMPTY_BUNDLE),
            "{err}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_recorded_path_that_escapes_is_refused_at_uninstall_time() {
        let root = scratch("escape");
        let store = Store::new(&root);
        let bytes = tarball(&[("plugin.json", b"{}")]);
        let path = write_tarball(&root, &bytes);
        install(&store, &bundle_for("alpha", "1.0.0", &bytes), &path).expect("install");

        // Hand-edit the record the way a bad actor (or a bug) might.
        let mut set = store.load().expect("load");
        let record = set.bundles.first_mut().expect("one record");
        record.files.push(InstalledFile {
            path: "../outside.txt".to_string(),
            sha256: sha256_hex(b"x"),
        });
        let text = serde_json::to_vec_pretty(&set).expect("encode");
        fs::write(store.record_path(), text).expect("write");

        // Loading refuses it, so uninstall never even gets to delete.
        let err = store.load().unwrap_err();
        assert!(
            err.to_string()
                .starts_with(crate::error::ERR_RECORD_CORRUPT),
            "{err}"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
