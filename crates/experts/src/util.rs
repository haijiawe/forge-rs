//! Small, dependency-free helpers so every diagnostic is byte-stable.
//!
//! Why this file exists instead of a `hearth-core` import: `docs/09-platform-tier.md`
//! §3 gives this crate an empty "may import" column and `architecture.yaml`'s
//! layer rule would let it reach *down*, but a loader for two Markdown formats
//! should not be a reason for L1 to start importing L0 vocabulary it does not
//! need. The helpers below are forty lines; an edge is forever.

use std::path::Path;

/// Render a path with forward slashes on every platform.
///
/// The message strings this crate produces are asserted in tests and printed by
/// `doctor`; normalising the separator here means one expected string works on
/// Windows and Unix instead of two.
pub(crate) fn display(path: &Path) -> String {
    let text = path.as_os_str().to_string_lossy().into_owned();
    if std::path::MAIN_SEPARATOR == '\\' {
        text.replace('\\', "/")
    } else {
        text
    }
}

/// A fixed English rendering of the I/O errors a roster can hit.
///
/// `std::io::Error`'s own `Display` includes platform wording that changes with
/// the OS locale; a refusal message that changes with the locale cannot be
/// matched by a test or a runbook.
pub(crate) fn describe_io(err: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound => "no such file or directory".to_string(),
        ErrorKind::PermissionDenied => "permission denied".to_string(),
        ErrorKind::AlreadyExists => "file exists".to_string(),
        ErrorKind::InvalidInput => "invalid argument".to_string(),
        ErrorKind::UnexpectedEof => "unexpected EOF".to_string(),
        _ => err.to_string(),
    }
}

/// Quote a value the way the rest of this workspace's diagnostics do.
pub(crate) fn quote(value: &str) -> String {
    format!("{value:?}")
}

/// `filepath.Clean` for the fallback canonical key, without touching the disk.
pub(crate) fn clean_path(text: &str) -> String {
    let absolute = text.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if let Some(last) = parts.last() {
                    if *last != ".." {
                        parts.pop();
                    } else {
                        parts.push(part);
                    }
                } else if !absolute {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let body = parts.join("/");
    if absolute {
        format!("/{body}")
    } else if body.is_empty() {
        ".".to_string()
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_uses_forward_slashes() {
        let path = Path::new("a").join("b").join("expert.md");
        assert_eq!(display(&path), "a/b/expert.md");
    }

    #[test]
    fn clean_path_resolves_dot_segments() {
        assert_eq!(clean_path("a/b/../c"), "a/c");
        assert_eq!(clean_path("/a//b/./c/"), "/a/b/c");
        assert_eq!(clean_path("."), ".");
    }
}
