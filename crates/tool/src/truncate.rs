//! Bounded tool output, ported from `forge-v2/internal/tool/truncate.go`.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Maximum bytes; zero uses the default, negative means unbounded.
    pub max_bytes: i64,
    /// Maximum complete lines; zero uses the default, negative means unbounded.
    pub max_lines: i64,
}

pub const DEFAULT_MAX_BYTES: i64 = 64 << 10;
pub const DEFAULT_MAX_LINES: i64 = 2000;

#[allow(non_upper_case_globals)]
pub const DefaultLimits: Limits = Limits {
    max_bytes: DEFAULT_MAX_BYTES,
    max_lines: DEFAULT_MAX_LINES,
};

/// Fill only zero fields with defaults; negative fields stay unbounded.
pub fn effective_limits(lim: Limits) -> Limits {
    Limits {
        max_bytes: if lim.max_bytes == 0 {
            DefaultLimits.max_bytes
        } else {
            lim.max_bytes
        },
        max_lines: if lim.max_lines == 0 {
            DefaultLimits.max_lines
        } else {
            lim.max_lines
        },
    }
}

fn is_utf8_start(byte: u8) -> bool {
    byte & 0xC0 != 0x80
}

/// Keep the head of `text`, drop the tail, and state exactly what was dropped.
pub fn truncate(text: &str, lim: Limits) -> (String, bool) {
    if lim.max_bytes <= 0 && lim.max_lines <= 0 {
        return (text.to_string(), false);
    }

    let mut out = text.to_string();
    let mut lines_cut = 0usize;
    let mut bytes_cut = 0usize;

    if lim.max_lines > 0 {
        let max_lines = lim.max_lines as usize;
        let mut kept = String::with_capacity(out.len());
        let mut rest = out.as_str();
        let mut line = 0usize;
        while let Some(index) = rest.find('\n') {
            if line == max_lines {
                lines_cut = rest.matches('\n').count();
                break;
            }
            kept.push_str(&rest[..=index]);
            rest = &rest[index + 1..];
            line += 1;
        }
        if lines_cut == 0 && line < max_lines {
            kept.push_str(rest);
        } else if lines_cut == 0 {
            lines_cut = rest.matches('\n').count();
        }
        out = kept;
    }

    if lim.max_bytes > 0 && out.len() > lim.max_bytes as usize {
        let mut cut = lim.max_bytes as usize;
        let bytes = out.as_bytes();
        while cut > 0 && !is_utf8_start(bytes[cut]) {
            cut -= 1;
        }
        bytes_cut = out.len() - cut;
        out.truncate(cut);
    }

    if lines_cut == 0 && bytes_cut == 0 {
        return (text.to_string(), false);
    }

    let mut parts = Vec::with_capacity(2);
    if bytes_cut > 0 {
        parts.push(format!("{bytes_cut} bytes"));
    }
    if lines_cut > 0 {
        parts.push(format!("{lines_cut} lines"));
    }
    let marker = format!("\n\n[truncated: {} not shown]", parts.join(", "));
    out.push_str(&marker);
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_head_and_marks_tail() {
        let (out, cut) = truncate(
            "a\nb\nc\n",
            Limits {
                max_bytes: -1,
                max_lines: 2,
            },
        );
        assert!(cut);
        assert_eq!(out, "a\nb\n\n\n[truncated: 1 lines not shown]");
    }

    #[test]
    fn cuts_at_utf8_boundary() {
        let text = "ééé";
        let (out, cut) = truncate(
            text,
            Limits {
                max_bytes: 3,
                max_lines: -1,
            },
        );
        assert!(cut);
        assert_eq!(out, "é\n\n[truncated: 4 bytes not shown]");
    }
}
