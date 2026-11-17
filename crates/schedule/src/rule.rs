//! The rule engine: parse the RFC 5545 subset this crate supports, then walk
//! the calendar to answer "when does this rule fire next".
//!
//! The walk is bounded on purpose. A rule can name an empty set (`FREQ=YEARLY;
//! BYDAY=MO` whose anniversary never lands on a Monday inside the walked range),
//! and an unbounded search would hang the caller instead of saying so. Every
//! walk has a period cap and answers `None` when it runs out; the caps are far
//! wider than any schedule a person writes on purpose.
//!
//! Nothing here reads a clock: DTSTART arrives as `start_ms` and the caller's
//! "now" arrives as `after_ms`, which is what makes the occurrence table a
//! deterministic oracle.

use forge_core::{Error, ErrorKind, Result};

use crate::civil::{
    civil_from_days, civil_from_month_index, days_from_civil, days_in_month, in_supported_year,
    last_supported_ms, month_index, weekday_from_days, MS_PER_DAY, MS_PER_HOUR, MS_PER_MINUTE,
    MS_PER_SECOND,
};

/// The most occurrences one search hands back. A caller asking for more than a
/// lifetime of deliveries is asking for a list, not a schedule.
const MAX_OCCURRENCES: usize = 10_000;

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Invalid, message)
}

/// The base frequency of a rule. The name is the RFC's `FREQ` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Secondly,
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    /// The RFC spelling, so the CLI can echo a rule back unchanged.
    pub fn as_str(self) -> &'static str {
        match self {
            Freq::Secondly => "SECONDLY",
            Freq::Minutely => "MINUTELY",
            Freq::Hourly => "HOURLY",
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
            Freq::Yearly => "YEARLY",
        }
    }

    pub fn parse(text: &str) -> Option<Freq> {
        match text.to_ascii_uppercase().as_str() {
            "SECONDLY" => Some(Freq::Secondly),
            "MINUTELY" => Some(Freq::Minutely),
            "HOURLY" => Some(Freq::Hourly),
            "DAILY" => Some(Freq::Daily),
            "WEEKLY" => Some(Freq::Weekly),
            "MONTHLY" => Some(Freq::Monthly),
            "YEARLY" => Some(Freq::Yearly),
            _ => None,
        }
    }

    /// The length of one candidate step, for the frequencies whose candidates
    /// are plain arithmetic rather than calendar days.
    fn unit_ms(self) -> Option<i64> {
        match self {
            Freq::Secondly => Some(MS_PER_SECOND),
            Freq::Minutely => Some(MS_PER_MINUTE),
            Freq::Hourly => Some(MS_PER_HOUR),
            Freq::Daily | Freq::Weekly | Freq::Monthly | Freq::Yearly => None,
        }
    }

    /// How many periods the walk may visit before giving up. Chosen so the caps
    /// cover at least a decade for every frequency: ~500k seconds is almost six
    /// days of steps but each step is one second, and the day-based frequencies
    /// get a decade or more directly.
    fn period_cap(self) -> u64 {
        match self {
            Freq::Secondly | Freq::Minutely | Freq::Hourly => 500_000,
            Freq::Daily => 3_700,
            Freq::Weekly => 1_000,
            Freq::Monthly => 1_200,
            Freq::Yearly => 500,
        }
    }
}

/// A day of the week. Monday is zero, matching both [`crate::civil`] and the
/// order `BYDAY` is usually written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Weekday(u8);

impl Weekday {
    pub const MO: Weekday = Weekday(0);
    pub const TU: Weekday = Weekday(1);
    pub const WE: Weekday = Weekday(2);
    pub const TH: Weekday = Weekday(3);
    pub const FR: Weekday = Weekday(4);
    pub const SA: Weekday = Weekday(5);
    pub const SU: Weekday = Weekday(6);

    pub fn from_index(index: u8) -> Option<Weekday> {
        if index < 7 {
            Some(Weekday(index))
        } else {
            None
        }
    }

    pub fn index(self) -> u8 {
        self.0
    }

