//! Small, dependency-free helpers for deterministic output.
//!
//! Every string that can reach a golden file is normalised here: paths use
//! forward slashes, I/O errors use a fixed vocabulary, and Go-style `%q`
//! quoting is approximated with Rust's `Debug` formatter.

use std::path::Path;

/// Render a path with forward slashes on every platform.
///
/// I/O still happens through `std::path`; only the *output* string is
/// normalised, so goldens are byte-identical on Windows and Unix.
pub(crate) fn display(path: &Path) -> String {
    let text = path.as_os_str().to_string_lossy().into_owned();
    if std::path::MAIN_SEPARATOR == '\\' {
        text.replace('\\', "/")
    } else {
        text
    }
}

/// Go's `filepath.Clean` for the subset of paths this crate produces.
///
/// Collapses duplicate separators and resolves `.` / `..` lexically. It never
/// touches the filesystem, which is why it is the fallback when `canonicalize`
/// fails.
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

/// A fixed English rendering of common I/O errors, matching Go's strings.
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

/// Quote a string the way Go's `%q` does for the values in our goldens.
pub(crate) fn quote(value: &str) -> String {
    format!("{value:?}")
}

/// Drop an incomplete UTF-8 sequence at the end of a capped byte slice.
///
/// Go scans backwards at most `utf8.UTFMax` bytes, skips continuation bytes,
/// and cuts at the first rune start that is not a complete rune.
pub(crate) fn trim_incomplete_rune(data: &[u8]) -> &[u8] {
    let from = data.len().saturating_sub(4);
    for index in (from..data.len()).rev() {
        let byte = data[index];
        if byte & 0b1100_0000 == 0b1000_0000 {
            continue;
        }
        if full_rune(&data[index..]) {
            return data;
        }
        return &data[..index];
    }
    data
}

/// Mirrors `utf8.FullRune`: is the first character of `data` complete?
fn full_rune(data: &[u8]) -> bool {
    let Some(&first) = data.first() else {
        return false;
    };
    if first < 0x80 {
        return true;
    }
    let needed = if first >= 0xF0 {
        4
    } else if first >= 0xE0 {
        3
    } else if first >= 0xC0 {
        2
    } else {
        return false;
    };
    if data.len() < needed {
        return false;
    }
    data[1..needed]
        .iter()
        .all(|byte| byte & 0b1100_0000 == 0b1000_0000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn clean_path_collapses_dot_segments() {
        assert_eq!(clean_path("a/b/../c"), "a/c");
        assert_eq!(clean_path("/a//b/./c/"), "/a/b/c");
        assert_eq!(clean_path("../a"), "../a");
        assert_eq!(clean_path("."), ".");
        assert_eq!(clean_path(""), ".");
    }

    #[test]
    fn display_uses_forward_slashes() {
        let path = PathBuf::from("a").join("b").join("SKILL.md");
        assert_eq!(display(&path), "a/b/SKILL.md");
    }

    #[test]
    fn trim_incomplete_rune_keeps_valid_utf8() {
        assert_eq!(trim_incomplete_rune(b"abc"), b"abc");
        assert_eq!(trim_incomplete_rune("é".as_bytes()), "é".as_bytes());
        assert_eq!(trim_incomplete_rune(&[0xF0, 0x9F, 0x98]), &[]);
        assert_eq!(trim_incomplete_rune(&[b'a', 0xF0, 0x9F, 0x98]), b"a");
    }
}
