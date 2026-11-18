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

use forge_core::Result;
use serde::{Deserialize, Serialize};

use crate::error::{
    invalid_with, refuse_with, unsupported_with, ERR_AMBIGUOUS_STAR, ERR_BUNDLE_NAME,
    ERR_BUNDLE_VERSION, ERR_DUPLICATE_BUNDLE, ERR_INDEX_VERSION, ERR_RANGE_REQUIREMENT,
    ERR_SHA256_SHAPE, ERR_SIZE, ERR_TARBALL_URL, ERR_UNKNOWN_NAME, ERR_UNKNOWN_VERSION,
};
use crate::hash::is_sha256_hex;

/// The only index generation this build reads.
pub const INDEX_VERSION: u32 = 1;

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
}

/// The `engines` object: factual constraints from the publisher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Engines {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<String>,
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
        for (position, bundle) in self.bundles.iter().enumerate() {
            for other in &self.bundles[position + 1..] {
                if other.name == bundle.name && other.version == bundle.version {
                    return Err(refuse_with(
                        ERR_DUPLICATE_BUNDLE,
                        format!(
                            "bundles[{position}] and a later entry are {}/{}",
                            bundle.name, bundle.version
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
        let requirement = requirement.trim();
        if requirement.is_empty() {
            return Err(refuse_with(
                ERR_RANGE_REQUIREMENT,
                "the requirement is empty; use an exact version or \"*\"",
            ));
        }
        let candidates: Vec<&Bundle> = self.versions_of(name);
        if candidates.is_empty() {
            return Err(forge_core::Error::new(
                forge_core::ErrorKind::NotFound,
                format!("{}: {name:?}", ERR_UNKNOWN_NAME),
            ));
        }

        if requirement == "*" {
            return match candidates.len() {
                1 => Ok(candidates[0]),
                _ => Err(refuse_with(
                    ERR_AMBIGUOUS_STAR,
                    format!(
                        "{name:?} has versions {}; name one",
                        candidates
                            .iter()
                            .map(|bundle| bundle.version.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
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

        match candidates
            .iter()
            .find(|bundle| bundle.version == requirement)
        {
            Some(bundle) => Ok(bundle),
            None => Err(forge_core::Error::new(
                forge_core::ErrorKind::NotFound,
                format!(
                    "{}: {name:?} has {}",
                    ERR_UNKNOWN_VERSION,
                    candidates
                        .iter()
                        .map(|bundle| bundle.version.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
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
    Ok(())
}

/// A bundle name becomes `<root>/bundles/<name>`; the rule is the same one
/// `forge-skill` and the plugin protocol use for a name that becomes a path.
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
        assert_eq!(err.kind(), forge_core::ErrorKind::UnsupportedVersion);
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
        assert_eq!(err.kind(), forge_core::ErrorKind::NotFound);

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
        // same rule `forge-core` states for event payloads.
        let text = format!(
            r#"{{"version":1,"bundles":[{{"name":"a","version":"1","description":"d","tarball":"https://x.invalid/a.tar","sha256":"{}","size":1,"screenshots":["a.png"]}}]}}"#,
            sha256_hex(b"x")
        );
        let index = Index::parse(text.as_bytes()).expect("an unknown member is data");
        assert_eq!(index.bundles.len(), 1);
    }
}