    pub fn as_str(self) -> &'static str {
        match self.0 {
            0 => "MO",
            1 => "TU",
            2 => "WE",
            3 => "TH",
            4 => "FR",
            5 => "SA",
            _ => "SU",
        }
    }

    pub fn parse(text: &str) -> Option<Weekday> {
        match text.to_ascii_uppercase().as_str() {
            "MO" => Some(Weekday::MO),
            "TU" => Some(Weekday::TU),
            "WE" => Some(Weekday::WE),
            "TH" => Some(Weekday::TH),
            "FR" => Some(Weekday::FR),
            "SA" => Some(Weekday::SA),
            "SU" => Some(Weekday::SU),
            _ => None,
        }
    }
}

/// A recurrence rule. The public fields are the parsed parts, so a caller can
/// report what it understood without re-parsing the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub freq: Freq,
    /// How many `FREQ` periods between occurrences; at least one.
    pub interval: u32,
    /// Empty means the anchor's own weekday (weekly) or every day (daily).
    pub by_day: Vec<Weekday>,
    /// Empty means the anchor's hour.
    pub by_hour: Vec<u32>,
    /// Empty means the anchor's minute.
    pub by_minute: Vec<u32>,
    pub count: Option<u32>,
    pub until_ms: Option<i64>,
}

/// Parse a rule string. Free function as well as [`Rule::parse`] because the
/// registry and the CLI both want "just the rule" without naming the type.
pub fn parse_rule(text: &str) -> Result<Rule> {
    Rule::parse(text)
}

impl Rule {
    pub fn parse(text: &str) -> Result<Rule> {
        let body = rule_body(text);
        if body.is_empty() {
            return Err(invalid("rrule: the rule is empty"));
        }

        let mut freq = None;
        let mut interval = 1u32;
        let mut by_day = Vec::new();
        let mut by_hour = Vec::new();
        let mut by_minute = Vec::new();
        let mut count = None;
        let mut until_ms = None;
        let mut seen: Vec<String> = Vec::new();

        for part in body.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let Some((key, value)) = part.split_once('=') else {
                return Err(invalid(format!("rrule: {part:?} is not a KEY=VALUE part")));
            };
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return Err(invalid(format!("rrule: {part:?} is not a KEY=VALUE part")));
            }
            // Two of the same part is ambiguous, and choosing the last one
            // would make two clients disagree about the same text.
            if seen.iter().any(|other| other == &key) {
                return Err(invalid(format!("rrule: {key} appears twice")));
            }
            match key.as_str() {
                "FREQ" => {
                    freq = Some(
                        Freq::parse(value)
                            .ok_or_else(|| invalid(format!("rrule: unknown FREQ {value:?}")))?,
                    );
                }
                "INTERVAL" => {
                    let parsed = parse_number("INTERVAL", value)?;
                    if parsed == 0 {
                        return Err(invalid("rrule: INTERVAL must be at least 1"));
                    }
                    interval = parsed;
                }
                "BYDAY" => by_day = parse_by_day(value)?,
                "BYHOUR" => by_hour = parse_small_list("BYHOUR", value, 23)?,
                "BYMINUTE" => by_minute = parse_small_list("BYMINUTE", value, 59)?,
                "COUNT" => {
                    let parsed = parse_number("COUNT", value)?;
                    if parsed == 0 {
                        return Err(invalid("rrule: COUNT must be at least 1"));
                    }
                    count = Some(parsed);
                }
                "UNTIL" => until_ms = Some(parse_until(value)?),
                other => {
                    return Err(invalid(format!(
                        "rrule: {other} is not part of this subset"
                    )));
                }
            }
            seen.push(key);
        }

        let Some(freq) = freq else {
            return Err(invalid("rrule: FREQ is required"));
        };
        // The RFC forbids both, and silently preferring one of them would turn
        // one text into two schedules depending on who read it.
        if count.is_some() && until_ms.is_some() {
            return Err(invalid("rrule: COUNT and UNTIL cannot both be given"));
        }

        Ok(Rule {
            freq,
            interval,
            by_day,
            by_hour,
            by_minute,
            count,
            until_ms,
        })
    }

    /// The first occurrence strictly after `after_ms`, or `None` when the rule
    /// has no more (exhausted, past `UNTIL`, or outside the walk bound).
    pub fn next_after(&self, start_ms: i64, after_ms: i64) -> Option<i64> {
        let start = Start::parse(start_ms)?;
        self.walk(&start, after_ms)?.next()
    }

    /// Up to `limit` occurrences in ascending order, all strictly after
    /// `after_ms`. Always terminates: the walk is bounded by construction.
    pub fn occurrences(&self, start_ms: i64, after_ms: i64, limit: usize) -> Vec<i64> {
        let mut out = Vec::new();
        let limit = limit.min(MAX_OCCURRENCES);
        if limit == 0 {
            return out;
        }
        let Some(start) = Start::parse(start_ms) else {
            return out;
        };
        let Some(mut walk) = self.walk(&start, after_ms) else {
            return out;
        };
        while out.len() < limit {
            let Some(at) = walk.next() else {
                break;
            };
            out.push(at);
        }
        out
    }

    fn walk<'a>(&'a self, start: &Start, after_ms: i64) -> Option<Walk<'a>> {
        if self.interval == 0 {
            // Unreachable through `parse`, but a `Rule` built by hand must not
            // divide by zero.
            return None;
        }
        match self.freq {
            Freq::Secondly | Freq::Minutely | Freq::Hourly => {
                SubWalk::build(self, start, after_ms).map(Walk::Sub)
            }
            Freq::Daily | Freq::Weekly | Freq::Monthly | Freq::Yearly => {
                DayWalk::build(self, start, after_ms).map(Walk::Day)
            }
        }
    }
}

