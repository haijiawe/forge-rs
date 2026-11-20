//! The occurrence table is this crate's oracle. Every instant below is written
//! with the UTC date it is, computed outside this crate (hand arithmetic, then
//! cross-checked with .NET's `DateTimeOffset`), not by the code under test —
//! otherwise the test would agree with whatever bug the calendar had.
//!
//! The rules are strings an operator could type, so the table pins the parser
//! and the walk together: a change to either shows up as a wrong instant here.

use hearth_schedule::{parse_rule, Freq, Rule, Weekday};

// 2026-11-17T09:00:00Z, a Tuesday.
const TUE_0900: i64 = 1_794_906_000_000;
// 2026-11-17T00:00:00Z
const TUE_0000: i64 = 1_794_873_600_000;
// 2026-11-17T09:15:00Z
const TUE_0915: i64 = 1_794_906_900_000;
// 2024-01-01T00:00:00Z, a Monday.
const MON_2024_0000: i64 = 1_704_067_200_000;
// 2024-01-01T08:00:00Z
const MON_2024_0800: i64 = 1_704_096_000_000;
// 2024-01-01T08:15:00Z
const MON_2024_0815: i64 = 1_704_096_900_000;
// 2024-01-01T09:00:00Z
const MON_2024_0900: i64 = 1_704_099_600_000;
// 2024-02-29T00:00:00Z, a Thursday.
const THU_2024_0229: i64 = 1_709_164_800_000;
// 2024-02-29T09:00:00Z
const THU_2024_0229_0900: i64 = 1_709_197_200_000;
// 2026-01-31T00:00:00Z
const SAT_2026_0131: i64 = 1_769_817_600_000;

fn next(rule: &str, start_ms: i64, after_ms: i64) -> Option<i64> {
    parse_rule(rule)
        .expect("rule parses")
        .next_after(start_ms, after_ms)
}

fn run(rule: &str, start_ms: i64, after_ms: i64, limit: usize) -> Vec<i64> {
    parse_rule(rule)
        .expect("rule parses")
        .occurrences(start_ms, after_ms, limit)
}

/// One row of the table: a rule text, the anchor, the search start, how many
/// occurrences to ask for, and the instants they must be.
struct Case {
    rule: &'static str,
    start_ms: i64,
    after_ms: i64,
    limit: usize,
    expected: &'static [i64],
}

