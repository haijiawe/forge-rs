//! The index document: `{"version":1,"bundles":[...]}`.
//!
//! Why this is parsed strictly rather than leniently: an index is the only thing
//! that tells an install which bytes are correct. A missing `sha256` that
//! defaulted to "" would make every tarball match the empty digest's complement
//! — that is, nothing verifies and every install succeeds. So every field the
//! verifier depends on is required, and a document that violates the shape is
//! refused as a whole with the index number of the offending bundle.
//!
//! Version 1 is the only generation this build reads. A version 2 index is
//! refused by number instead of being read for the fields this build happens to
//! understand: the fields it does not understand are exactly the ones that would
//! change what "verified" means.

use hearth_core::Result;
use serde::{Deserialize, Serialize};

use crate::error::{
    invalid_with, refuse_with, unsupported_with, ERR_AMBIGUOUS_PLATFORM, ERR_AMBIGUOUS_STAR,
    ERR_BUNDLE_NAME, ERR_BUNDLE_PLATFORM, ERR_BUNDLE_VERSION, ERR_DUPLICATE_BUNDLE,
    ERR_INDEX_VERSION, ERR_NO_BUILD_FOR_PLATFORM, ERR_RANGE_REQUIREMENT, ERR_SHA256_SHAPE,
    ERR_SIZE, ERR_TARBALL_URL, ERR_UNKNOWN_ARCH, ERR_UNKNOWN_NAME, ERR_UNKNOWN_OS,
    ERR_UNKNOWN_VERSION,
};
use crate::hash::is_sha256_hex;

/// The only index generation this build reads.
pub const INDEX_VERSION: u32 = 1;

/// The `os` values an index may declare.
///
/// These are the spellings `.github/workflows/release.yml` publishes, not the
/// ones Rust uses internally: `darwin`, not `macos`. A closed set spelled
/// differently from the release matrix would refuse the very artifacts this
/// project builds.
pub const KNOWN_OS: [&str; 3] = ["windows", "linux", "darwin"];

/// The `arch` values an index may declare, again matching the release matrix.
pub const KNOWN_ARCH: [&str; 2] = ["amd64", "arm64"];

/// A platform an install can ask for.
///
/// It is a parameter rather than something read from `cfg!` inside the resolver
/// so a test can ask about a platform it is not running on. A resolver that could
/// only be tested on the host would leave five of the six release targets
/// untested, and "which bundle does linux/arm64 get" is exactly the question a
/// platform-aware index exists to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
}

impl Platform {
    pub const fn new(os: &'static str, arch: &'static str) -> Platform {
        Platform { os, arch }
    }

    /// How this platform is written in a message.
    pub fn label(&self) -> String {
        format!("{}/{}", self.os, self.arch)
    }
}

/// The platform this binary was built for.
pub fn host_platform() -> Platform {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        // An OS outside the release matrix. It stays a real string rather than
        // being mapped onto a neighbour: a bundle for a platform we do not build
        // must not be silently offered to one we cannot name.
        "unknown"
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "unknown"
    };
    Platform::new(os, arch)
}

/// One installable bundle, as the index describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Absolute http(s) URL of the tarball.
    pub tarball: String,
    /// Lowercase hex SHA-256 of the tarball, 64 characters.
    pub sha256: String,
    /// The exact byte length of the tarball.
    pub size: u64,
    /// Why the bundle needs a runtime the host may not have. Recorded, not
    /// enforced: the market installs files, it does not run them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engines: Option<Engines>,
    /// The operating system these bytes were built for, from [`KNOWN_OS`].
    ///
    /// Absent on **both** `os` and `arch` means the bundle is platform-
    /// independent: a source plugin whose interpreter exists everywhere. Present
    /// on both means a native build, and then it is one of several entries for the
    /// same name and version. Half-declared is refused rather than guessed —
    /// "windows, any arch" is not a thing a compiler produces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// The architecture these bytes were built for, from [`KNOWN_ARCH`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

impl Bundle {
    /// Is this bundle usable on `platform`?
    ///
    /// A platform-independent bundle fits everywhere; a native one fits only its
    /// own pair. This is the whole reason the fields exist: without them a native
    /// bundle would be offered to every platform and fail at run time instead of
    /// at resolve time.
    pub fn fits(&self, platform: &Platform) -> bool {
        match (&self.os, &self.arch) {
            (None, None) => true,
            (Some(os), Some(arch)) => os == platform.os && arch == platform.arch,
            // Refused by `validate`, so an index that reaches a caller cannot
            // contain this. Treating it as "does not fit" keeps a hand-built
            // Bundle from being offered to a platform it never named.
            _ => false,
        }
    }

