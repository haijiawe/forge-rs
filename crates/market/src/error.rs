//! Every refusal this crate can make, in one place.
//!
//! Why one place: a refusal is a contract with the operator. `market install`
//! that stops has to say *which* rule it hit, and a test that only asserts "an
//! error happened" would pass for a typo in an unrelated branch. Each constant
//! below is the prefix of at least one message and the anchor of at least one
//! assertion, so rewriting a reason is a visible, test-breaking act.
//!
//! The alternative — one `ErrorKind` per rule — was rejected because
//! `hearth_core::ErrorKind` is the vocabulary the CLI and the HTTP envelope
//! already speak; adding `ChecksumMismatch` there would make every other crate
//! carry a word it never uses.

use hearth_core::{Error, ErrorKind};

// ---- the index -----------------------------------------------------------

/// A different index generation is refused, not guessed at.
pub const ERR_INDEX_VERSION: &str = "market: index version is not 1";
/// A bundle name becomes a directory, so it must already be safe as one.
pub const ERR_BUNDLE_NAME: &str = "market: bundle name is not a safe directory name";
/// The version is opaque text, but it is also used in file names.
pub const ERR_BUNDLE_VERSION: &str = "market: bundle version is not a plain version string";
pub const ERR_SHA256_SHAPE: &str = "market: sha256 is not 64 lowercase hex characters";
pub const ERR_SIZE: &str = "market: bundle size must be greater than zero";
pub const ERR_TARBALL_URL: &str = "market: tarball is not an absolute http(s) URL";
pub const ERR_DUPLICATE_BUNDLE: &str =
    "market: two bundles share the same name, version and platform";
pub const ERR_BUNDLE_PLATFORM: &str = "market: a bundle must declare both os and arch, or neither";
pub const ERR_UNKNOWN_OS: &str = "market: bundle os is not one this build knows";
pub const ERR_UNKNOWN_ARCH: &str = "market: bundle arch is not one this build knows";
pub const ERR_NO_BUILD_FOR_PLATFORM: &str = "market: the index has no build for this platform";
pub const ERR_AMBIGUOUS_PLATFORM: &str =
    "market: several bundles match this name, version and platform";
pub const ERR_UNKNOWN_NAME: &str = "market: the index has no bundle with that name";
pub const ERR_UNKNOWN_VERSION: &str = "market: the index has no such version for that bundle";
pub const ERR_RANGE_REQUIREMENT: &str = "market: version ranges are not supported";
pub const ERR_AMBIGUOUS_STAR: &str = "market: '*' matches more than one version";

// ---- download and verification ------------------------------------------

/// The cap lives on the `Client`, not on the request, so a bundle bigger than
/// the client's cap cannot be fetched honestly: the client would cut the body
/// and the length check below could not tell a cut from a short answer.
pub const ERR_CLIENT_CAP: &str = "market: the bundle is larger than the client body cap";
pub const ERR_HTTP_STATUS: &str = "market: the tarball request did not succeed";
pub const ERR_BODY_TOO_LARGE: &str = "market: the response body is larger than the declared size";
pub const ERR_TRUNCATED: &str = "market: the response body is shorter than the declared size";
pub const ERR_CHECKSUM_MISMATCH: &str = "market: sha256 does not match the index";

// ---- DEFLATE and gzip ----------------------------------------------------

pub const ERR_DEFLATE_TRUNCATED: &str = "market: deflate stream ends early";
pub const ERR_DEFLATE_RESERVED: &str = "market: deflate block type 3 is reserved";
pub const ERR_DEFLATE_STORED: &str =
    "market: deflate stored block LEN does not match its complement";
pub const ERR_DEFLATE_HUFFMAN: &str = "market: deflate Huffman code is invalid";
pub const ERR_DEFLATE_CODE_LENGTHS: &str =
    "market: deflate code lengths are over-subscribed or out of range";