#[test]
fn the_occurrence_table() {
    let cases = [
        Case {
            // Daily at 09:00, anchored on a Tuesday at 09:00: the anchor itself
            // is not an occurrence, so the first answer is the next day.
            rule: "FREQ=DAILY;BYHOUR=9;BYMINUTE=0",
            start_ms: TUE_0900,
            after_ms: TUE_0900,
            limit: 3,
            expected: &[
                1_794_992_400_000, // 2026-11-18T09:00:00Z
                1_795_078_800_000, // 2026-11-19T09:00:00Z
                1_795_165_200_000, // 2026-11-20T09:00:00Z
            ],
        },
        Case {
            // Weekly on Monday, Wednesday and Friday. The anchor is a Tuesday,
            // so the first occurrence is the Wednesday of the same week.
            rule: "FREQ=WEEKLY;BYDAY=MO,WE,FR;BYHOUR=9",
            start_ms: TUE_0900,
            after_ms: TUE_0900,
            limit: 3,
            expected: &[
                1_794_992_400_000, // 2026-11-18T09:00:00Z Wednesday
                1_795_165_200_000, // 2026-11-20T09:00:00Z Friday
                1_795_424_400_000, // 2026-11-23T09:00:00Z Monday
            ],
        },
        Case {
            // A weekly rule with no BYDAY repeats the anchor's weekday.
            rule: "FREQ=WEEKLY;BYHOUR=9",
            start_ms: TUE_0900,
            after_ms: TUE_0900,
            limit: 1,
            expected: &[1_795_510_800_000], // 2026-11-24T09:00:00Z, next Tuesday
        },
        Case {
            // Every second week: the same weekday, two week blocks later.
            rule: "FREQ=WEEKLY;INTERVAL=2;BYHOUR=9",
            start_ms: TUE_0900,
            after_ms: TUE_0900,
            limit: 1,
            expected: &[1_796_115_600_000], // 2026-12-01T09:00:00Z
        },
        Case {
            // "The 31st" measured from January: February has no 31st, so the
            // rule means the last day rather than skipping five months a year.
            rule: "FREQ=MONTHLY",
            start_ms: SAT_2026_0131,
            after_ms: SAT_2026_0131,
            limit: 1,
            expected: &[1_772_236_800_000], // 2026-02-28T00:00:00Z
        },
        Case {
            // COUNT counts from the anchor, which itself is number one: two
            // occurrences are left after it.
            rule: "FREQ=DAILY;COUNT=3",
            start_ms: TUE_0000,
            after_ms: TUE_0000,
            limit: 10,
            expected: &[
                1_794_960_000_000, // 2026-11-18T00:00:00Z
                1_795_046_400_000, // 2026-11-19T00:00:00Z
            ],
        },
        Case {
            // UNTIL is inclusive, and an occurrence exactly on it still fires.
            rule: "FREQ=DAILY;UNTIL=20261120T000000Z",
            start_ms: TUE_0000,
            after_ms: TUE_0000,
            limit: 10,
            expected: &[
                1_794_960_000_000, // 2026-11-18T00:00:00Z
                1_795_046_400_000, // 2026-11-19T00:00:00Z
                1_795_132_800_000, // 2026-11-20T00:00:00Z
            ],
        },
        Case {
            // A date-only UNTIL covers the whole of that UTC day.
            rule: "FREQ=DAILY;UNTIL=20261119",
            start_ms: TUE_0000,
            after_ms: TUE_0000,
            limit: 10,
            expected: &[
                1_794_960_000_000, // 2026-11-18T00:00:00Z
                1_795_046_400_000, // 2026-11-19T00:00:00Z
            ],
        },
        Case {
            // Several hours expand into several occurrences a day; the 08:00
            // candidate on the anchor day is before DTSTART and is skipped.
            rule: "FREQ=DAILY;BYHOUR=8,18",
            start_ms: TUE_0900,
            after_ms: TUE_0900,
            limit: 1,
            expected: &[1_794_938_400_000], // 2026-11-17T18:00:00Z
        },
        Case {
            // Several minutes expand the same way inside the hour.
            rule: "FREQ=DAILY;BYMINUTE=0,30",
            start_ms: TUE_0915,
            after_ms: TUE_0915,
            limit: 1,
            expected: &[1_794_907_800_000], // 2026-11-17T09:30:00Z
        },
        Case {
            // Every February 29. The next one after 2024 is 2028; clamping to
            // February 28 would turn this into "every year".
            rule: "FREQ=YEARLY",
            start_ms: THU_2024_0229,
            after_ms: THU_2024_0229,
            limit: 1,
            expected: &[1_835_395_200_000], // 2028-02-29T00:00:00Z
        },
        Case {
            // A yearly rule whose month and day are fixed but filtered to a
            // weekday waits for the year that lines up: 2026-11-17 is a
            // Tuesday, and the next Monday the 17th is 2031.
            rule: "FREQ=YEARLY;BYDAY=MO",
            start_ms: TUE_0000,
            after_ms: TUE_0000,
            limit: 1,
            expected: &[1_952_640_000_000], // 2031-11-17T00:00:00Z, a Monday
        },
        Case {
            // And a February 29 that is also a Sunday: 2032.
            rule: "FREQ=YEARLY;BYDAY=SU",
            start_ms: THU_2024_0229,
            after_ms: THU_2024_0229,
            limit: 1,
            expected: &[1_961_625_600_000], // 2032-02-29T00:00:00Z, a Sunday
        },
        Case {
            // Sub-hourly frequencies are plain arithmetic on the anchor.
            rule: "FREQ=HOURLY;INTERVAL=6",
            start_ms: TUE_0000,
            after_ms: TUE_0000,
            limit: 2,
            expected: &[
                1_794_895_200_000, // 2026-11-17T06:00:00Z
                1_794_916_800_000, // 2026-11-17T12:00:00Z
            ],
        },
        Case {
            // ... and BYHOUR filters them: hourly, but only at 09:00.
            rule: "FREQ=HOURLY;BYHOUR=9",
            start_ms: MON_2024_0000,
            after_ms: MON_2024_0000,
            limit: 2,
            expected: &[
                1_704_099_600_000, // 2024-01-01T09:00:00Z
                1_704_186_000_000, // 2024-01-02T09:00:00Z
            ],
        },
        Case {
            // A daily rule filtered to the weekend: Saturday, Sunday, then the
            // Saturday after.
            rule: "FREQ=DAILY;BYDAY=SA,SU",
            start_ms: MON_2024_0900,
            after_ms: MON_2024_0900,
            limit: 3,
            expected: &[
                1_704_531_600_000, // 2024-01-06T09:00:00Z Saturday
                1_704_618_000_000, // 2024-01-07T09:00:00Z Sunday
                1_705_136_400_000, // 2024-01-13T09:00:00Z Saturday
            ],
        },
        Case {
            // A monthly rule with BYDAY expands every matching day of the
            // month instead of moving the anchor day.
            rule: "FREQ=MONTHLY;BYDAY=MO",
            start_ms: MON_2024_0900,
            after_ms: MON_2024_0900,
            limit: 5,
            expected: &[
                1_704_704_400_000, // 2024-01-08T09:00:00Z Monday
                1_705_309_200_000, // 2024-01-15T09:00:00Z Monday
                1_705_914_000_000, // 2024-01-22T09:00:00Z Monday
                1_706_518_800_000, // 2024-01-29T09:00:00Z Monday
                1_707_123_600_000, // 2024-02-05T09:00:00Z Monday
            ],
        },
        Case {
            // Every fourth February 29 that is also a Monday. 2044 is twenty
            // years out, well past the point where a naive walk of every day
            // would have given up, and the next sample of this rule.
            rule: "FREQ=YEARLY;INTERVAL=4;BYDAY=MO",
            start_ms: THU_2024_0229_0900,
            after_ms: THU_2024_0229_0900,
            limit: 1,
            expected: &[2_340_349_200_000], // 2044-02-29T09:00:00Z, a Monday
        },
        Case {
            // Two hours a day, starting before the anchor's hour: on the first
            // day only the later one is an occurrence.
            rule: "FREQ=DAILY;BYHOUR=9,17",
            start_ms: MON_2024_0800,
            after_ms: MON_2024_0800,
            limit: 2,
            expected: &[
                1_704_099_600_000, // 2024-01-01T09:00:00Z
                1_704_128_400_000, // 2024-01-01T17:00:00Z
            ],
        },
        Case {
            // Two minutes an hour: the 08:00 slot on the anchor day is before
            // DTSTART, so the first answer is 08:30.
            rule: "FREQ=DAILY;BYMINUTE=0,30",
            start_ms: MON_2024_0815,
            after_ms: MON_2024_0815,
            limit: 3,
            expected: &[
                1_704_097_800_000, // 2024-01-01T08:30:00Z
                1_704_182_400_000, // 2024-01-02T08:00:00Z
                1_704_184_200_000, // 2024-01-02T08:30:00Z
            ],
        },
    ];

    for case in cases {
        assert_eq!(
            run(case.rule, case.start_ms, case.after_ms, case.limit),
            case.expected.to_vec(),
            "rule {:?} anchored at {}",
            case.rule,
            case.start_ms
        );
    }
}