    /// How this bundle's platform is written in a message.
    pub fn platform_label(&self) -> String {
        match (&self.os, &self.arch) {
            (Some(os), Some(arch)) => format!("{os}/{arch}"),
            _ => "any platform".to_string(),
        }
    }
}

/// The `engines` object: factual constraints from the publisher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Engines {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hearth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// A parsed index document.
#[derive(Debug, Clone)]
pub struct Index {
    pub bundles: Vec<Bundle>,
}

/// The wire shape, kept private so the validated type is the only way in.
///
/// There is deliberately no `version` field: `Index::parse` reads it off the raw
/// tree first (so a v2 document is refused for its version rather than for
/// whichever member this build does not know), and a second declaration here
/// would be a field nothing reads.
#[derive(Deserialize)]
struct RawIndex {
    bundles: Vec<Bundle>,
}

impl Index {
    /// Parse and validate an index document.
    pub fn parse(bytes: &[u8]) -> Result<Index> {
        // The version is checked on a raw tree *before* the bundles are decoded,
        // so a v2 document is refused for its version rather than for whichever
        // member this build's struct happens not to know.
        let tree: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| {
            invalid_with(
                ERR_INDEX_VERSION,
                format!("the document is not JSON: {err}"),
            )
        })?;
        match tree.get("version").and_then(serde_json::Value::as_u64) {
            Some(version) if version == u64::from(INDEX_VERSION) => {}
            Some(version) => {
                return Err(unsupported_with(
                    ERR_INDEX_VERSION,
                    format!("got version {version}, this build reads {INDEX_VERSION}"),
                ))
            }
            None => {
                return Err(unsupported_with(
                    ERR_INDEX_VERSION,
                    "the document has no numeric \"version\" member",
                ))
            }
        }

        let raw: RawIndex = serde_json::from_value(tree)
            .map_err(|err| invalid_with(ERR_INDEX_VERSION, format!("unexpected shape: {err}")))?;