/// Splits the optional `RRULE:` property name off the front. Operators paste
/// the line out of a calendar file often enough that refusing it would be a
/// papercut, and the value after the colon is the whole rule.
fn rule_body(text: &str) -> &str {
    let trimmed = text.trim();
    match trimmed.split_once(':') {
        Some((name, value)) if name.trim().eq_ignore_ascii_case("RRULE") => value.trim(),
        _ => trimmed,
    }
}

fn parse_number(part: &str, value: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| invalid(format!("rrule: {part}={value:?} is not a number")))
}

fn parse_by_day(value: &str) -> Result<Vec<Weekday>> {
    let mut days = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(invalid(format!("rrule: BYDAY {value:?} has an empty day")));
        }
        // `1MO` and `-1FR` are the RFC's "first/last Monday of the month".
        // Accepting the text without that meaning would fire on the wrong day.
        let starts_with_number = item.as_bytes().first().is_some_and(u8::is_ascii_digit);
        if starts_with_number || item.starts_with('+') || item.starts_with('-') {
            return Err(invalid(format!(
                "rrule: BYDAY {item:?} uses an ordinal, which is not part of this subset"
            )));
        }
        let weekday = Weekday::parse(item)
            .ok_or_else(|| invalid(format!("rrule: unknown BYDAY {item:?}")))?;
        days.push(weekday);
    }
    days.sort();
    days.dedup();
    Ok(days)
}

fn parse_small_list(part: &str, value: &str, max: u32) -> Result<Vec<u32>> {
    let mut numbers = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        let number = parse_number(part, item)?;
        if number > max {
            return Err(invalid(format!(
                "rrule: {part}={item:?} is out of range 0..={max}"
            )));
        }
        numbers.push(number);
    }
    numbers.sort_unstable();
    numbers.dedup();
    Ok(numbers)
}