#[test]
fn count_stops_the_rule_for_good() {
    let rule = "FREQ=DAILY;COUNT=3";
    assert_eq!(
        next(rule, TUE_0000, 1_795_046_400_000),
        None,
        "the third occurrence is the last one there will ever be"
    );
    assert_eq!(
        next(rule, TUE_0000, 0),
        Some(TUE_0000),
        "asked from before the anchor, the anchor itself is the first occurrence"
    );
}

#[test]
fn a_second_past_the_last_occurrence_is_enough_to_be_done() {
    assert_eq!(
        next("FREQ=SECONDLY;COUNT=5", TUE_0000, TUE_0000 + 4_000),
        None
    );
    assert_eq!(
        next("FREQ=SECONDLY;COUNT=5", TUE_0000, TUE_0000 + 3_000),
        Some(TUE_0000 + 4_000)
    );
}

#[test]
fn a_rule_that_can_never_match_answers_none_instead_of_looping() {
    // Every seven days from a Monday is still a Monday, so "only on Wednesday"
    // names nothing at all. The walk has to notice rather than run forever.
    assert_eq!(
        next(
            "FREQ=DAILY;INTERVAL=7;BYDAY=WE",
            MON_2024_0900,
            MON_2024_0900
        ),
        None
    );
    assert_eq!(
        run(
            "FREQ=DAILY;INTERVAL=7;BYDAY=WE",
            MON_2024_0900,
            MON_2024_0900,
            5
        ),
        Vec::<i64>::new()
    );
}

#[test]
fn an_old_anchor_is_answered_without_walking_from_it() {
    // The anchor is 1970 and the question is 2026. A walk that started at the
    // anchor would burn its budget long before reaching the answer, so the
    // engine jumps to the period around the cutoff.
    let after = TUE_0900;
    assert_eq!(
        next("FREQ=DAILY", 0, after),
        Some(1_794_960_000_000), // 2026-11-18T00:00:00Z
    );
    // Same for the sub-daily frequencies: the answer is one step past the
    // cutoff, exactly.
    assert_eq!(next("FREQ=SECONDLY", 0, after), Some(after + 1_000));
}

