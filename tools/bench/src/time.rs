//! Minimal RFC 3339 time handling, std only.
//!
//! The baseline format records timestamps as RFC 3339 strings. Selecting the
//! newest record for a platform needs a real instant comparison (offsets such
//! as `+08:00` must not sort after `Z` by plain string order), and writing a
//! record needs a UTC timestamp. No third-party date crate is available, so
//! this module implements the civil-calendar conversions directly.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: (year, month, day).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month as u32, day as u32)
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_now_rfc3339() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn digits(bytes: &[u8], at: usize, len: usize) -> Option<i64> {
    if at + len > bytes.len() {
        return None;
    }
    let mut value = 0i64;
    for &b in &bytes[at..at + len] {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (b - b'0') as i64;
    }
    Some(value)
}

/// Parse the RFC 3339 subset emitted by this tool and by the Go oracle:
/// `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)`.
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let b = text.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let year = digits(b, 0, 4)?;
    let month = digits(b, 5, 2)? as u32;
    let day = digits(b, 8, 2)? as u32;
    let hour = digits(b, 11, 2)?;
    let minute = digits(b, 14, 2)?;
    let second = digits(b, 17, 2)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let mut at = 19;
    let mut nanos = 0u32;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while at < b.len() && b[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
        let mut fraction = text[start..at].to_string();
        while fraction.len() < 9 {
            fraction.push('0');
        }
        fraction.truncate(9);
        nanos = fraction.parse::<u32>().ok()?;
    }

    let (offset_secs, end) = match b.get(at) {
        Some(b'Z') | Some(b'z') => (0i64, at + 1),
        Some(sign @ (b'+' | b'-')) => {
            if at + 6 > b.len() || b[at + 3] != b':' {
                return None;
            }
            let oh = digits(b, at + 1, 2)?;
            let om = digits(b, at + 4, 2)?;
            if oh > 23 || om > 59 {
                return None;
            }
            let value = oh * 3600 + om * 60;
            (if *sign == b'-' { -value } else { value }, at + 6)
        }
        _ => return None,
    };
    if end != b.len() {
        return None;
    }

    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_secs;
    let base = if secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))?
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs((-secs) as u64))?
    };
    base.checked_add(Duration::from_nanos(nanos as u64))
}

/// True when `a` is a strictly later instant than `b`.
///
/// A parseable timestamp beats an unparseable one; two unparseable timestamps
/// tie, so the first record encountered is kept. This mirrors the Go oracle's
/// `timestampAfter` selection rule.
pub fn timestamp_after(a: &str, b: &str) -> bool {
    match (parse_rfc3339(a), parse_rfc3339(b)) {
        (Some(x), Some(y)) => x > y,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_conversion_roundtrips_epoch_and_leap_day() {
        for (y, m, d) in [
            (1970, 1, 1),
            (2024, 2, 29),
            (2000, 3, 1),
            (1999, 12, 31),
            (2100, 3, 1),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn parses_utc_and_offset_offsets_to_the_same_instant() {
        let z = parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        let plus = parse_rfc3339("2026-01-01T08:00:00+08:00").unwrap();
        let minus = parse_rfc3339("2025-12-31T16:00:00-08:00").unwrap();
        assert_eq!(z, plus);
        assert_eq!(z, minus);
        assert_eq!(
            parse_rfc3339("2026-01-01T00:00:00.123456789Z").unwrap(),
            z + Duration::from_millis(123) + Duration::from_nanos(456_789)
        );
    }

    #[test]
    fn rejects_malformed_timestamps() {
        for bad in [
            "",
            "2026-01-01",
            "2026-13-01T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:00",
            "2026-01-01T00:00:00Zjunk",
            "not-a-time",
        ] {
            assert!(parse_rfc3339(bad).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn timestamp_after_prefers_parseable_and_keeps_first_on_tie() {
        assert!(timestamp_after(
            "2026-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z"
        ));
        assert!(!timestamp_after(
            "2025-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z"
        ));
        assert!(timestamp_after("2026-01-01T00:00:00Z", "garbage"));
        assert!(!timestamp_after("garbage", "2026-01-01T00:00:00Z"));
        assert!(!timestamp_after("garbage", "also garbage"));
        assert!(!timestamp_after(
            "2026-01-01T08:00:00+08:00",
            "2026-01-01T00:00:00Z"
        ));
    }

    #[test]
    fn now_is_parseable_utc_second_precision() {
        let now = utc_now_rfc3339();
        assert!(now.ends_with('Z'));
        assert!(parse_rfc3339(&now).is_some());
    }
}