        let mut index = Index {
            bundles: raw.bundles,
        };
        index.validate()?;
        Ok(index)
    }

    /// The checks [`Index::parse`] makes, split out so a caller that built an
    /// `Index` by hand (a test, a mirrored index) can run them too.
    pub fn validate(&mut self) -> Result<()> {
        for (position, bundle) in self.bundles.iter_mut().enumerate() {
            validate_bundle(bundle, position)?;
        }
        // The key includes the platform, so six builds of one version are six
        // distinct entries rather than five duplicates. Two entries for the same
        // platform are still a duplicate: a resolver would have to pick one, and
        // picking would mean this crate has an opinion it does not have.
        for (position, bundle) in self.bundles.iter().enumerate() {
            for other in &self.bundles[position + 1..] {
                if other.name == bundle.name
                    && other.version == bundle.version
                    && other.os == bundle.os
                    && other.arch == bundle.arch
                {
                    return Err(refuse_with(
                        ERR_DUPLICATE_BUNDLE,
                        format!(
                            "bundles[{position}] and a later entry are {}/{} for {}",
                            bundle.name,
                            bundle.version,
                            bundle.platform_label()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Every bundle with this name, newest-first is not defined: a market index
    /// has no ordering contract, so the caller sees the document order.
    pub fn versions_of(&self, name: &str) -> Vec<&Bundle> {
        self.bundles
            .iter()
            .filter(|bundle| bundle.name == name)
            .collect()
    }

    /// Resolve a name and a requirement to exactly one bundle.
    ///
    /// Supported requirements:
    ///   * `*` — the only version of that name, or nothing. An index with
    ///     several versions and a `*` requirement is *refused*, not resolved to
    ///     the first: picking one would mean this crate has a version-ordering
    ///     opinion, and the whole point of refusing ranges is not to have one.
    ///   * an exact version — `0.3.1`, `2026.11`, `v2`. Compared as text, which
    ///     is why a version that is not plain text is refused at parse time.
    ///
    /// Everything else — `^1.2`, `~1`, `>=1 <2`, `1.x`, `1.2.*` — is refused by
    /// name. Half of semver is worse than none: a resolver that understands
    /// `^1.2.3` but not prerelease ordering answers some questions wrongly and
    /// cannot be trusted with any.
    pub fn resolve(&self, name: &str, requirement: &str) -> Result<&Bundle> {
        self.resolve_for(name, requirement, &host_platform())
    }

    /// [`Index::resolve`] for a named platform.
    ///
    /// The platform is a parameter so a test can ask what `linux/arm64` would get
    /// while running on Windows. Reading `cfg!` inside the resolver would leave
    /// five of the six release targets untestable.
    pub fn resolve_for(
        &self,
        name: &str,
        requirement: &str,
        platform: &Platform,
    ) -> Result<&Bundle> {
        let requirement = requirement.trim();
        if requirement.is_empty() {
            return Err(refuse_with(
                ERR_RANGE_REQUIREMENT,
                "the requirement is empty; use an exact version or \"*\"",
            ));
        }
        let all: Vec<&Bundle> = self.versions_of(name);
        if all.is_empty() {
            return Err(hearth_core::Error::new(
                hearth_core::ErrorKind::NotFound,
                format!("{}: {name:?}", ERR_UNKNOWN_NAME),
            ));
        }
        // Filter before resolving, so "no such version" never means "we do not
        // ship your platform". Those are different problems with different fixes.
        let candidates: Vec<&Bundle> = all
            .iter()
            .copied()
            .filter(|bundle| bundle.fits(platform))
            .collect();
        if candidates.is_empty() {
            return Err(refuse_with(
                ERR_NO_BUILD_FOR_PLATFORM,
                format!(
                    "{name:?} is published for {}, not for {}",
                    all.iter()
                        .map(|bundle| bundle.platform_label())
                        .collect::<Vec<_>>()
                        .join(", "),
                    platform.label()
                ),
            ));
        }

        if requirement == "*" {
            // Distinct versions, not entries: six platform builds of one version
            // are one version, and only one of them fits this platform anyway.
            let mut versions: Vec<&str> = candidates
                .iter()
                .map(|bundle| bundle.version.as_str())
                .collect();
            versions.sort_unstable();
            versions.dedup();
            if versions.len() > 1 {
                return Err(refuse_with(
                    ERR_AMBIGUOUS_STAR,
                    format!("{name:?} has versions {}; name one", versions.join(", ")),
                ));
            }
            return match candidates.len() {
                1 => Ok(candidates[0]),
                // One version, two fitting entries: a source bundle and a native
                // build for this same platform. Choosing between them is a
                // publisher's decision, not a resolver's guess.
                _ => Err(refuse_with(
                    ERR_AMBIGUOUS_PLATFORM,
                    format!(
                        "{name:?} {} has {} entries that fit {}; the index must name one",
                        versions[0],
                        candidates.len(),
                        platform.label()
                    ),
                )),
            };
        }

        if is_range(requirement) {
            return Err(refuse_with(
                ERR_RANGE_REQUIREMENT,
                format!(
                    "{requirement:?} is a range; this build resolves exact versions and \"*\" only"
                ),
            ));
        }

        let matching: Vec<&Bundle> = candidates
            .iter()
            .copied()
            .filter(|bundle| bundle.version == requirement)
            .collect();
        match matching.len() {
            1 => Ok(matching[0]),
            0 => {
                // The version may exist for other platforms. Say which, because
                // "no such version" would send the operator looking for a typo
                // that is not there.
                let elsewhere: Vec<String> = all
                    .iter()
                    .filter(|bundle| bundle.version == requirement)
                    .map(|bundle| bundle.platform_label())
                    .collect();
                if !elsewhere.is_empty() {
                    return Err(refuse_with(
                        ERR_NO_BUILD_FOR_PLATFORM,
                        format!(
                            "{name:?} {requirement} is published for {}, not for {}",
                            elsewhere.join(", "),
                            platform.label()
                        ),
                    ));
                }
                let mut versions: Vec<&str> = candidates
                    .iter()
                    .map(|bundle| bundle.version.as_str())
                    .collect();
                versions.sort_unstable();
                versions.dedup();
                Err(hearth_core::Error::new(
                    hearth_core::ErrorKind::NotFound,
                    format!(
                        "{}: {name:?} has {}",
                        ERR_UNKNOWN_VERSION,
                        versions.join(", ")
                    ),
                ))
            }
            _ => Err(refuse_with(
                ERR_AMBIGUOUS_PLATFORM,
                format!(
                    "{name:?} {requirement} has {} entries that fit {}; the index must name one",
                    matching.len(),
                    platform.label()
                ),
            )),
        }
    }
}

/// Does this requirement contain range syntax?
///
/// The list is the semver operators people actually type. It is deliberately
/// over-inclusive: `1.x` and `1.*` are not valid semver ranges but they are
/// certainly not exact versions, and refusing a typo with "ranges are not
/// supported" is a better message than "no such version".
fn is_range(requirement: &str) -> bool {
    const OPERATORS: [&str; 6] = ["^", "~", ">", "<", "=", ","];
    if OPERATORS.iter().any(|op| requirement.contains(op)) {
        return true;
    }
    // `1.*` / `1.x` / `1.X` and the hyphen range `1.0.0 - 2.0.0`.
    let lower = requirement.to_ascii_lowercase();
    lower.contains(".*") || lower.contains(".x") || lower.split_whitespace().count() > 1
}

/// One bundle's checks. The `position` is for the message, so an operator
/// fixing a published index knows which line to look at.
fn validate_bundle(bundle: &mut Bundle, position: usize) -> Result<()> {
    if !is_safe_name(&bundle.name) {
        return Err(refuse_with(
            ERR_BUNDLE_NAME,
            format!(
                "bundles[{position}].name {:?} must match [a-z0-9][a-z0-9._-]* so it can be a directory",
                bundle.name
            ),
        ));
    }
    if !is_safe_version(&bundle.version) {
        return Err(refuse_with(
            ERR_BUNDLE_VERSION,
            format!(
                "bundles[{position}].version {:?} must match [A-Za-z0-9][A-Za-z0-9._+-]* so it can be a directory",
                bundle.version
            ),
        ));
    }
    if bundle.description.trim().is_empty() {
        return Err(invalid_with(
            ERR_INDEX_VERSION,
            format!("bundles[{position}].description is empty"),
        ));
    }
    if !is_sha256_hex(&bundle.sha256) {
        return Err(invalid_with(
            ERR_SHA256_SHAPE,
            format!("bundles[{position}] has {:?}", bundle.sha256),
        ));
    }
    if bundle.size == 0 {
        return Err(invalid_with(
            ERR_SIZE,
            format!("bundles[{position}] declares size 0"),
        ));
    }
    if !is_absolute_http_url(&bundle.tarball) {
        return Err(invalid_with(
            ERR_TARBALL_URL,
            format!("bundles[{position}] has {:?}", bundle.tarball),
        ));
    }
    match (&bundle.os, &bundle.arch) {
        (None, None) => {}
        (Some(os), Some(arch)) => {
            if !KNOWN_OS.contains(&os.as_str()) {
                return Err(refuse_with(
                    ERR_UNKNOWN_OS,
                    format!(
                        "bundles[{position}].os {os:?} is not one of {}",
                        KNOWN_OS.join(", ")
                    ),
                ));
            }
            if !KNOWN_ARCH.contains(&arch.as_str()) {
                return Err(refuse_with(
                    ERR_UNKNOWN_ARCH,
                    format!(
                        "bundles[{position}].arch {arch:?} is not one of {}",
                        KNOWN_ARCH.join(", ")
                    ),
                ));
            }
        }
        // Half a platform is not a platform. Defaulting the missing half to the
        // host's would make an index mean different things on different machines.
        _ => {
            return Err(refuse_with(
                ERR_BUNDLE_PLATFORM,
                format!(
                    "bundles[{position}] declares os={:?} arch={:?}; declare both or neither",
                    bundle.os, bundle.arch
                ),
            ))
        }
    }
    Ok(())
}

/// A bundle name becomes `<root>/bundles/<name>`; the rule is the same one
/// `hearth-skill` and the plugin protocol use for a name that becomes a path.
pub fn is_safe_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= 64
        && bytes.all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-')
        })
        // A name of dots would be `.` or `..` as a directory.
        && !name.bytes().all(|byte| byte == b'.')
}

/// A version becomes a directory name too, but it must not be able to escape:
/// no slashes, no `..`, and it must start with something other than a dot.
pub fn is_safe_version(version: &str) -> bool {
    if version.is_empty() || version.len() > 64 || version.starts_with('.') {
        return false;
    }
    version
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

/// Is this an absolute `http://` or `https://` URL with a host?
///
/// The check is here rather than left to the HTTP client because the index is
/// *data*: a `tarball` of `file:///etc/passwd` must be refused while the index
/// is being read, not when a download later fails with a less honest message.
/// The client's own parser is the authority on what it will accept; this is the
/// early, cheap subset check.
pub fn is_absolute_http_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let rest = match lower.strip_prefix("https://") {
        Some(rest) => rest,
        None => match lower.strip_prefix("http://") {
            Some(rest) => rest,
            None => return false,
        },
    };
    // A host, and no whitespace anywhere.
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    !authority.is_empty()
        && !authority.starts_with('.')
        && !text.contains(char::is_whitespace)
        && !text.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::sha256_hex;

    fn valid_bundle(name: &str, version: &str) -> Bundle {
        Bundle {
            name: name.to_string(),
            version: version.to_string(),
            description: "a test bundle".to_string(),
            author: Some("someone".to_string()),
            keywords: Some(vec!["test".to_string()]),
            homepage: None,
            tarball: format!("https://example.invalid/{name}-{version}.tar.gz"),
            sha256: sha256_hex(b"pretend"),
            size: 1234,
            engines: None,
            os: None,
            arch: None,
        }
    }

    fn document(bundles: &[Bundle]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"version": 1, "bundles": bundles}))
            .expect("encode the test document")
    }

    #[test]
    fn a_well_formed_index_parses() {
        let index = Index::parse(&document(&[valid_bundle("alpha", "0.1.0")])).expect("parse");
        assert_eq!(index.bundles.len(), 1);
        assert_eq!(index.bundles[0].name, "alpha");
        assert_eq!(index.bundles[0].author.as_deref(), Some("someone"));
    }

    #[test]
    fn optional_members_may_be_absent() {
        let text = format!(
            r#"{{"version":1,"bundles":[{{"name":"a","version":"1","description":"d","tarball":"https://x.invalid/a.tar","sha256":"{}","size":1}}]}}"#,
            sha256_hex(b"x")
        );
        let index = Index::parse(text.as_bytes()).expect("parse");
        assert!(index.bundles[0].author.is_none());
        assert!(index.bundles[0].keywords.is_none());
        assert!(index.bundles[0].engines.is_none());
    }

    #[test]
    fn a_different_index_version_is_refused_by_number() {
        let text = r#"{"version":2,"bundles":[]}"#;
        let err = Index::parse(text.as_bytes()).unwrap_err();
        assert!(err.to_string().starts_with(ERR_INDEX_VERSION), "{err}");
        assert_eq!(err.kind(), hearth_core::ErrorKind::UnsupportedVersion);
    }

    #[test]
    fn an_index_without_a_version_is_refused() {
        let err = Index::parse(br#"{"bundles":[]}"#).unwrap_err();
        assert!(err.to_string().starts_with(ERR_INDEX_VERSION), "{err}");
    }

    #[test]
    fn an_index_that_is_not_json_is_refused() {
        let err = Index::parse(b"<html>not an index</html>").unwrap_err();
        assert!(err.to_string().starts_with(ERR_INDEX_VERSION), "{err}");
    }

    #[test]
    fn a_sha256_that_is_not_64_lowercase_hex_is_refused() {
        for bad in [
            "",
            "deadbeef",
            "0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
        ] {
            let mut bundle = valid_bundle("alpha", "1");
            bundle.sha256 = bad.to_string();
            let err = Index::parse(&document(&[bundle])).unwrap_err();
            assert!(
                err.to_string().starts_with(ERR_SHA256_SHAPE),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn a_zero_size_is_refused() {
        let mut bundle = valid_bundle("alpha", "1");
        bundle.size = 0;
        let err = Index::parse(&document(&[bundle])).unwrap_err();
        assert!(err.to_string().starts_with(ERR_SIZE), "{err}");
    }

    #[test]
    fn a_tarball_url_that_is_not_http_is_refused() {
        for bad in [
            "file:///etc/passwd",
            "/absolute/path.tar",
            "relative.tar",
            "ftp://example.invalid/x.tar",
            "https://",
            "http://host/with space",
        ] {
            let mut bundle = valid_bundle("alpha", "1");
            bundle.tarball = bad.to_string();
            let err = Index::parse(&document(&[bundle])).unwrap_err();
            assert!(
                err.to_string().starts_with(ERR_TARBALL_URL),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn a_name_that_could_not_be_a_directory_is_refused() {
        for bad in ["", "Alpha", "a/b", "..", ".", "a b", "-leading", "a\\b"] {
            let mut bundle = valid_bundle("alpha", "1");
            bundle.name = bad.to_string();
            let err = Index::parse(&document(&[bundle])).unwrap_err();
            assert!(
                err.to_string().starts_with(ERR_BUNDLE_NAME),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn a_version_that_could_not_be_a_directory_is_refused() {
        for bad in ["", "../1", "1/2", "..", ".hidden", "1 2"] {
            let mut bundle = valid_bundle("alpha", "1");
            bundle.version = bad.to_string();
            let err = Index::parse(&document(&[bundle])).unwrap_err();
            assert!(
                err.to_string().starts_with(ERR_BUNDLE_VERSION),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn the_same_name_and_version_twice_is_refused() {
        let index = document(&[valid_bundle("alpha", "1"), valid_bundle("alpha", "1")]);
        let err = Index::parse(&index).unwrap_err();
        assert!(err.to_string().starts_with(ERR_DUPLICATE_BUNDLE), "{err}");
    }

    #[test]
    fn the_same_name_with_two_versions_is_fine() {
        let index = Index::parse(&document(&[
            valid_bundle("alpha", "1.0.0"),
            valid_bundle("alpha", "1.1.0"),
        ]))
        .expect("parse");
        assert_eq!(index.versions_of("alpha").len(), 2);
    }

    #[test]
    fn resolve_takes_an_exact_version() {
        let index = Index::parse(&document(&[
            valid_bundle("alpha", "1.0.0"),
            valid_bundle("alpha", "1.1.0"),
        ]))
        .expect("parse");
        let bundle = index.resolve("alpha", "1.1.0").expect("resolve");
        assert_eq!(bundle.version, "1.1.0");
    }

    #[test]
    fn resolve_star_works_when_there_is_exactly_one_version() {
        let index = Index::parse(&document(&[valid_bundle("alpha", "1.0.0")])).expect("parse");
        let bundle = index.resolve("alpha", "*").expect("resolve");
        assert_eq!(bundle.version, "1.0.0");
    }

    #[test]
    fn resolve_star_refuses_to_pick_among_several() {
        let index = Index::parse(&document(&[
            valid_bundle("alpha", "1.0.0"),
            valid_bundle("alpha", "1.1.0"),
        ]))
        .expect("parse");
        let err = index.resolve("alpha", "*").unwrap_err();
        assert!(err.to_string().starts_with(ERR_AMBIGUOUS_STAR), "{err}");
        assert!(err.to_string().contains("1.0.0"), "{err}");
        assert!(err.to_string().contains("1.1.0"), "{err}");
    }

    #[test]
    fn every_range_syntax_is_refused_by_name() {
        let index = Index::parse(&document(&[valid_bundle("alpha", "1.2.3")])).expect("parse");
        for requirement in [
            "^1.2.3",
            "~1.2",
            ">=1.0.0",
            "<2.0.0",
            "=1.2.3",
            ">1",
            "1.*",
            "1.x",
            "1.0.0 - 2.0.0",
            "1.0.0 2.0.0",
            "",
        ] {
            let err = index.resolve("alpha", requirement).unwrap_err();
            assert!(
                err.to_string().starts_with(ERR_RANGE_REQUIREMENT),
                "{requirement:?}: {err}"
            );
        }
    }

    #[test]
    fn an_unknown_name_and_an_unknown_version_are_told_apart() {
        let index = Index::parse(&document(&[valid_bundle("alpha", "1.0.0")])).expect("parse");

        let err = index.resolve("beta", "1.0.0").unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNKNOWN_NAME), "{err}");
        assert_eq!(err.kind(), hearth_core::ErrorKind::NotFound);

        let err = index.resolve("alpha", "9.9.9").unwrap_err();
        assert!(err.to_string().starts_with(ERR_UNKNOWN_VERSION), "{err}");
        // The message names what does exist, so the operator does not have to
        // fetch the index again to find out.
        assert!(err.to_string().contains("1.0.0"), "{err}");
    }

    #[test]
    fn a_name_rule_allows_what_it_should() {
        for good in [
            "a",
            "alpha",
            "my-bundle",
            "my_bundle",
            "bundle.v2",
            "a1",
            "1a",
        ] {
            assert!(is_safe_name(good), "{good}");
        }
        for bad in ["", "A", "a/b", "..", "...", "-a", "a b", &"x".repeat(65)] {
            assert!(!is_safe_name(bad), "{bad}");
        }
    }

    #[test]
    fn an_unknown_member_does_not_stop_a_v1_index_from_reading() {
        // Additive members are how a market grows without a version bump; the
        // same rule `hearth-core` states for event payloads.
        let text = format!(
            r#"{{"version":1,"bundles":[{{"name":"a","version":"1","description":"d","tarball":"https://x.invalid/a.tar","sha256":"{}","size":1,"screenshots":["a.png"]}}]}}"#,
            sha256_hex(b"x")
        );
        let index = Index::parse(text.as_bytes()).expect("an unknown member is data");
        assert_eq!(index.bundles.len(), 1);
    }

    // --- platform-aware bundles -------------------------------------------
    //
    // Before these fields existed a native plugin could not be distributed at
    // all: `Bundle` had no os/arch and the uniqueness key was name+version, so
    // six platform builds of one version could not even be listed. ADR-0003 §2.7
    // measured why that matters — a native plugin costs 0.7 MB private against a
    // Node plugin's 19.1 MB, and until the market can carry it that number is
    // only reachable by hand-copying a binary.

    /// The six targets `.github/workflows/release.yml` actually publishes.
    const RELEASE_MATRIX: [(&str, &str); 3] = [
        ("windows", "amd64"),
        ("linux", "amd64"),
        ("darwin", "arm64"),
    ];

    fn native_bundle(name: &str, version: &str, os: &str, arch: &str) -> Bundle {
        let mut bundle = valid_bundle(name, version);
        bundle.tarball = format!("https://example.invalid/{name}-{version}-{os}-{arch}.tar.gz");
        // Distinct bytes per platform, so a test that got the wrong entry sees a
        // different digest rather than passing by coincidence.
        bundle.sha256 = sha256_hex(format!("{os}-{arch}").as_bytes());
        bundle.os = Some(os.to_string());
        bundle.arch = Some(arch.to_string());
        bundle
    }

    #[test]
    fn every_release_target_can_be_listed_and_resolved_to_its_own_build() {
        let bundles: Vec<Bundle> = RELEASE_MATRIX
            .iter()
            .map(|(os, arch)| native_bundle("native", "1.0.0", os, arch))
            .collect();
        let index = Index::parse(&document(&bundles)).expect("three builds of one version");
        assert_eq!(index.bundles.len(), 3);
        for (os, arch) in RELEASE_MATRIX {
            let picked = index
                .resolve_for("native", "1.0.0", &Platform::new(os, arch))
                .expect("a build for this target");
            assert_eq!(picked.os.as_deref(), Some(os));
            assert_eq!(picked.arch.as_deref(), Some(arch));
            assert_eq!(picked.sha256, sha256_hex(format!("{os}-{arch}").as_bytes()));
        }
    }

    #[test]
    fn the_accepted_spellings_are_the_ones_the_release_pipeline_publishes() {
        // `darwin`, not `macos`; `amd64`, not `x86_64`. A closed set spelled like
        // Rust's own target names would refuse the artifacts this project builds.
        for (os, arch) in RELEASE_MATRIX {
            assert!(KNOWN_OS.contains(&os), "{os}");
            assert!(KNOWN_ARCH.contains(&arch), "{arch}");
        }
        for wrong in [
            native_bundle("n", "1", "macos", "arm64"),
            native_bundle("n", "1", "darwin", "x86_64"),
        ] {
            let err = Index::parse(&document(&[wrong])).unwrap_err();
            assert!(err.message().contains("is not one of"), "{}", err.message());
        }
    }

    #[test]
    fn two_entries_for_one_platform_are_still_a_duplicate() {
        let bundles = [
            native_bundle("native", "1.0.0", "linux", "amd64"),
            native_bundle("native", "1.0.0", "linux", "amd64"),
        ];
        let err = Index::parse(&document(&bundles)).unwrap_err();
        assert_eq!(
            err.message(),
            format!(
                "{ERR_DUPLICATE_BUNDLE}: bundles[0] and a later entry are native/1.0.0 for linux/amd64"
            )
        );
    }

    #[test]
    fn half_a_platform_is_refused_rather_than_defaulted() {
        let mut os_only = valid_bundle("half", "1");
        os_only.os = Some("linux".to_string());
        let mut arch_only = valid_bundle("half", "2");
        arch_only.arch = Some("amd64".to_string());
        for bundle in [os_only, arch_only] {
            let err = Index::parse(&document(&[bundle])).unwrap_err();
            assert!(
                err.message().contains("declare both or neither"),
                "{}",
                err.message()
            );
        }
    }

    #[test]
    fn a_source_bundle_fits_every_platform_including_ones_we_do_not_build() {
        let source = valid_bundle("source", "1.0.0");
        for platform in [
            Platform::new("windows", "amd64"),
            Platform::new("linux", "arm64"),
            Platform::new("darwin", "amd64"),
            Platform::new("unknown", "unknown"),
        ] {
            assert!(source.fits(&platform), "{}", platform.label());
        }
        assert_eq!(source.platform_label(), "any platform");
    }

    #[test]
    fn a_version_published_only_elsewhere_says_where_it_is_published() {
        // The difference between "you typed a version that does not exist" and
        // "we do not ship your platform" is the difference between looking for a
        // typo and waiting for a build. The message has to name the platforms.
        let index = Index::parse(&document(&[native_bundle(
            "native", "1.0.0", "linux", "amd64",
        )]))
        .expect("parse");
        let err = index
            .resolve_for("native", "1.0.0", &Platform::new("windows", "amd64"))
            .unwrap_err();
        assert!(err.message().contains(ERR_NO_BUILD_FOR_PLATFORM));
        assert!(err.message().contains("linux/amd64"), "{}", err.message());
        assert!(err.message().contains("windows/amd64"), "{}", err.message());
        assert!(!err.message().contains(ERR_UNKNOWN_VERSION));
    }

    #[test]
    fn star_counts_versions_not_platform_entries() {
        let bundles: Vec<Bundle> = RELEASE_MATRIX
            .iter()
            .map(|(os, arch)| native_bundle("native", "2.0.0", os, arch))
            .collect();
        let index = Index::parse(&document(&bundles)).expect("parse");
        // Three entries, one version: `*` is unambiguous for each target.
        for (os, arch) in RELEASE_MATRIX {
            let picked = index
                .resolve_for("native", "*", &Platform::new(os, arch))
                .expect("one version");
            assert_eq!(picked.version, "2.0.0");
            assert_eq!(picked.os.as_deref(), Some(os));
        }
    }

    #[test]
    fn a_source_and_a_native_build_for_one_platform_are_refused_not_guessed() {
        // Both fit windows/amd64. Preferring the native one would be a policy
        // this crate does not have, and preferring the source one would silently
        // discard the build the publisher added on purpose.
        let bundles = [
            valid_bundle("dual", "1.0.0"),
            native_bundle("dual", "1.0.0", "windows", "amd64"),
        ];
        let index = Index::parse(&document(&bundles)).expect("distinct platforms");
        let platform = Platform::new("windows", "amd64");
        for requirement in ["*", "1.0.0"] {
            let err = index
                .resolve_for("dual", requirement, &platform)
                .unwrap_err();
            assert!(
                err.message().contains(ERR_AMBIGUOUS_PLATFORM),
                "{requirement}: {}",
                err.message()
            );
        }
        // Another platform sees only the source bundle, so it is not ambiguous.
        let picked = index
            .resolve_for("dual", "1.0.0", &Platform::new("linux", "amd64"))
            .expect("only the source bundle fits");
        assert!(picked.os.is_none());
    }

    #[test]
    fn an_index_without_platform_members_reads_exactly_as_before() {
        // Every index published before this change has no os/arch. It must keep
        // resolving on every platform, or the change breaks the existing market.
        let index = Index::parse(&document(&[valid_bundle("legacy", "1.0.0")])).expect("parse");
        for (os, arch) in RELEASE_MATRIX {
            let picked = index
                .resolve_for("legacy", "*", &Platform::new(os, arch))
                .expect("a legacy bundle fits everywhere");
            assert_eq!(picked.name, "legacy");
        }
        let encoded = serde_json::to_string(&index.bundles[0]).expect("encode");
        assert!(!encoded.contains("\"os\""), "{encoded}");
        assert!(!encoded.contains("\"arch\""), "{encoded}");
    }
}