/// `UNTIL`, in either the date form (`YYYYMMDD`) or the UTC instant form
/// (`YYYYMMDDTHHMMSSZ`).
///
/// A date alone has no time of day, so it covers the whole of that UTC day:
/// end-of-day is the inclusive reading, and the one a reader of "until the
/// 20th" expects.
fn parse_until(text: &str) -> Result<i64> {
    let (date, time) = match text.split_once('T') {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let (year, month, day) = parse_date(date, text)?;
    let midnight = days_from_civil(year, month, day).saturating_mul(MS_PER_DAY);
    let Some(time) = time else {
        return Ok(midnight + MS_PER_DAY - 1);
    };
    let digits = time
        .strip_suffix('Z')
        .or_else(|| time.strip_suffix('z'))
        .ok_or_else(|| {
            invalid(format!(
                "rrule: UNTIL {text:?} must be a UTC instant ending in Z"
            ))
        })?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(format!(
            "rrule: UNTIL {text:?} must be YYYYMMDD or YYYYMMDDTHHMMSSZ"
        )));
    }
    let (hour, rest) = digits.split_at(2);
    let (minute, second) = rest.split_at(2);
    let hour: i64 = hour
        .parse()
        .map_err(|_| invalid(format!("rrule: UNTIL {text:?} has a bad hour")))?;
    let minute: i64 = minute
        .parse()
        .map_err(|_| invalid(format!("rrule: UNTIL {text:?} has a bad minute")))?;
    let second: i64 = second
        .parse()
        .map_err(|_| invalid(format!("rrule: UNTIL {text:?} has a bad second")))?;
    if hour > 23 || minute > 59 || second > 59 {
        return Err(invalid(format!(
            "rrule: UNTIL {text:?} has an out-of-range time"
        )));
    }
    Ok(midnight + hour * MS_PER_HOUR + minute * MS_PER_MINUTE + second * MS_PER_SECOND)
}

fn parse_date(text: &str, whole: &str) -> Result<(i64, u32, u32)> {
    if text.len() != 8 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(format!(
            "rrule: UNTIL {whole:?} is not YYYYMMDD or YYYYMMDDTHHMMSSZ"
        )));
    }
    let (year, rest) = text.split_at(4);
    let (month, day) = rest.split_at(2);
    let year: i64 = year
        .parse()
        .map_err(|_| invalid(format!("rrule: UNTIL {whole:?} has a bad year")))?;
    let month: u32 = month
        .parse()
        .map_err(|_| invalid(format!("rrule: UNTIL {whole:?} has a bad month")))?;
    let day: u32 = day
        .parse()
        .map_err(|_| invalid(format!("rrule: UNTIL {whole:?} has a bad day")))?;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return Err(invalid(format!(
            "rrule: UNTIL {whole:?} is not a real date"
        )));
    }
    if !in_supported_year(year) {
        return Err(invalid(format!(
            "rrule: UNTIL {whole:?} is outside the supported years"
        )));
    }
    Ok((year, month, day))
}

/// DTSTART, split into the parts the walk needs. The anchor supplies the parts
/// a rule leaves out: the hour and minute for the day walks, the weekday for a
/// weekly rule without `BYDAY`.
#[derive(Debug, Clone, Copy)]
struct Start {
    ms: i64,
    day: i64,
    year: i64,
    month: u32,
    dom: u32,
    weekday: u32,
    hour: u32,
    minute: u32,
    second: u32,
    milli: i64,
}

impl Start {
    fn parse(ms: i64) -> Option<Start> {
        let day = day_of(ms);
        let of_day = ms.rem_euclid(MS_PER_DAY);
        let (year, month, dom) = civil_from_days(day);
        if !in_supported_year(year) {
            return None;
        }
        Some(Start {
            ms,
            day,
            year,
            month,
            dom,
            weekday: weekday_from_days(day),
            hour: (of_day / MS_PER_HOUR) as u32,
            minute: ((of_day % MS_PER_HOUR) / MS_PER_MINUTE) as u32,
            second: ((of_day % MS_PER_MINUTE) / MS_PER_SECOND) as u32,
            milli: of_day % MS_PER_SECOND,
        })
    }

    /// The instant at a day and a millisecond-of-day slot. The anchor's seconds
    /// and milliseconds ride along, so a DTSTART at 09:00:30 produces 09:00:30
    /// occurrences rather than quietly rounding to the minute.
    fn at_slot(&self, day: i64, slot_ms: i64) -> i64 {
        day.saturating_mul(MS_PER_DAY) + slot_ms + self.milli_of_minute()
    }

