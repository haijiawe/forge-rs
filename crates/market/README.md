---
dir: crates/market
layer: L2
role: plugin market consumer — index fetch, resolve, checksummed download, tar/tar.gz unpack, install record
public: [Index, Bundle, Engines, Platform, host_platform, KNOWN_OS, KNOWN_ARCH, INDEX_VERSION, Store, InstalledSet, InstalledRecord, InstalledFile, install, install_bytes, uninstall, list, modified_files, extract_to, Installed, Removed, download, verify_file, open_tarball, parse_tar, Archive, Entry, MAX_ENTRIES, unpack, Unpacked, gunzip, gunzip_limited, is_gzip, inflate, inflate_from, sha256_hex, is_sha256_hex, digest_of_file, crc32, crc16_from_crc32, is_safe_name, is_safe_version, is_safe_relative_path, is_absolute_http_url, now_rfc3339, rfc3339_from_epoch, BUNDLES_DIR, RECORD_NAME, RECORD_VERSION, STAGING_DIR, DEFAULT_MAX_OUTPUT, DEFAULT_MAX_GZIP_OUTPUT]
may_import: [forge-core, forge-net]
must_not:
  - import any crate above L2
  - add flate2, tar or sha2; the budget answers to "why can the standard library not do this"
  - treat a checksum mismatch as a warning
  - write outside the destination directory during an unpack
  - keep an install that failed verification
gate: cargo test -p forge-market --offline
---

## 做什么

The consumer half of a plugin market. It fetches an `{"version":1,"bundles":[...]}` index, resolves a name and a requirement to exactly one `Bundle`, downloads that bundle's tarball over HTTP, proves its SHA-256 against the bytes actually received, unpacks it under `<root>/bundles/<name>/`, and records what it wrote in `<root>/installed.json` so an uninstall can delete exactly those files.

The parts that matter are the refusals, not the fetches:

- **A digest is checked after the bytes are on disk**, so the digest describes the file a later install reads, not a buffer that was written twice. A mismatch deletes the file and reports both digests; a short body is `truncated` with both sizes; a body longer than the index declares is refused rather than trimmed to fit.
- **Nothing is skipped on the way in.** The index is refused whole for a version this build does not read, a malformed `sha256`, a zero `size`, a `tarball` that is not an absolute http(s) URL, or two entries sharing name, version **and platform**. A semver range (`^1.2`, `1.x`, `>=1 <2`) is refused by name instead of approximated — half of semver is worse than none.
- **Nothing escapes the destination.** The tar reader refuses absolute paths, `..`, drive and stream prefixes, backslashes, symbolic links and every non-regular type; the writer re-checks containment component by component against the canonical destination, so a symlink planted in the destination is caught before a file lands below it.
- **An install is all-or-nothing.** Files are unpacked into `<root>/.staging/<name>-<pid>` and only moved into `<root>/bundles/<name>` after the tar parsed and every file hashed. A failed install leaves the root byte-identical.
- **An uninstall deletes only what the record lists.** A recorded file whose digest changed, a recorded file that vanished, or a file in the bundle directory the record never listed all stop the whole uninstall *before* anything is deleted, with the offending path named — a user's edit is not the market's to delete.

`open_tarball` is the one entry point most callers want: it sniffs the gzip magic and either gunzips then parses, or parses the `.tar` directly.

**A bundle may be platform-specific.** `os` and `arch` are optional members, but they travel together: both absent means a platform-independent source bundle (one tarball, every platform), both present means a native build for exactly that pair, and half-declared is refused rather than defaulted — "windows, any arch" is not something a compiler produces. The accepted spellings are the ones `.github/workflows/release.yml` publishes (`windows`/`linux`/`darwin`, `amd64`/`arm64`), not Rust's internal target names, so the market does not refuse the artifacts this project builds. The uniqueness key is name + version + os + arch, which is what lets six platform builds of one version be listed at all. `resolve` filters to the host platform before picking a version, so "no build for your platform" names the platforms that *are* published instead of masquerading as "no such version"; `resolve_for(name, requirement, &Platform)` takes the platform as a parameter so a test can ask what `linux/arm64` would get while running on Windows. When a source bundle and a native build both fit one platform, resolution refuses rather than guessing which the operator meant.

