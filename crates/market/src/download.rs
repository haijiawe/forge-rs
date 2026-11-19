//! Fetching a tarball and proving it is the one the index named.
//!
//! The order matters and is the acceptance criterion: **download, then verify,
//! then hand the bytes on — never write an unverified byte into place.** A
//! mismatch deletes the file and reports both digests, so an operator can see
//! whether the index is wrong or the network is.
//!
//! Two sizes are checked against the index's `size` before the digest:
//!
//! * a body longer than the declared size is refused as
//!   [`ERR_BODY_TOO_LARGE`] — the server is trying to give us more than the index
//!   promised, and a cap derived from the declaration is what keeps an
//!   unexpected 2 GB body from being absorbed;
//! * a body shorter than the declared size is refused as [`ERR_TRUNCATED`],
//!   which is a different fault from a digest mismatch and worth a different
//!   message: a truncated body stops at a byte boundary, a tampered one does not.
//!
//! The client's own body cap cannot be raised per request, so a bundle larger
//! than the client's `max_bytes` is refused up front instead of being fetched and
//! then reported as truncated — the client would have cut the body at its cap and
//! this module could not tell that cut from a short answer.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use forge_core::{Error, ErrorKind, Result};
use forge_net::Client;

use crate::error::{
    invalid_with, refuse_with, ERR_BODY_TOO_LARGE, ERR_CHECKSUM_MISMATCH, ERR_CLIENT_CAP,
    ERR_HTTP_STATUS, ERR_TRUNCATED,
};
use crate::hash::sha256_hex;
use crate::index::Bundle;

/// Fetch `bundle`'s tarball into `dest_dir` and verify it.
///
/// Returns the path of the verified file. On any failure no file is left behind:
/// the file is written before it is verified (so the bytes on disk are the bytes
/// that were checked, not a second copy in memory), and a failed check removes
/// it.
pub fn download(client: &Client, bundle: &Bundle, dest_dir: &Path) -> Result<PathBuf> {
    let declared = usize::try_from(bundle.size).map_err(|_| {
        invalid_with(
            ERR_BODY_TOO_LARGE,
            format!(
                "bundle {} declares {} bytes, more than this process can hold",
                bundle.name, bundle.size
            ),
        )
    })?;
    let cap = client.options().max_bytes;
    if declared > cap {
        return Err(refuse_with(
            ERR_CLIENT_CAP,
            format!(
                "{} is {} bytes but the client caps a body at {cap}",
                bundle.name, declared
            ),
        ));
    }

    let response = client.get(&bundle.tarball)?;
    if !response.is_success() {
        return Err(Error::new(
            ErrorKind::Invalid,
            format!(
                "{}: {} for {}",
                ERR_HTTP_STATUS,
                response.error_summary(),
                bundle.tarball
            ),
        ));
    }
    // The client stops at its own cap and says so; with `declared <= cap` a
    // truncated read means the body is longer than the index promised.
    if response.truncated || response.body.len() > declared {
        return Err(invalid_with(
            ERR_BODY_TOO_LARGE,
            format!(
                "the index declares {declared} bytes, the response is {}",
                if response.truncated {
                    format!("more than {cap} (the client cap cut it)")
                } else {
                    format!("{}", response.body.len())
                }
            ),
        ));
    }
    if response.body.len() < declared {
        return Err(invalid_with(
            ERR_TRUNCATED,
            format!(
                "the index declares {declared} bytes, the response is {}",
                response.body.len()
            ),
        ));
    }
    // Content-Length is advisory and the client already read the body; it is
    // still worth reporting when the server contradicts itself, because that is
    // how a proxy that mangles a download is noticed before the digest fires.
    if let Some(declared_header) = response.header("content-length") {
        if let Ok(length) = declared_header.trim().parse::<usize>() {
            if length != response.body.len() {
                return Err(invalid_with(
                    ERR_TRUNCATED,
                    format!(
                        "Content-Length says {length}, the body is {} bytes",
                        response.body.len()
                    ),
                ));
            }
        }
    }

    fs::create_dir_all(dest_dir)?;
    let path = unique_path(dest_dir, &file_name_for(bundle));
    write(&path, &response.body)?;

    // Hash what is on disk, not the buffer: the file is what a later install
    // reads, and a short write on a full disk would otherwise slip through.
    let on_disk = fs::read(&path)?;
    let actual = sha256_hex(&on_disk);
    if actual != bundle.sha256 {
        // The file must not survive a failed verification: an unverified tarball
        // sitting in a directory is one `install` away from being trusted.
        let removed = fs::remove_file(&path);
        let note = match removed {
            Ok(()) => String::new(),
            Err(err) => format!(" (and it could not be removed: {err})"),
        };
        return Err(invalid_with(
            ERR_CHECKSUM_MISMATCH,
            format!(
                "{}: the index says {}, the bytes give {actual}{note}",
                bundle.name, bundle.sha256
            ),
        ));
    }
    Ok(path)
}