pub const ERR_DEFLATE_DISTANCE: &str = "market: deflate distance is too far back";
pub const ERR_DEFLATE_ENDOFBLOCK: &str = "market: deflate literal tree has no end-of-block code";
pub const ERR_OUTPUT_LIMIT: &str = "market: decompressed output exceeds the limit";
pub const ERR_GZIP_MAGIC: &str = "market: not a gzip stream";
pub const ERR_GZIP_METHOD: &str = "market: gzip compression method is not deflate";
pub const ERR_GZIP_FLAGS: &str = "market: gzip header has reserved flag bits set";
pub const ERR_GZIP_HEADER: &str = "market: gzip header is truncated";
pub const ERR_GZIP_HCRC: &str = "market: gzip header CRC16 does not match";
pub const ERR_GZIP_CRC: &str = "market: gzip CRC32 does not match the decompressed bytes";
pub const ERR_GZIP_SIZE: &str = "market: gzip ISIZE does not match the decompressed length";
pub const ERR_GZIP_TRAILING: &str = "market: gzip stream has bytes after its footer";

// ---- tar ----------------------------------------------------------------

pub const ERR_TAR_HEADER: &str = "market: tar header is truncated";
pub const ERR_TAR_TRAILING: &str = "market: tar has bytes after the end-of-archive marker";
pub const ERR_TAR_CHECKSUM: &str = "market: tar header checksum does not match";
pub const ERR_TAR_TRUNCATED: &str = "market: tar entry data is truncated";
pub const ERR_TAR_OCTAL: &str = "market: tar numeric field is not a valid octal number";
pub const ERR_TAR_PAX: &str = "market: pax extended header is malformed";
pub const ERR_UNSAFE_PATH: &str = "market: tar entry path escapes the destination";
pub const ERR_UNSUPPORTED_ENTRY: &str = "market: tar entry type is not supported";
pub const ERR_DUPLICATE_ENTRY: &str = "market: tar contains the same path twice";
pub const ERR_ENTRY_LIMIT: &str = "market: tar has more entries than the limit";
pub const ERR_EMPTY_BUNDLE: &str = "market: the tarball holds no files";

// ---- the installed set --------------------------------------------------

pub const ERR_RECORD_VERSION: &str = "market: installed.json version is not 1";
pub const ERR_RECORD_CORRUPT: &str = "market: installed.json does not match the record shape";
pub const ERR_ALREADY_INSTALLED: &str = "market: that bundle is already installed";
pub const ERR_NOT_INSTALLED: &str = "market: that bundle is not installed";
pub const ERR_FILE_MODIFIED: &str = "market: a recorded file changed since it was installed";
pub const ERR_EXTRA_FILES: &str =
    "market: the bundle directory holds files the record does not list";
pub const ERR_TARGET_EXISTS: &str = "market: the bundle directory exists but no record lists it";

// ---- packing (the producer half) ----------------------------------------

pub const ERR_PACK_SOURCE: &str = "market: pack source is not a readable directory";
pub const ERR_PACK_EMPTY: &str = "market: pack source holds no files";
pub const ERR_PACK_PATH: &str = "market: pack entry path cannot be stored safely";
pub const ERR_PACK_SYMLINK: &str = "market: pack refuses to follow a symbolic link";
pub const ERR_PACK_SIZE: &str = "market: pack entry is too large for a ustar header";

/// The body of a refusal, with the concrete value appended.
pub(crate) fn refuse_with(reason: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Refused, format!("{reason}: {detail}"))
}

/// Bytes that were not what they claimed to be.
pub(crate) fn invalid_with(reason: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Invalid, format!("{reason}: {detail}"))
}

/// The request contradicts what exists (already installed, file edited).
pub(crate) fn conflict_with(reason: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Conflict, format!("{reason}: {detail}"))
}

/// The named thing is not there.
pub(crate) fn not_found_with(reason: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::NotFound, format!("{reason}: {detail}"))
}

/// A record written by a generation this build does not read.
pub(crate) fn unsupported_with(reason: &str, detail: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::UnsupportedVersion, format!("{reason}: {detail}"))
}
