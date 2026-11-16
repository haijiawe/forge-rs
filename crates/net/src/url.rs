//! Just enough URL parsing for an HTTP client: scheme, authority, path.
//!
//! This is not a general URL library. It accepts what an HTTP request line
//! needs — `scheme://authority/path?query` — and refuses everything else, so a
//! malformed URL is reported once, at parse time, instead of producing a
//! request line the server reads as a different resource.

use forge_core::{Error, ErrorKind, Result};

/// A parsed absolute HTTP(S) URL.
///
/// `host` and `port` are split out because connecting needs them separately
/// while the request line needs the authority as written (it may carry a port
/// and must not carry a scheme).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub authority: String,
    pub host: String,
    pub port: u16,
    /// Path plus query, exactly as it should appear in the request line. Empty
    /// when the URL named no path; use [`Url::request_target`] to get `/`.
    pub path: String,
}

impl Url {
    /// Parse `scheme://authority[/path][?query]`.
    ///
    /// A fragment is dropped: it is never sent to a server.
    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.trim();
        let without_fragment = match trimmed.split_once('#') {
            Some((head, _)) => head,
            None => trimmed,
        };
        let Some((scheme, rest)) = without_fragment.split_once("://") else {
            return Err(invalid(format!(
                "URL {input:?} has no scheme; write http:// or https://"
            )));
        };
        let scheme = scheme.to_ascii_lowercase();
        let is_https = match scheme.as_str() {
            "http" => false,
            "https" => true,
            other => {
                return Err(invalid(format!(
                    "URL scheme {other:?} is not supported; only http and https are"
                )))
            }
        };

        // Authority ends at the first `/` or `?`.
        let (authority, path) = match rest.find(['/', '?']) {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(invalid(format!("URL {input:?} names no host")));
        }
        if authority.contains('@') {
            return Err(invalid(format!(
                "URL {input:?} carries credentials; send them as a header instead"
            )));
        }
        let host = host_without_port(authority);
        if host.is_empty() {
            return Err(invalid(format!("URL {input:?} names no host")));
        }
        let default_port = if is_https { 443 } else { 80 };
        let port = port_from_authority(authority).unwrap_or(default_port);

        Ok(Self {
            scheme,
            authority: authority.to_string(),
            host,
            port,
            path: path.to_string(),
        })
    }

    /// The request target for the request line. Never empty.
    pub fn request_target(&self) -> String {
        if self.path.is_empty() {
            "/".to_string()
        } else {
            self.path.clone()
        }
    }

    /// Resolve a `Location` header against this URL.
    ///
    /// Handles the three forms a server actually sends: absolute, root-relative
    /// (`/x`) and path-relative (`x`, `../x`).
    pub fn join(&self, location: &str) -> Result<Self> {
        let location = location.trim();
        if location.is_empty() {
            return Err(invalid("redirect has an empty Location".to_string()));
        }
        if location.contains("://") {
            return Self::parse(location);
        }
        let target = if let Some(rest) = location.strip_prefix("//") {
            format!("{}://{rest}", self.scheme)
        } else if location.starts_with('/') {
            format!("{}://{}{}", self.scheme, self.authority, location)
        } else {
            let base = self.request_target();
            let directory = match base.rfind('/') {
                Some(index) => &base[..=index],
                None => "/",
            };
            format!(
                "{}://{}{}",
                self.scheme,
                self.authority,
                normalize(&format!("{directory}{location}"))
            )
        };
        Self::parse(&target)
    }

    /// The origin (scheme, host, port) without a path, for `Host` headers.
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.authority)
    }
}

fn invalid(message: String) -> Error {
    Error::new(ErrorKind::Invalid, message)
}

/// Strip `:port` from an authority. IPv6 literals keep their brackets.
pub fn host_without_port(authority: &str) -> String {
    if let Some(rest) = authority.strip_prefix('[') {
        // `[::1]:8080` or `[::1]`
        return match rest.split_once(']') {
            Some((host, _)) => format!("[{host}]"),
            None => authority.to_string(),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_string(),
        _ => authority.to_string(),
    }
}

/// Parse an explicit `:port`. Returns `None` when absent or unusable.
pub fn port_from_authority(authority: &str) -> Option<u16> {
    if authority.starts_with('[') {
        let rest = authority.strip_prefix('[')?;
        let (_, after) = rest.split_once(']')?;
        return after.strip_prefix(':')?.parse().ok();
    }
    let (_, port) = authority.rsplit_once(':')?;
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    port.parse().ok()
}

/// Collapse `.` and `..` segments in a path, leaving a leading `/` and any
/// trailing slash intact.
fn normalize(path: &str) -> String {
    let query = match path.find('?') {
        Some(index) => path[index..].to_string(),
        None => String::new(),
    };
    let without_query = match path.find('?') {
        Some(index) => &path[..index],
        None => path,
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in without_query.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut out = String::from("/");
    out.push_str(&segments.join("/"));
    if without_query.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(&query);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_scheme_host_port_and_path() {
        let url = Url::parse("https://example.com:8443/a/b?x=1#frag").expect("parse");
        assert_eq!(url.scheme, "https");
        assert_eq!(url.authority, "example.com:8443");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 8443);
        assert_eq!(url.path, "/a/b?x=1");
        assert_eq!(url.request_target(), "/a/b?x=1");
    }

    #[test]
    fn a_bare_host_gets_a_root_request_target() {
        let url = Url::parse("http://example.com").expect("parse");
        assert_eq!(url.request_target(), "/");
        assert_eq!(url.port, 80);
        let url = Url::parse("https://example.com").expect("parse");
        assert_eq!(url.port, 443);
    }

    #[test]
    fn refuses_what_it_cannot_send() {
        assert!(Url::parse("example.com/x").is_err());
        assert!(Url::parse("ftp://example.com/").is_err());
        assert!(Url::parse("https:///x").is_err());
        assert!(Url::parse("https://user:pw@example.com/").is_err());
    }

    #[test]
    fn keeps_ipv6_literals_whole() {
        let url = Url::parse("http://[::1]:8080/x").expect("parse");
        assert_eq!(url.host, "[::1]");
        assert_eq!(url.port, 8080);
        let url = Url::parse("http://[::1]/x").expect("parse");
        assert_eq!(url.host, "[::1]");
        assert_eq!(url.port, 80);
    }

    #[test]
    fn resolves_the_three_forms_of_location() {
        let base = Url::parse("https://example.com/a/b?q=1").expect("parse");

        let absolute = base.join("http://other.test/z").expect("absolute");
        assert_eq!(absolute.origin(), "http://other.test");

        let root = base.join("/x/y").expect("root relative");
        assert_eq!(root.path, "/x/y");
        assert_eq!(root.authority, "example.com");

        let relative = base.join("c").expect("relative");
        assert_eq!(relative.path, "/a/c");

        let up = base.join("../d").expect("parent");
        assert_eq!(up.path, "/d");

        let dotted = base.join("./e").expect("dot");
        assert_eq!(dotted.path, "/a/e");
    }

    #[test]
    fn normalizing_keeps_the_trailing_slash_and_query() {
        assert_eq!(normalize("/a/b/../c/"), "/a/c/");
        assert_eq!(normalize("/a/./b?x=1"), "/a/b?x=1");
        assert_eq!(normalize("/../x"), "/x");
    }
}