/// Write `bytes` to `path`, refusing a file that already exists.
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| {
            Error::new(
                ErrorKind::Io,
                format!("market: cannot create {}: {err}", path.display()),
            )
        })?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(())
}

/// Pick a name for the tarball: the URL's last segment, sanitised, or
/// `<name>-<version>.tar` when the URL does not end in a usable name.
///
/// Why not trust the URL's name as written: it is remote data, and while a
/// separator would not leave `dest_dir` (this is a single component), a name like
/// `..` or `CON` is still a name this crate should not create.
fn file_name_for(bundle: &Bundle) -> String {
    let without_query = bundle
        .tarball
        .split(['?', '#'])
        .next()
        .unwrap_or(&bundle.tarball);
    let candidate = without_query.rsplit('/').next().unwrap_or("").to_string();
    let safe = candidate.len() <= 100
        && !candidate.is_empty()
        && candidate != "."
        && candidate != ".."
        && candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
        && !candidate.starts_with('.');
    if safe {
        return candidate;
    }
    format!("{}-{}.tar", bundle.name, bundle.version)
}

/// A path in `dir` that does not exist yet.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    for attempt in 1..1000 {
        let candidate = dir.join(format!("{attempt}-{name}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{}-{name}", std::process::id()))
}

/// Verify an existing file against an index entry's digest and size.
///
/// Used by an install that reads a tarball the CLI downloaded earlier, so the
/// check happens on the bytes that will actually be unpacked rather than on a
/// copy of them.
pub fn verify_file(path: &Path, bundle: &Bundle) -> Result<()> {
    let meta = fs::metadata(path)?;
    if meta.len() != bundle.size {
        return Err(invalid_with(
            ERR_TRUNCATED,
            format!(
                "{}: the index declares {} bytes, {} is {}",
                bundle.name,
                bundle.size,
                path.display(),
                meta.len()
            ),
        ));
    }
    let bytes = fs::read(path)?;
    let actual = sha256_hex(&bytes);
    if actual != bundle.sha256 {
        return Err(invalid_with(
            ERR_CHECKSUM_MISMATCH,
            format!(
                "{}: the index says {}, the bytes give {actual}",
                bundle.name, bundle.sha256
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::sha256_hex;

    fn bundle(tarball: &str) -> Bundle {
        Bundle {
            name: "alpha".to_string(),
            version: "1.0.0".to_string(),
            description: "d".to_string(),
            author: None,
            keywords: None,
            homepage: None,
            tarball: tarball.to_string(),
            sha256: sha256_hex(b"x"),
            size: 1,
            engines: None,
            os: None,
            arch: None,
        }
    }

    #[test]
    fn a_url_name_is_used_when_it_is_safe() {
        assert_eq!(
            file_name_for(&bundle("https://example.invalid/a/b/alpha-1.0.0.tar.gz")),
            "alpha-1.0.0.tar.gz"
        );
    }

    #[test]
    fn a_url_name_with_a_query_is_still_used() {
        assert_eq!(
            file_name_for(&bundle("https://example.invalid/alpha.tar.gz?sig=abc")),
            "alpha.tar.gz"
        );
    }

    #[test]
    fn an_unusable_url_name_falls_back_to_the_bundle_identity() {
        for tarball in [
            "https://example.invalid/",
            "https://example.invalid/..",
            "https://example.invalid/.hidden",
            "https://example.invalid/a%20b.tar",
        ] {
            assert_eq!(
                file_name_for(&bundle(tarball)),
                "alpha-1.0.0.tar",
                "{tarball}"
            );
        }
    }

    #[test]
    fn a_unique_path_does_not_overwrite_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("forge-market-dl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create");
        fs::write(dir.join("alpha.tar"), b"first").expect("write");
        let second = unique_path(&dir, "alpha.tar");
        assert_ne!(second, dir.join("alpha.tar"));
        assert!(!second.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