## 公开面

`Index::parse` / `resolve(name, requirement)` / `resolve_for(name, requirement, &Platform)` / `versions_of` / `validate`; `Bundle` (with `fits(&Platform)` and `platform_label()`) and `Engines` as the index's wire types; `Platform` / `host_platform()` / `KNOWN_OS` / `KNOWN_ARCH` for platform-aware resolution; `INDEX_VERSION`.

`download(&Client, &Bundle, dest_dir) -> Result<PathBuf>` and `verify_file(&Path, &Bundle)`.

`Store` (`new`, `record_path`, `bundles_dir`, `staging_dir`, `bundle_dir`, `load`, `save`, `list`, `get`, `ensure_absent`, `ensure_no_stray_directory`, `digest_of_file`), with `InstalledSet`, `InstalledRecord`, `InstalledFile`, and the constants `RECORD_NAME` / `RECORD_VERSION` / `BUNDLES_DIR`.

`install`, `install_bytes`, `uninstall`, `list`, `modified_files`, `extract_to`, returning `Installed` and `Removed`.

`open_tarball`, `parse_tar` with `Archive` / `Entry` / `MAX_ENTRIES`, `unpack` with `Unpacked`, `gunzip` / `is_gzip`, `inflate`.

`sha256_hex`, `is_sha256_hex`, `digest_of_file`, `crc32`, `now_rfc3339`, `rfc3339_from_epoch`, and the path/URL shape predicates `is_safe_name`, `is_safe_version`, `is_safe_relative_path`, `is_absolute_http_url`.

Every refusal prefix lives in `error` as a `pub const ERR_*`, so a caller (and a test) can name the rule it hit instead of matching on message text.

## 依赖与边界

`forge-core` for `Error`/`ErrorKind`/`Result`, `forge-net` for the HTTP client, and `ring` for SHA-256 — all three already in the budget, none of them added for this crate. `serde` and `serde_json` are here because the index and the record are JSON.

There is deliberately no `flate2`, no `tar` and no `sha2`. The DEFLATE decoder in `inflate` covers all three RFC 1951 block types (stored, fixed Huffman, dynamic Huffman), all 29 length codes and 30 distance codes, the repeat codes 16/17/18 in the code-length alphabet, and a bounded output buffer; it does not accept preset dictionaries (gzip has no such field) or trailing bytes after the final block. It is checked against vectors produced by a reference encoder, one of which is a real `tar -czf` blob from this machine with its digest pinned in the tests, so the dynamic-Huffman path — the one that tends to be silently wrong — is exercised against output no test writer made up. The tar reader is hand-written for the same reason: 512-byte headers, octal and GNU base-256 numbers, GNU `L` and pax `x` long names, ustar `prefix`, and both spellings of the header checksum.

HTTP is not re-implemented here. `forge_net::Client` owns redirects, TLS trust roots, the deadline and the body cap; this crate only decides what a status, a length and a digest mean. A non-2xx response is reported as a refusal naming the status, never retried and never treated as a transport failure.

The window is narrow on purpose: this crate does not publish an index, does not sign anything, and does not run what it installs. `engines` is recorded, not enforced.

## 不要做

- Do not add `flate2`, `tar`, `sha2` or an archive crate. The hand-written decoders are the point: one budget, one behaviour under audit, and tests that pin real streams.
- Do not turn a checksum mismatch into a warning, a retry, or an install. The whole crate exists to make that one case impossible.
- Do not delete a file the record does not list, and do not delete a listed file whose digest moved. Refusing is always the cheaper mistake.
- Do not resolve a semver range, and do not pick a version for a `*` that matches several. A wrong answer here is silent and a refusal is not.
- Do not default a half-declared platform, and do not pick between a source bundle and a native build that both fit one platform. The first makes an index mean different things on different machines; the second runs code the operator did not choose. Refuse and name the conflict.
- Do not move an install into place before the tar parsed and every file hashed; a half-installed bundle is worse than none.
- Do not write the install record and the files in a second pass. The record describes what was written, or it describes nothing.