    fn milli_of_minute(&self) -> i64 {
        i64::from(self.second) * MS_PER_SECOND + self.milli
    }
}

fn day_of(ms: i64) -> i64 {
    ms.div_euclid(MS_PER_DAY)
}

fn hour_of(ms: i64) -> u32 {
    (ms.rem_euclid(MS_PER_DAY) / MS_PER_HOUR) as u32
}

fn minute_of(ms: i64) -> u32 {
    ((ms.rem_euclid(MS_PER_DAY) % MS_PER_HOUR) / MS_PER_MINUTE) as u32
}

enum Walk<'a> {
    Sub(SubWalk<'a>),
    Day(DayWalk<'a>),
}

impl Walk<'_> {
    fn next(&mut self) -> Option<i64> {
        match self {
            Walk::Sub(walk) => walk.next(),
            Walk::Day(walk) => walk.next(),
        }
    }
}

/// `FREQ=SECONDLY|MINUTELY|HOURLY`: candidates are the anchor plus whole steps,
/// and `BY*` parts filter rather than expand.
///
/// The step index jumps straight to the first candidate at or after the cutoff,
/// so a task whose anchor is years old is still answered in constant time; the
/// step budget only matters when a filter rejects most candidates.
struct SubWalk<'a> {
    rule: &'a Rule,
    start_ms: i64,
    after_ms: i64,
    step_ms: i64,
    index: i64,
    remaining: Option<u32>,
    steps: u64,
    max_steps: u64,
    done: bool,
}

impl<'a> SubWalk<'a> {
    fn build(rule: &'a Rule, start: &Start, after_ms: i64) -> Option<SubWalk<'a>> {
        let step_ms = rule.freq.unit_ms()?.checked_mul(i64::from(rule.interval))?;
        // With COUNT the index is the occurrence number, which has to begin at
        // the anchor or the count would be wrong. Without it, begin where the
        // caller is looking.
        let index = if rule.count.is_some() || after_ms < start.ms {
            0
        } else {
            (after_ms - start.ms) / step_ms + 1
        };
        Some(SubWalk {
            rule,
            start_ms: start.ms,
            after_ms,
            step_ms,
            index,
            remaining: rule.count,
            steps: 0,
            max_steps: rule.freq.period_cap(),
            done: false,
        })
    }

    fn next(&mut self) -> Option<i64> {
        while !self.done {
            if self.remaining == Some(0) || self.steps >= self.max_steps {
                self.done = true;
                return None;
            }
            let at = self
                .index
                .checked_mul(self.step_ms)
                .and_then(|offset| self.start_ms.checked_add(offset))?;
            self.index += 1;
            self.steps += 1;
            if at > last_supported_ms() {
                self.done = true;
                return None;
            }
            if let Some(until) = self.rule.until_ms {
                if at > until {
                    self.done = true;
                    return None;
                }
            }
            if !self.matches(at) {
                continue;
            }
            if let Some(left) = self.remaining {
                self.remaining = Some(left - 1);
            }
            if at > self.after_ms {
                return Some(at);
            }
        }
        None
    }

    fn matches(&self, at: i64) -> bool {
        if !self.rule.by_hour.is_empty() && !self.rule.by_hour.contains(&hour_of(at)) {
            return false;
        }
        if !self.rule.by_minute.is_empty() && !self.rule.by_minute.contains(&minute_of(at)) {
            return false;
        }
        self.rule.by_day.is_empty()
            || self
                .rule
                .by_day
                .iter()
                .any(|day| u32::from(day.index()) == weekday_from_days(day_of(at)))
    }
}

/// `FREQ=DAILY|WEEKLY|MONTHLY|YEARLY`: candidates are whole days, grouped into
/// periods of `INTERVAL` base units. Within a period `BYDAY` expands the days
/// (weekly, monthly) or filters a single day (daily, yearly), and
/// `BYHOUR`/`BYMINUTE` expand the times of day.
struct DayWalk<'a> {
    rule: &'a Rule,
    start: Start,
    /// Milliseconds of day, ascending: the cartesian product of the hour and
    /// minute lists, defaulted from the anchor.
    times: Vec<i64>,
    by_day: Vec<u32>,
    after_ms: i64,
    period: i64,
    days: Vec<i64>,
    day_pos: usize,
    slot_pos: usize,
    remaining: Option<u32>,
    steps: u64,
    max_steps: u64,
    done: bool,
}

