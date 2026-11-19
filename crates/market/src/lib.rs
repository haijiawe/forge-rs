//! The plugin market: fetch an index, resolve a name, download a tarball, prove
//! its digest, unpack it, and remember what was installed.
//!
//! This crate is the *consumer* side of a market — the index is an HTTP document
//! and a bundle is a tarball, but nothing here publishes either. It is the only
//! place in the tree that decides whether bytes from the network are the bytes a
//! publisher signed for, so the interesting code is the verification, not the
//! fetching.
//!
//! # The rules this crate refuses to bend
//!
//! * **A digest is checked against the bytes actually received**, after they are
//!   on disk, and a mismatch deletes the file and reports both digests. A
//!   tampered tarball is refused, not installed-with-a-warning.
//! * **Nothing is extracted before verification, and nothing is moved into place
//!   before extraction finishes.** An install is all-or-nothing; a failure leaves
//!   the root exactly as it was.
//! * **Every extracted path lands under the destination.** Refused at the reader
//!   (absolute, `..`, drive prefixes, non-file types) and again at the writer
//!   (symbolic links in the destination, canonical containment). Two checks that
//!   are not the same check.
//! * **Uninstall deletes only what the record lists, and only if it is unchanged.**
//!   A user's edit is data loss, so a changed file stops the whole uninstall.
//! * **An unknown version is refused, never guessed.** Index generation 1 and
//!   record generation 1 are the only ones this build reads; a different one is
//!   an error with its number in it.
//!
//! # Dependency budget
//!
//! `flate2` and `tar` are not in it, so [`inflate`] is a hand-written RFC 1951
//! decoder (stored, fixed-Huffman and dynamic-Huffman blocks) and [`tar`] is a
//! hand-written reader. `ring` supplies SHA-256, which is already in the budget
//! for TLS. The tests are what keep that honest: the vectors in [`inflate`] are
//! pinned from a reference implementation, and one real `tar -czf` blob produced
//! on this machine is embedded with its digest pinned.
//!
//! # Layout of a market root
//!
//! ```text
//! <root>/installed.json     the record: name, version, digest, files+digests
//! <root>/bundles/<name>/    one directory per installed bundle
//! <root>/.staging/          where an install is assembled before it moves
//! ```

#![forbid(unsafe_code)]

pub mod crc32;
pub mod download;
pub mod error;
pub mod gzip;
pub mod hash;
pub mod index;
pub mod inflate;
pub mod install;
pub mod store;
pub mod tar;
pub mod unpack;

pub use download::{download, verify_file};
pub use error::*;
pub use gzip::{gunzip, is_gzip};
pub use hash::{is_sha256_hex, sha256_hex};
pub use index::{
    host_platform, Bundle, Engines, Index, Platform, INDEX_VERSION, KNOWN_ARCH, KNOWN_OS,
};
pub use inflate::inflate;
pub use install::{
    extract_to, install, install_bytes, list, modified_files, uninstall, Installed, Removed,
};
pub use store::{
    InstalledFile, InstalledRecord, InstalledSet, Store, BUNDLES_DIR, RECORD_NAME, RECORD_VERSION,
};
pub use tar::{parse_tar, Archive, Entry, MAX_ENTRIES};
pub use unpack::{unpack, Unpacked};

/// Parse a tarball into an archive, decompressing it first when it is a gzip
/// member. The one entry point most callers want after a download.
pub fn open_tarball(bytes: &[u8]) -> forge_core::Result<Archive> {
    if is_gzip(bytes) {
        parse_tar(&gunzip(bytes)?)
    } else {
        parse_tar(bytes)
    }
}
