//! Writing a parsed [`Archive`] under a destination directory.
//!
//! The whole point of this module is the containment rule: **every byte an
//! extraction writes lands under `dest`.** The tar reader has already refused
//! absolute paths, `..` components, drive prefixes and non-file entry types; this
//! module is the second, different check — it resolves each path component
//! against the real filesystem and refuses a component that is a symbolic link,
//! because a symlink planted in the destination turns `dest/link/evil` into
//! `elsewhere/evil` without any archive entry containing `..`.
//!
//! Two checks that are not the same check catch different mistakes. Neither is
//! optional, and neither is "defence in depth" in the decorative sense: the tar
//! reader cannot see the filesystem and this module cannot see the header.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use forge_core::Result;

use crate::error::{refuse_with, ERR_EMPTY_BUNDLE, ERR_UNSAFE_PATH};
use crate::hash::sha256_hex;
use crate::tar::Archive;

/// One extracted file, with the digest of the bytes that are now on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unpacked {
    /// Relative to the destination, `/`-separated.
    pub path: String,
    pub sha256: String,
}

/// Write every entry of `archive` under `dest`.
///
/// `dest` is created if it does not exist. An existing file at `dest` is an
/// error: a bundle directory that is a file cannot be extracted into, and
/// half-replacing it would be a silent partial install.
///
/// On any error the caller is responsible for removing `dest`; this function
/// does not clean up after itself because a partial tree is exactly what the
/// caller's rollback wants to see and delete.
pub fn unpack(archive: &Archive, dest: &Path) -> Result<Vec<Unpacked>> {
    if dest.exists() && !dest.is_dir() {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!("{} exists and is not a directory", dest.display()),
        ));
    }
    fs::create_dir_all(dest)?;
    let base = fs::canonicalize(dest)?;

    let mut written: Vec<Unpacked> = Vec::with_capacity(archive.file_count());
    for entry in &archive.entries {
        let target = resolve_beneath(&base, &entry.path)?;
        if entry.is_dir {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // `create_new` rather than `create`: the archive reader refuses duplicate
        // paths, so a file that already exists here was not put there by this
        // extraction, and overwriting it would mean trusting the destination.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|err| {
                forge_core::Error::new(
                    forge_core::ErrorKind::Io,
                    format!("market: cannot create {}: {err}", target.display()),
                )
            })?;
        file.write_all(&entry.data)?;
        file.flush()?;
        written.push(Unpacked {
            path: entry.path.clone(),
            sha256: sha256_hex(&entry.data),
        });
    }

    if written.is_empty() {
        return Err(refuse_with(ERR_EMPTY_BUNDLE, "no files were written"));
    }
    Ok(written)
}

/// Resolve `relative` under `base`, refusing anything that leaves it.
///
/// `base` must already be canonical. The walk is component by component so that
/// a symbolic link anywhere in the middle is caught *before* the file below it
/// is created.
fn resolve_beneath(base: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = base.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(refuse_with(
                ERR_UNSAFE_PATH,
                format!("{relative:?} has a component that is not a plain name"),
            ));
        }
        // A name that Windows treats specially even though it is a "plain" name.
        if part.contains(':') || part.contains('\\') {
            return Err(refuse_with(
                ERR_UNSAFE_PATH,
                format!("{relative:?} has a component with a drive or separator"),
            ));
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(refuse_with(
                    ERR_UNSAFE_PATH,
                    format!(
                        "{relative:?} would be written through the symbolic link {}",
                        path.display()
                    ),
                ))
            }
            _ => {}
        }
    }
    if !path.starts_with(base) {
        return Err(refuse_with(
            ERR_UNSAFE_PATH,
            format!("{relative:?} resolves outside {}", base.display()),
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tar::fixture::header;
    use crate::tar::parse_tar;

    const BLOCK: usize = 512;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "forge-market-unpack-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    fn simple_archive() -> Archive {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header("plugin.json", 2, b'0', 0o644));
        bytes.extend_from_slice(b"{}");
        bytes.extend_from_slice(&[0u8; BLOCK - 2]);
        bytes.extend_from_slice(&header("lib/", 0, b'5', 0o755));
        bytes.extend_from_slice(&header("lib/util.js", 3, b'0', 0o644));
        bytes.extend_from_slice(b"abc");
        bytes.extend_from_slice(&[0u8; BLOCK - 3]);
        bytes.extend_from_slice(&[0u8; BLOCK * 2]);
        parse_tar(&bytes).expect("parse the fixture archive")
    }

    #[test]
    fn files_and_directories_land_under_the_destination() {
        let dest = scratch("ok");
        let written = unpack(&simple_archive(), &dest).expect("unpack");
        assert_eq!(written.len(), 2);
        assert_eq!(written[0].path, "plugin.json");
        assert_eq!(written[0].sha256, sha256_hex(b"{}"));
        assert_eq!(
            fs::read(dest.join("lib/util.js")).expect("read"),
            b"abc".to_vec()
        );
        assert!(dest.join("lib").is_dir());
        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn an_existing_file_at_the_destination_is_refused() {
        let dest = scratch("file");
        fs::create_dir_all(dest.parent().expect("parent")).expect("parent");
        fs::write(&dest, b"not a directory").expect("write");
        let err = unpack(&simple_archive(), &dest).unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSAFE_PATH), "{err}");
        let _ = fs::remove_file(&dest);
    }

    #[test]
    fn a_dotdot_component_is_refused_even_though_the_reader_should_have_caught_it() {
        let base = std::env::temp_dir();
        let err = resolve_beneath(&base, "../outside").unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSAFE_PATH), "{err}");
        let err = resolve_beneath(&base, "a/../b").unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSAFE_PATH), "{err}");
    }

    #[test]
    fn a_plain_name_resolves_under_the_base() {
        let base = std::env::temp_dir();
        let resolved = resolve_beneath(&base, "a/b.txt").expect("resolve");
        assert!(resolved.starts_with(&base));
    }

    /// A symbolic link in the destination is the one escape no header check can
    /// see. Creating one needs a privilege Windows does not grant by default, so
    /// the assertion is skipped when the OS refuses to make the link; the same
    /// test asserts the refusal on any host that can.
    #[test]
    fn a_symbolic_link_in_the_destination_is_refused() {
        let dest = scratch("symlink");
        fs::create_dir_all(&dest).expect("create dest");
        let outside = scratch("symlink-outside");
        fs::create_dir_all(&outside).expect("create outside");
        let link = dest.join("link");

        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&outside, &link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&outside, &link);

        if made.is_err() {
            // No privilege: nothing to test on this host.
            let _ = fs::remove_dir_all(&dest);
            let _ = fs::remove_dir_all(&outside);
            return;
        }

        let base = fs::canonicalize(&dest).expect("canonicalize");
        let err = resolve_beneath(&base, "link/escaped.txt").unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNSAFE_PATH), "{err}");
        assert!(err.to_string().contains("symbolic link"), "{err}");
        let _ = fs::remove_dir_all(&dest);
        let _ = fs::remove_dir_all(&outside);
    }
}
