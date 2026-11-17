//! Civil-calendar arithmetic, without a date library.
//!
//! Howard Hinnant's `days_from_civil` / `civil_from_days`. They are exact for
//! every date in the range this crate accepts, they are thirty lines, and unlike
//! a dependency they can be checked against a table of hand-computed days.
//!
//! A day is one integer: days since 1970-01-01 in the proleptic Gregorian
//! calendar. Everything else in the crate is milliseconds derived from it, so
//! there is exactly one place where months and leap years are remembered.

/// Milliseconds in a UTC day. UTC has no leap seconds, which is what makes a
/// day a constant.
pub(crate) const MS_PER_DAY: i64 = 86_400_000;
pub(crate) const MS_PER_HOUR: i64 = 3_600_000;
pub(crate) const MS_PER_MINUTE: i64 = 60_000;
pub(crate) const MS_PER_SECOND: i64 = 1_000;

/// The years the rule engine will walk. Outside them the calendar code would
/// have to leave `i64`, and a schedule a few thousand years out is not a
/// schedule anyone can act on.
pub(crate) const MIN_YEAR: i64 = -9999;
pub(crate) const MAX_YEAR: i64 = 9999;

pub(crate) fn in_supported_year(year: i64) -> bool {
    (MIN_YEAR..=MAX_YEAR).contains(&year)
}

/// The last instant the engine will produce.
pub(crate) fn last_supported_ms() -> i64 {
    days_from_civil(MAX_YEAR, 12, 31) * MS_PER_DAY + MS_PER_DAY - 1
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    // March is month 0 here: it puts the leap day at the end of the year, which
    // is what makes the era arithmetic below a few integer divisions.
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let month_index = (i64::from(month) + 9) % 12; // [0, 11]
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`].
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

pub(crate) fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The length of a month. Callers validate the month first; an out-of-range
/// month answers zero so a caller that forgot cannot silently get 31 days.
pub(crate) fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Monday is 0, Sunday is 6. 1970-01-01 was a Thursday, so day zero maps to 3.
pub(crate) fn weekday_from_days(days: i64) -> u32 {
    (days + 3).rem_euclid(7) as u32
}

/// A zero-based month index, so month arithmetic is one integer add.
pub(crate) fn month_index(year: i64, month: u32) -> i64 {
    year * 12 + i64::from(month) - 1
}

pub(crate) fn civil_from_month_index(index: i64) -> (i64, u32) {
    (index.div_euclid(12), (index.rem_euclid(12) + 1) as u32)
}

/// A UTC civil time as milliseconds since the epoch.
///
/// Out-of-range components are carried the way a calendar would carry them. The
/// command (`forge schedule --start`) validates its fields before calling this,
/// so a rejection there is a usage error and not something this function has to
/// report twice.
pub fn from_civil(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    let days = days_from_civil(year, month, day);
    let seconds = days.saturating_mul(86_400)
        + i64::from(hour) * 3_600
        + i64::from(minute) * 60
        + i64::from(second);
    seconds.saturating_mul(1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_day_zero_and_a_thursday() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(weekday_from_days(0), 3, "1970-01-01 was a Thursday");
        assert_eq!(weekday_from_days(-1), 2, "1969-12-31 was a Wednesday");
    }

    #[test]
    fn a_leap_day_exists_in_2024_and_not_in_2100() {
        // 2024-02-29 is day 19782; the next day is March.
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(days_from_civil(2024, 3, 1), 19_783);
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert!(is_leap_year(2024));
        // 2100 is divisible by 100 and not by 400: no February 29.
        assert!(!is_leap_year(2100));
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_from_civil(2100, 2, 28), 47_540);
        assert_eq!(days_from_civil(2100, 3, 1), 47_541);
        assert_eq!(civil_from_days(47_541), (2100, 3, 1));
        // 2000 is divisible by 400: it is a leap year.
        assert!(is_leap_year(2000));
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
    }

    #[test]
    fn a_month_end_rolls_into_the_next_month() {
        // January 31 plus one day is February 1 in an ordinary year and in a
        // leap year alike; the roll is the day count, not a special case.
        assert_eq!(
            days_from_civil(2024, 1, 31) + 1,
            days_from_civil(2024, 2, 1)
        );
        assert_eq!(
            days_from_civil(2024, 2, 28) + 1,
            days_from_civil(2024, 2, 29)
        );
        assert_eq!(
            days_from_civil(2024, 2, 29) + 1,
            days_from_civil(2024, 3, 1)
        );
        assert_eq!(
            days_from_civil(2025, 2, 28) + 1,
            days_from_civil(2025, 3, 1)
        );
        assert_eq!(
            days_from_civil(2024, 12, 31) + 1,
            days_from_civil(2025, 1, 1)
        );
        assert_eq!(
            civil_from_days(days_from_civil(2024, 1, 31) + 1),
            (2024, 2, 1)
        );
    }

    #[test]
    fn every_sampled_day_round_trips() {
        // A wide, sparse sweep: a bug in the era arithmetic tends to show up a
        // few centuries from the epoch, not on the epoch itself.
        for day in (-400_000..=400_000).step_by(991) {
            let (year, month, dom) = civil_from_days(day);
            assert_eq!(days_from_civil(year, month, dom), day, "day {day}");
        }
    }

    #[test]
    fn month_indices_are_linear() {
        assert_eq!(month_index(2024, 1), month_index(2023, 12) + 1);
        assert_eq!(civil_from_month_index(month_index(2024, 1)), (2024, 1));
        assert_eq!(civil_from_month_index(month_index(2024, 1) - 1), (2023, 12));
        assert_eq!(month_index(1970, 1), 1970 * 12);
    }

    #[test]
    fn weekdays_agree_with_known_dates() {
        // 2024-01-01 was a Monday; 2024-01-06 a Saturday; 2024-01-07 a Sunday.
        assert_eq!(weekday_from_days(days_from_civil(2024, 1, 1)), 0);
        assert_eq!(weekday_from_days(days_from_civil(2024, 1, 6)), 5);
        assert_eq!(weekday_from_days(days_from_civil(2024, 1, 7)), 6);
        // 2024-02-29 was a Thursday.
        assert_eq!(weekday_from_days(days_from_civil(2024, 2, 29)), 3);
    }

    #[test]
    fn from_civil_matches_the_instants_the_tests_pin() {
        assert_eq!(from_civil(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(from_civil(2026, 11, 17, 9, 0, 0), 1_794_906_000_000);
        assert_eq!(from_civil(2026, 11, 17, 0, 0, 0), 1_794_873_600_000);
        assert_eq!(from_civil(2024, 2, 29, 9, 0, 0), 1_709_197_200_000);
        assert_eq!(from_civil(1969, 12, 31, 0, 0, 0), -86_400_000);
        assert_eq!(
            from_civil(2026, 11, 17, 9, 0, 0) / MS_PER_SECOND % 60,
            0,
            "a whole minute"
        );
    }

    #[test]
    fn the_supported_range_is_wide_but_finite() {
        assert!(in_supported_year(2026));
        assert!(in_supported_year(MAX_YEAR));
        assert!(in_supported_year(MIN_YEAR));
        assert!(!in_supported_year(MAX_YEAR + 1));
        assert!(last_supported_ms() > days_from_civil(MAX_YEAR, 12, 31) * MS_PER_DAY);
    }
}