impl<'a> DayWalk<'a> {
    fn build(rule: &'a Rule, start: &Start, after_ms: i64) -> Option<DayWalk<'a>> {
        let hours = if rule.by_hour.is_empty() {
            vec![start.hour]
        } else {
            rule.by_hour.clone()
        };
        let minutes = if rule.by_minute.is_empty() {
            vec![start.minute]
        } else {
            rule.by_minute.clone()
        };
        let mut times = Vec::with_capacity(hours.len() * minutes.len());
        for hour in &hours {
            for minute in &minutes {
                times.push(i64::from(*hour) * MS_PER_HOUR + i64::from(*minute) * MS_PER_MINUTE);
            }
        }
        times.sort_unstable();
        times.dedup();

        let by_day: Vec<u32> = rule
            .by_day
            .iter()
            .map(|day| u32::from(day.index()))
            .collect();

        // COUNT is counted from the anchor, so that walk cannot skip ahead.
        // Otherwise start one period before the cutoff: one period of slack
        // because the period containing the cutoff may already be consumed.
        let period = if rule.count.is_some() || after_ms <= start.ms {
            0
        } else {
            (periods_before(rule, start, after_ms) - 1).max(0)
        };

        Some(DayWalk {
            rule,
            start: *start,
            times,
            by_day,
            after_ms,
            period,
            days: Vec::new(),
            day_pos: 0,
            slot_pos: 0,
            remaining: rule.count,
            steps: 0,
            max_steps: rule.freq.period_cap(),
            done: false,
        })
    }

    fn next(&mut self) -> Option<i64> {
        loop {
            if self.done {
                return None;
            }
            if self.remaining == Some(0) {
                self.done = true;
                return None;
            }
            if self.day_pos >= self.days.len() {
                if self.steps >= self.max_steps {
                    self.done = true;
                    return None;
                }
                self.load_period();
                self.period += 1;
                self.steps += 1;
                continue;
            }
            let day = self.days[self.day_pos];
            if self.slot_pos >= self.times.len() {
                self.day_pos += 1;
                self.slot_pos = 0;
                continue;
            }
            let at = self.start.at_slot(day, self.times[self.slot_pos]);
            self.slot_pos += 1;
            if at > last_supported_ms() {
                self.done = true;
                return None;
            }
            if let Some(until) = self.rule.until_ms {
                if at > until {
                    self.done = true;
                    return None;
                }
            }
            // A weekly period starts on the Monday of the anchor's week, so the
            // days before DTSTART are candidates of the period but not
            // occurrences of the rule.
            if at < self.start.ms {
                continue;
            }
            if let Some(left) = self.remaining {
                self.remaining = Some(left - 1);
            }
            if at > self.after_ms {
                return Some(at);
            }
        }
    }

    /// Fills `days` with the candidate days of the current period, ascending.
    fn load_period(&mut self) {
        self.days.clear();
        self.day_pos = 0;
        self.slot_pos = 0;
        let interval = i64::from(self.rule.interval);
        match self.rule.freq {
            Freq::Daily => {
                let day = self.start.day + self.period * interval;
                if self.by_day.is_empty() || self.by_day.contains(&weekday_from_days(day)) {
                    self.days.push(day);
                }
            }
            Freq::Weekly => {
                let week_start = self.start.day - i64::from(self.start.weekday);
                let week = week_start + self.period * 7 * interval;
                if self.by_day.is_empty() {
                    self.days.push(week + i64::from(self.start.weekday));
                } else {
                    for weekday in &self.by_day {
                        self.days.push(week + i64::from(*weekday));
                    }
                }
            }
            Freq::Monthly => {
                let index = month_index(self.start.year, self.start.month) + self.period * interval;
                let (year, month) = civil_from_month_index(index);
                let length = days_in_month(year, month);
                if self.by_day.is_empty() {
                    // "The 31st" in a 30-day month is that month's last day,
                    // not a skipped month: skipping would silently drop five
                    // occurrences a year from a schedule the operator wrote as
                    // "monthly".
                    self.days
                        .push(days_from_civil(year, month, self.start.dom.min(length)));
                } else {
                    let first = days_from_civil(year, month, 1);
                    for offset in 0..length {
                        let day = first + i64::from(offset);
                        if self.by_day.contains(&weekday_from_days(day)) {
                            self.days.push(day);
                        }
                    }
                }
            }
            Freq::Yearly => {
                let year = self.start.year + self.period * interval;
                // February 29 in a common year does not exist, and clamping it
                // to the 28th would turn "every leap day" into "every year".
                if self.start.dom <= days_in_month(year, self.start.month) {
                    let day = days_from_civil(year, self.start.month, self.start.dom);
                    if self.by_day.is_empty() || self.by_day.contains(&weekday_from_days(day)) {
                        self.days.push(day);
                    }
                }
            }
            Freq::Secondly | Freq::Minutely | Freq::Hourly => {}
        }
        self.days.sort_unstable();
    }
}

/// How many whole periods of `rule` fit between the anchor and `after_ms`.
fn periods_before(rule: &Rule, start: &Start, after_ms: i64) -> i64 {
    let interval = i64::from(rule.interval);
    match rule.freq {
        Freq::Daily => (day_of(after_ms) - start.day).div_euclid(interval),
        Freq::Weekly => {
            let week_start = start.day - i64::from(start.weekday);
            (day_of(after_ms) - week_start).div_euclid(7 * interval)
        }
        Freq::Monthly => {
            let (year, month, _) = civil_from_days(day_of(after_ms));
            (month_index(year, month) - month_index(start.year, start.month)).div_euclid(interval)
        }
        Freq::Yearly => {
            let (year, _, _) = civil_from_days(day_of(after_ms));
            (year - start.year).div_euclid(interval)
        }
        Freq::Secondly | Freq::Minutely | Freq::Hourly => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(text: &str) -> Rule {
        Rule::parse(text).expect(text)
    }

    macro_rules! rejected {
        ($text:expr, $needle:expr) => {{
            let error = Rule::parse($text).expect_err($text);
            assert!(
                error.message().contains($needle),
                "rule {:?}: {:?} does not mention {:?}",
                $text,
                error.message(),
                $needle
            );
        }};
    }

    #[test]
    fn a_rule_is_refused_when_a_part_is_missing_or_unknown() {
        rejected!("", "is empty");
        rejected!("   ", "is empty");
        rejected!("BYHOUR=9", "FREQ is required");
        rejected!("FREQ=FORTNIGHTLY", "unknown FREQ");
        rejected!("FREQ=DAILY;BYMONTH=3", "not part of this subset");
        rejected!("FREQ=DAILY;NOT_A_PART", "not a KEY=VALUE part");
        rejected!("FREQ=DAILY;FREQ=WEEKLY", "appears twice");
    }

    #[test]
    fn interval_and_count_must_be_positive() {
        rejected!("FREQ=DAILY;INTERVAL=0", "at least 1");
        rejected!("FREQ=DAILY;INTERVAL=x", "not a number");
        rejected!("FREQ=DAILY;COUNT=0", "at least 1");
    }

    #[test]
    fn ordinals_are_refused_rather_than_ignored() {
        rejected!("FREQ=MONTHLY;BYDAY=1MO", "ordinal");
        rejected!("FREQ=MONTHLY;BYDAY=-1FR", "ordinal");
        rejected!("FREQ=WEEKLY;BYDAY=+1MO", "ordinal");
        rejected!("FREQ=WEEKLY;BYDAY=XX", "unknown BYDAY");
    }

    #[test]
    fn parts_with_ranges_are_checked() {
        rejected!("FREQ=DAILY;BYHOUR=24", "out of range");
        rejected!("FREQ=DAILY;BYMINUTE=60", "out of range");
        rejected!("FREQ=DAILY;BYHOUR=-1", "not a number");
    }

    #[test]
    fn count_and_until_are_mutually_exclusive() {
        rejected!("FREQ=DAILY;COUNT=3;UNTIL=20261120", "cannot both be given");
        rejected!("FREQ=DAILY;UNTIL=20261120;COUNT=3", "cannot both be given");
    }

    #[test]
    fn until_dates_are_validated() {
        rejected!("FREQ=DAILY;UNTIL=20261301", "not a real date");
        rejected!("FREQ=DAILY;UNTIL=20260230", "not a real date");
        rejected!("FREQ=DAILY;UNTIL=2026112", "is not YYYYMMDD");
        rejected!("FREQ=DAILY;UNTIL=20261120T000000", "ending in Z");
        rejected!("FREQ=DAILY;UNTIL=20261120T250000Z", "out-of-range time");
    }

    #[test]
    fn a_date_only_until_is_the_end_of_that_day() {
        let parsed = rule("FREQ=DAILY;UNTIL=20261120");
        let instant = parsed.until_ms.expect("until is set");
        assert_eq!(
            instant,
            crate::civil::from_civil(2026, 11, 20, 23, 59, 59) + 999
        );
    }

    #[test]
    fn the_rule_text_may_carry_the_rrule_prefix_and_odd_whitespace() {
        assert_eq!(
            rule(" RRULE:FREQ=DAILY ; BYHOUR=9 "),
            rule("FREQ=DAILY;BYHOUR=9")
        );
        assert_eq!(rule("rrule:FREQ=DAILY"), rule("FREQ=DAILY"));
    }

    #[test]
    fn parts_are_case_insensitive_and_lists_are_sorted_and_deduped() {
        let parsed = rule("freq=weekly;byday=fr,mo,fr;byhour=17,9,9;byminute=30,0");
        assert_eq!(parsed.freq, Freq::Weekly);
        assert_eq!(parsed.by_day, vec![Weekday::MO, Weekday::FR]);
        assert_eq!(parsed.by_hour, vec![9, 17]);
        assert_eq!(parsed.by_minute, vec![0, 30]);
    }

    #[test]
    fn every_frequency_has_a_name_and_round_trips() {
        for freq in [
            Freq::Secondly,
            Freq::Minutely,
            Freq::Hourly,
            Freq::Daily,
            Freq::Weekly,
            Freq::Monthly,
            Freq::Yearly,
        ] {
            assert_eq!(Freq::parse(freq.as_str()), Some(freq));
        }
        assert_eq!(Freq::parse("hourly"), Some(Freq::Hourly));
        assert_eq!(Freq::parse("FORTNIGHTLY"), None);
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
            assert_eq!(Weekday::parse(name), Some(weekday));
        }
        assert_eq!(Weekday::from_index(7), None);
        assert_eq!(Weekday::parse("XX"), None);
    }

    #[test]
    fn a_rule_built_by_hand_with_a_zero_interval_answers_nothing() {
        let broken = Rule {
            freq: Freq::Daily,
            interval: 0,
            by_day: Vec::new(),
            by_hour: Vec::new(),
            by_minute: Vec::new(),
            count: None,
            until_ms: None,
        };
        assert_eq!(broken.next_after(0, 0), None);
        assert_eq!(broken.occurrences(0, 0, 10), Vec::<i64>::new());
    }

    #[test]
    fn a_limit_of_zero_asks_for_nothing() {
        assert_eq!(rule("FREQ=DAILY").occurrences(0, 0, 0), Vec::<i64>::new());
    }

    #[test]
    fn an_anchor_outside_the_supported_years_answers_nothing() {
        // Year 10000, one millennium past the supported range.
        let far = 253_402_300_800_000;
        assert_eq!(rule("FREQ=DAILY").next_after(far, far), None);
        assert_eq!(
            rule("FREQ=DAILY").occurrences(far, far, 5),
            Vec::<i64>::new()
        );
    }
}