#[test]
fn the_anchor_is_never_delivered() {
    let rule: Rule = parse_rule("FREQ=DAILY").expect("parses");
    assert!(
        rule.next_after(TUE_0000, TUE_0000)
            .expect("there is a next one")
            > TUE_0000,
        "strictly after the anchor, or a due task would fire forever"
    );
}

#[test]
fn occurrences_respect_the_limit_and_stay_ascending() {
    for limit in [1usize, 3, 10] {
        let instants = run("FREQ=DAILY;BYHOUR=9", TUE_0000, TUE_0000, limit);
        assert_eq!(instants.len(), limit);
        assert!(instants.windows(2).all(|pair| pair[0] < pair[1]));
    }
    assert_eq!(run("FREQ=DAILY", TUE_0000, TUE_0000, 0), Vec::<i64>::new());
}

#[test]
fn bad_rules_are_refused_with_the_part_named() {
    for (text, needle) in [
        ("", "is empty"),
        ("BYHOUR=9", "FREQ is required"),
        ("FREQ=FORTNIGHTLY", "unknown FREQ"),
        ("FREQ=DAILY;INTERVAL=0", "at least 1"),
        ("FREQ=DAILY;INTERVAL=x", "not a number"),
        ("FREQ=DAILY;BYDAY=1MO", "ordinal"),
        ("FREQ=WEEKLY;BYDAY=-1FR", "ordinal"),
        ("FREQ=WEEKLY;BYDAY=XX", "unknown BYDAY"),
        ("FREQ=DAILY;BYHOUR=24", "out of range"),
        ("FREQ=DAILY;BYMINUTE=60", "out of range"),
        ("FREQ=DAILY;COUNT=3;UNTIL=20261120", "cannot both be given"),
        ("FREQ=DAILY;COUNT=0", "at least 1"),
        ("FREQ=DAILY;BYMONTH=3", "not part of this subset"),
        ("FREQ=DAILY;UNTIL=20261301", "not a real date"),
        ("FREQ=DAILY;NOT_A_PART", "not a KEY=VALUE part"),
    ] {
        let error = parse_rule(text).expect_err(text);
        assert!(
            error.message().contains(needle),
            "rule {text:?}: {:?} does not mention {needle:?}",
            error.message()
        );
    }
}

#[test]
fn the_rule_text_may_carry_the_rrule_prefix_and_odd_whitespace() {
    let with_prefix = parse_rule(" RRULE:FREQ=DAILY ; BYHOUR=9 ").expect("parses");
    let plain = parse_rule("FREQ=DAILY;BYHOUR=9").expect("parses");
    assert_eq!(with_prefix, plain);
    assert_eq!(with_prefix.freq, Freq::Daily);
    assert_eq!(with_prefix.by_hour, vec![9]);
}

#[test]
fn weekdays_round_trip_through_their_names() {
    for (weekday, name, index) in [
        (Weekday::MO, "MO", 0),
        (Weekday::TU, "TU", 1),
        (Weekday::WE, "WE", 2),
        (Weekday::TH, "TH", 3),
        (Weekday::FR, "FR", 4),
        (Weekday::SA, "SA", 5),
        (Weekday::SU, "SU", 6),
    ] {
        assert_eq!(weekday.as_str(), name);
        assert_eq!(weekday.index(), index);
        assert_eq!(Weekday::from_index(index), Some(weekday));
    }
    assert_eq!(Weekday::from_index(7), None);
}

#[test]
fn a_rule_exposes_its_parts_for_the_cli() {
    let rule: Rule =
        parse_rule("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;BYHOUR=9;BYMINUTE=0").expect("parses");
    assert_eq!(rule.freq, Freq::Weekly);
    assert_eq!(rule.interval, 2);
    assert_eq!(rule.by_day, vec![Weekday::MO, Weekday::WE]);
    assert_eq!(rule.by_hour, vec![9]);
    assert_eq!(rule.by_minute, vec![0]);
    assert_eq!(rule.count, None);
    assert_eq!(rule.until_ms, None);
    assert_eq!(rule.freq.as_str(), "WEEKLY");
}

#[test]
fn the_calendar_helpers_agree_with_the_table() {
    // The table above is the arbiter; these pin the pieces it rests on.
    assert_eq!(
        hearth_schedule::from_civil(2026, 11, 17, 9, 0, 0),
        TUE_0900,
        "2026-11-17T09:00:00Z"
    );
    assert_eq!(
        hearth_schedule::from_civil(2024, 2, 29, 0, 0, 0),
        THU_2024_0229,
        "2024-02-29T00:00:00Z"
    );
    assert_eq!(
        hearth_schedule::from_civil(2024, 1, 1, 8, 15, 0),
        MON_2024_0815,
        "2024-01-01T08:15:00Z"
    );
}
