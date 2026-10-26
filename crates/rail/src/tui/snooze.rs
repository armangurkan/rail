//! The snooze chooser's times: the choices it offers, and the times that can be typed into it
//! (the chooser is a picker, so this sits beside the clock rather than in the mail screen).
//! Both are wall-clock times in the clock's zone, resolved to the Unix seconds the daemon keeps,
//! and both come from the `Clock` alone, so tests fix the time and the zone.

use jiff::civil::{Date, Weekday};
use jiff::{ToSpan, Zoned};
use rail_core::ops::{MAX_SNOOZE_SECS, MIN_SNOOZE_SECS};

use crate::tui::clock::Clock;

/// What the chooser shows for text it cannot read as a time.
pub const TRY: &str = "Try 6pm, tomorrow 9am, 2 days, nov 3 or fri.";
/// What it shows for a time that is not at least a minute ahead.
pub const PASSED: &str = "That time has passed.";
/// What it shows for a time the daemon would refuse as too far ahead.
pub const TOO_FAR: &str = "That is more than a year away.";
/// What a time too close to now says: the daemon would refuse it by the time it arrived.
pub const SOON: &str = "That is too soon.";
/// Seconds past the daemon's minute that a time must be: its clock may tick on, or past a second,
/// before the request arrives.
const SLACK_SECS: i64 = 15;

/// The hour a morning choice, and a typed day without a time, come back at.
const MORNING: i8 = 8;
/// The hour "Later today" comes back at.
const EVENING: i8 = 18;
/// The hour "tonight" means.
const NIGHT: i8 = 20;
/// "Later today" is offered before this hour only: later, an hour's snooze is not worth one.
const LATE: i8 = 17;
/// The most digits a typed span (`+3h`) has, so its arithmetic cannot overflow.
const SPAN_DIGITS: usize = 4;
/// The days a typed weekday names.
const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
];
/// The months a typed date names: three letters of the name or more (`nov`, `sept`).
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// One of the chooser's choices: its label and the time it stands for (Unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnoozeChoice {
    pub label: &'static str,
    pub at: i64,
}

/// The choices offered now: later today at 18:00 (before 17:00), tomorrow at 08:00, this
/// weekend (Saturday 08:00, Monday to Friday) and next week (Monday 08:00). A choice whose time
/// an earlier one already offers is dropped: on a Friday "This weekend" is "Tomorrow".
pub fn snooze_choices(clock: &Clock) -> Vec<SnoozeChoice> {
    let Some(now) = clock.zoned(clock.now()) else {
        return Vec::new();
    };
    let today = now.date();
    let weekday = !matches!(today.weekday(), Weekday::Saturday | Weekday::Sunday);
    let saturday = weekday.then(|| today.nth_weekday(1, Weekday::Saturday).ok());
    let offered = [
        ("Later today", (now.hour() < LATE).then_some(today), EVENING),
        ("Tomorrow", today.tomorrow().ok(), MORNING),
        ("This weekend", saturday.flatten(), MORNING),
        (
            "Next week",
            today.nth_weekday(1, Weekday::Monday).ok(),
            MORNING,
        ),
    ];
    let mut choices: Vec<SnoozeChoice> = Vec::new();
    for (label, day, hour) in offered {
        let Some(at) = day.and_then(|day| local(clock, day, hour, 0)) else {
            continue;
        };
        if choices.iter().all(|choice| choice.at != at) {
            choices.push(SnoozeChoice { label, at });
        }
    }
    choices
}

/// The time `text` names, or the message to show instead. Whatever `time` is set to, it reads:
/// a time of day (`18:00`, `6pm`, `6:30 pm`, `noon`: today, or tomorrow once it has passed); a
/// span (`3 days`, `in 2 hours`, `an hour`, `+3h`); a named day (`tomorrow`, `tonight`, `later
/// today`, `this weekend`, `next week`, `fri`) or a date (`nov 3`, `3 nov 2026`, `2026-11-03`);
/// and a day with a time in either order (`tomorrow 9am`, `9am fri`, `nov 3 at 14:00`). A day
/// alone comes back at its own hour: 08:00, `tonight` 20:00, `today` and `later today` 18:00.
/// The time must be between a minute and a year ahead, as the daemon requires.
pub fn parse_snooze(text: &str, clock: &Clock) -> Result<i64, &'static str> {
    let text = text.to_ascii_lowercase().replace(',', " ");
    let words: Vec<&str> = text.split_whitespace().collect();
    let now = clock.zoned(clock.now()).ok_or(TRY)?;
    let at = span(&words, &now)
        .or_else(|| moment(&words, now.date(), clock))
        .ok_or(TRY)?;
    ahead(at, clock.now())
}

/// `at` as a snooze's end at `now`: passed, too soon (the daemon wants a minute, and the request
/// takes a moment to reach it), more than a year ahead, or fine. The chooser asks again at Enter.
pub fn ahead(at: i64, now: i64) -> Result<i64, &'static str> {
    if at <= now {
        Err(PASSED)
    } else if at < now.saturating_add(MIN_SNOOZE_SECS + SLACK_SECS) {
        Err(SOON)
    } else if at > now.saturating_add(MAX_SNOOZE_SECS) {
        Err(TOO_FAR)
    } else {
        Ok(at)
    }
}

/// `day` at `hour`:`minute` in the clock's zone, as Unix seconds. A time a daylight-saving change
/// skips moves past the gap, by jiff's compatible rule.
fn local(clock: &Clock, day: Date, hour: i8, minute: i8) -> Option<i64> {
    let zoned = day
        .at(hour, minute, 0, 0)
        .to_zoned(clock.zone().clone())
        .ok()?;
    Some(zoned.timestamp().as_second())
}

/// The first of `dates` at `time` that is still ahead, or the last of them when none is (the
/// caller then says the time has passed).
fn first_ahead(clock: &Clock, dates: &[Date], (hour, minute): (i8, i8)) -> Option<i64> {
    let times: Vec<i64> = dates
        .iter()
        .filter_map(|day| local(clock, *day, hour, minute))
        .collect();
    let ahead = times.iter().copied().find(|at| *at > clock.now());
    ahead.or_else(|| times.last().copied())
}

/// `3 days`, `in 2 hours`, `an hour`, `90 min`, `+3h`, `1 month`: minutes and hours of elapsed
/// time, or calendar days, weeks and months, which keep the time of day across a daylight-saving
/// change.
fn span(words: &[&str], now: &Zoned) -> Option<i64> {
    let words = match words {
        ["in", rest @ ..] => rest,
        _ => words,
    };
    let (count, unit) = match words {
        ["a" | "an", unit] => (1, *unit),
        [count, unit] => (count_of(count.strip_prefix('+').unwrap_or(count))?, *unit),
        [joined] => {
            let joined = joined.strip_prefix('+').unwrap_or(joined);
            let digits = joined.bytes().take_while(u8::is_ascii_digit).count();
            let (count, unit) = joined.split_at(digits);
            (count_of(count)?, unit)
        }
        _ => return None,
    };
    let span = match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => count.minutes(),
        "h" | "hr" | "hrs" | "hour" | "hours" => count.hours(),
        "d" | "day" | "days" => count.days(),
        "w" | "wk" | "wks" | "week" | "weeks" => count.weeks(),
        "mo" | "month" | "months" => count.months(),
        _ => return None,
    };
    Some(now.checked_add(span).ok()?.timestamp().as_second())
}

/// One to four ASCII digits, so a span's arithmetic cannot overflow.
fn count_of(text: &str) -> Option<i64> {
    let digits =
        (1..=SPAN_DIGITS).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit());
    if digits { text.parse().ok() } else { None }
}

/// A time alone (today, or tomorrow once it has passed), or a day with or without a time, in
/// either order, an `at` allowed before a time that follows the day.
fn moment(words: &[&str], today: Date, clock: &Clock) -> Option<i64> {
    if let Some(time) = time_of(words) {
        return first_ahead(clock, &[today, today.tomorrow().ok()?], time);
    }
    (1..=words.len()).find_map(|split| {
        let (head, tail) = words.split_at(split);
        let after = match tail {
            ["at", rest @ ..] => rest,
            _ => tail,
        };
        on_day(head, after, today, clock)
            .or_else(|| (!tail.is_empty()).then(|| on_day(tail, head, today, clock))?)
    })
}

/// The days `day` names, at `time` or, with none, at the day's own hour.
fn on_day(day: &[&str], time: &[&str], today: Date, clock: &Clock) -> Option<i64> {
    let (dates, hour) = days(day, today)?;
    let time = if time.is_empty() {
        (hour, 0)
    } else {
        time_of(time)?
    };
    first_ahead(clock, &dates, time)
}

/// The days `words` name, soonest first (the first whose time is still ahead is meant), and the
/// hour a day named alone comes back at.
fn days(words: &[&str], today: Date) -> Option<(Vec<Date>, i8)> {
    let found = match words {
        ["today" | "later"] | ["later", "today"] => (vec![today], EVENING),
        ["tonight"] => (vec![today], NIGHT),
        ["tomorrow"] => (vec![today.tomorrow().ok()?], MORNING),
        ["weekend"] | ["this" | "the", "weekend"] => (weekly(today, Weekday::Saturday)?, MORNING),
        ["next", "week"] => (vec![today.nth_weekday(1, Weekday::Monday).ok()?], MORNING),
        // `next` skips today when today is that day; `this` and the bare day count it.
        ["next", day] if weekday(day).is_some() => {
            (vec![today.nth_weekday(1, weekday(day)?).ok()?], MORNING)
        }
        ["this", day] | [day] if weekday(day).is_some() => (weekly(today, weekday(day)?)?, MORNING),
        [date] if civil_date(date).is_some() => (vec![civil_date(date)?], MORNING),
        _ => (month_date(words, today)?, MORNING),
    };
    Some(found)
}

/// A weekday by three letters of its name or more (`tue`, `tues`, `tuesday`).
fn weekday(word: &str) -> Option<Weekday> {
    if word.len() < 3 {
        return None;
    }
    WEEKDAYS
        .iter()
        .find(|(name, _)| name.starts_with(word))
        .map(|(_, day)| *day)
}

/// The coming `day` (today, when today is one) and the one a week later.
fn weekly(today: Date, day: Weekday) -> Option<Vec<Date>> {
    let first = if today.weekday() == day {
        today
    } else {
        today.nth_weekday(1, day).ok()?
    };
    Some(vec![first, first.checked_add(1.week()).ok()?])
}

/// `nov 3`, `3 nov` or `november 3rd`, with or without a year: without one it is this year's, or
/// next year's once this year's has passed.
fn month_date(words: &[&str], today: Date) -> Option<Vec<Date>> {
    let ((month, day), year) = match words {
        [a, b] => (month_day(a, b)?, None),
        [a, b, year] => (month_day(a, b)?, Some(year_of(year)?)),
        _ => return None,
    };
    let years = match year {
        Some(year) => vec![year],
        None => vec![today.year(), today.year().checked_add(1)?],
    };
    let dates: Vec<Date> = years
        .into_iter()
        .filter_map(|year| Date::new(year, month, day).ok())
        .collect();
    (!dates.is_empty()).then_some(dates)
}

/// A month's name and a day of the month, in either order.
fn month_day(a: &str, b: &str) -> Option<(i8, i8)> {
    month_of(a)
        .zip(ordinal(b))
        .or_else(|| month_of(b).zip(ordinal(a)))
}

/// A month by three letters of its name or more (`nov`, `sept`, `november`).
fn month_of(word: &str) -> Option<i8> {
    if word.len() < 3 {
        return None;
    }
    let index = MONTHS.iter().position(|name| name.starts_with(word))?;
    i8::try_from(index + 1).ok()
}

/// A day of the month: `3`, `03` or `3rd`.
fn ordinal(word: &str) -> Option<i8> {
    let digits = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suffix| word.strip_suffix(suffix))
        .unwrap_or(word);
    number(digits)
}

/// A four-digit year.
fn year_of(word: &str) -> Option<i16> {
    let digits = word.len() == 4 && word.bytes().all(|b| b.is_ascii_digit());
    if digits { word.parse().ok() } else { None }
}

/// A `YYYY-MM-DD` date that exists; nothing looser.
fn civil_date(text: &str) -> Option<Date> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(at, byte)| match at {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !shaped {
        return None;
    }
    let (year, month, day) = (&text[..4], &text[5..7], &text[8..]);
    Date::new(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?).ok()
}

/// `18:00`, `6pm`, `6:30 pm`, `noon` or `midnight`: the hour and minute.
fn time_of(words: &[&str]) -> Option<(i8, i8)> {
    match words {
        [] => None,
        ["noon"] => Some((12, 0)),
        ["midnight"] => Some((0, 0)),
        _ => time_of_day(&words.join(" ")),
    }
}

/// `18:00`, `6pm` or `6:30 pm`: the hour and minute. A bare number is not a time.
fn time_of_day(text: &str) -> Option<(i8, i8)> {
    let (digits, half) = if let Some(rest) = text.strip_suffix("am") {
        (rest.trim_end(), Some(0))
    } else if let Some(rest) = text.strip_suffix("pm") {
        (rest.trim_end(), Some(12))
    } else {
        (text, None)
    };
    let (hour, minute) = match digits.split_once(':') {
        Some((hour, minute)) if minute.len() == 2 => (number(hour)?, number(minute)?),
        None if half.is_some() => (number(digits)?, 0),
        _ => return None,
    };
    let hour = match half {
        None if hour < 24 => hour,
        Some(half) if (1..=12).contains(&hour) => hour % 12 + half,
        _ => return None,
    };
    (minute < 60).then_some((hour, minute))
}

/// One or two ASCII digits.
fn number(text: &str) -> Option<i8> {
    let digits = (1..=2).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit());
    if digits { text.parse().ok() } else { None }
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;

    use super::*;

    const NOW: i64 = 1_760_100_000; // Friday 2025-10-10 12:40 UTC
    const MIDNIGHT: i64 = NOW - 45_600; // 2025-10-10 00:00 UTC
    const DAY: i64 = 86_400;
    const HOUR: i64 = 3_600;

    fn utc(now: i64) -> Clock {
        Clock::fixed(now, TimeZone::UTC)
    }

    /// UTC+1, and UTC+2 in summer until 03:00 on the second Sunday of October (12 Oct 2025): a
    /// daylight-saving change two days after `NOW`, with no time zone database needed.
    fn shifting() -> Clock {
        Clock::fixed(NOW, TimeZone::posix("XST-1XDT,M3.5.0,M10.2.0/3").unwrap())
    }

    fn offered(clock: &Clock) -> Vec<(&'static str, i64)> {
        snooze_choices(clock)
            .into_iter()
            .map(|c| (c.label, c.at))
            .collect()
    }

    #[test]
    fn a_friday_afternoon_offers_later_today_tomorrow_and_next_week() {
        assert_eq!(
            offered(&utc(NOW)),
            [
                ("Later today", MIDNIGHT + 18 * HOUR),
                ("Tomorrow", MIDNIGHT + DAY + 8 * HOUR),
                ("Next week", MIDNIGHT + 3 * DAY + 8 * HOUR),
            ],
            "on a Friday this weekend is tomorrow, so it is dropped"
        );
    }

    #[test]
    fn later_today_is_offered_before_five_only() {
        let before = offered(&utc(MIDNIGHT + 17 * HOUR - 60));
        assert_eq!(before[0], ("Later today", MIDNIGHT + 18 * HOUR));
        let at_five = offered(&utc(MIDNIGHT + 17 * HOUR));
        assert_eq!(at_five[0], ("Tomorrow", MIDNIGHT + DAY + 8 * HOUR));
    }

    #[test]
    fn this_weekend_is_offered_monday_to_friday_and_a_repeated_time_is_dropped() {
        let wednesday = MIDNIGHT - 2 * DAY + 10 * HOUR;
        assert_eq!(
            offered(&utc(wednesday)),
            [
                ("Later today", MIDNIGHT - 2 * DAY + 18 * HOUR),
                ("Tomorrow", MIDNIGHT - DAY + 8 * HOUR),
                ("This weekend", MIDNIGHT + DAY + 8 * HOUR),
                ("Next week", MIDNIGHT + 3 * DAY + 8 * HOUR),
            ]
        );
        let saturday = MIDNIGHT + DAY + 10 * HOUR;
        assert_eq!(
            offered(&utc(saturday)),
            [
                ("Later today", MIDNIGHT + DAY + 18 * HOUR),
                ("Tomorrow", MIDNIGHT + 2 * DAY + 8 * HOUR),
                ("Next week", MIDNIGHT + 3 * DAY + 8 * HOUR),
            ]
        );
        let sunday = MIDNIGHT + 2 * DAY + 10 * HOUR;
        assert_eq!(
            offered(&utc(sunday)),
            [
                ("Later today", MIDNIGHT + 2 * DAY + 18 * HOUR),
                ("Tomorrow", MIDNIGHT + 3 * DAY + 8 * HOUR),
            ],
            "on a Sunday next week is tomorrow"
        );
        let monday_evening = MIDNIGHT + 3 * DAY + 17 * HOUR + 30 * 60;
        assert_eq!(
            offered(&utc(monday_evening)),
            [
                ("Tomorrow", MIDNIGHT + 4 * DAY + 8 * HOUR),
                ("This weekend", MIDNIGHT + 8 * DAY + 8 * HOUR),
                ("Next week", MIDNIGHT + 10 * DAY + 8 * HOUR),
            ],
            "on a Monday next week is seven days on"
        );
    }

    #[test]
    fn the_choices_are_wall_clock_times_across_a_daylight_saving_change() {
        // Now is Fri 14:40 at UTC+2; by Monday the zone is at UTC+1.
        assert_eq!(
            offered(&shifting()),
            [
                ("Later today", MIDNIGHT + 16 * HOUR),
                ("Tomorrow", MIDNIGHT + DAY + 6 * HOUR),
                ("Next week", MIDNIGHT + 3 * DAY + 7 * HOUR),
            ]
        );
    }

    fn parses(clock: &Clock, cases: &[(&str, i64)]) {
        for (text, at) in cases {
            assert_eq!(parse_snooze(text, clock), Ok(*at), "{text:?}");
        }
    }

    #[test]
    fn a_time_of_day_is_today_or_tomorrow_once_it_has_passed() {
        parses(
            &utc(NOW),
            &[
                ("18:00", MIDNIGHT + 18 * HOUR),
                ("6pm", MIDNIGHT + 18 * HOUR),
                ("6:30 pm", MIDNIGHT + 18 * HOUR + 30 * 60),
                (" 6:30PM ", MIDNIGHT + 18 * HOUR + 30 * 60),
                ("12:42", NOW + 120),
                ("12:45pm", MIDNIGHT + 12 * HOUR + 45 * 60),
                ("12:40", MIDNIGHT + DAY + 12 * HOUR + 40 * 60),
                ("9:05", MIDNIGHT + DAY + 9 * HOUR + 5 * 60),
                ("06:00", MIDNIGHT + DAY + 6 * HOUR),
                ("12pm", MIDNIGHT + DAY + 12 * HOUR),
                ("12am", MIDNIGHT + DAY),
                ("00:00", MIDNIGHT + DAY),
            ],
        );
    }

    #[test]
    fn dates_weekdays_and_spans_resolve_in_the_clocks_zone() {
        parses(
            &utc(NOW),
            &[
                ("2025-11-03", MIDNIGHT + 24 * DAY + 8 * HOUR),
                ("2025-11-03 09:30", MIDNIGHT + 24 * DAY + 9 * HOUR + 30 * 60),
                (
                    "2025-11-03 9:30 pm",
                    MIDNIGHT + 24 * DAY + 21 * HOUR + 30 * 60,
                ),
                ("+3h", NOW + 3 * HOUR),
                ("+2d", NOW + 2 * DAY),
                ("+1w", NOW + 7 * DAY),
                ("+52w", NOW + 364 * DAY),
                ("+366d", NOW + 366 * DAY),
                ("mon", MIDNIGHT + 3 * DAY + 8 * HOUR),
                ("Tues", MIDNIGHT + 4 * DAY + 8 * HOUR),
                ("thursday", MIDNIGHT + 6 * DAY + 8 * HOUR),
                ("fri", MIDNIGHT + 7 * DAY + 8 * HOUR),
            ],
        );
        parses(
            &shifting(),
            &[
                ("18:00", MIDNIGHT + 16 * HOUR),
                ("+3h", NOW + 3 * HOUR),
                ("+2d", NOW + 2 * DAY + HOUR),
                ("mon", MIDNIGHT + 3 * DAY + 7 * HOUR),
            ],
        );
    }

    #[test]
    fn a_time_not_ahead_or_too_far_ahead_says_why() {
        let clock = utc(NOW);
        for text in ["2025-10-01", "2025-10-10", "2025-10-10 09:00", "+0h"] {
            assert_eq!(parse_snooze(text, &clock), Err(PASSED), "{text:?}");
        }
        for text in ["2026-11-03", "+53w", "+367d", "+9999d"] {
            assert_eq!(parse_snooze(text, &clock), Err(TOO_FAR), "{text:?}");
        }
    }

    #[test]
    fn next_and_a_weekday_named_on_that_weekday_means_the_week_after() {
        // Monday 2025-10-13 07:00 UTC.
        let monday = utc(MIDNIGHT + 3 * DAY + 7 * HOUR);
        parses(
            &monday,
            &[
                ("next monday", MIDNIGHT + 10 * DAY + 8 * HOUR),
                ("next mon 9am", MIDNIGHT + 10 * DAY + 9 * HOUR),
                ("monday", MIDNIGHT + 3 * DAY + 8 * HOUR),
                ("this monday", MIDNIGHT + 3 * DAY + 8 * HOUR),
            ],
        );
    }

    #[test]
    fn a_time_a_daylight_saving_change_skips_moves_past_the_gap() {
        // Saturday 2026-03-28 12:00 XST (UTC+1); that night clocks go from 02:00 to 03:00 XDT.
        let zone = TimeZone::posix("XST-1XDT,M3.5.0,M10.2.0/3").unwrap();
        let clock = Clock::fixed(1_774_695_600, zone);
        // 02:30 does not exist then: it reads as 03:30 XDT, which is 01:30 UTC.
        assert_eq!(parse_snooze("tomorrow 2:30am", &clock), Ok(1_774_747_800));
    }

    #[test]
    fn the_margin_over_the_daemons_minute_is_fifteen_seconds() {
        assert_eq!(ahead(NOW + 74, NOW), Err(SOON));
        assert_eq!(ahead(NOW + 75, NOW), Ok(NOW + 75));
    }

    #[test]
    fn a_time_the_daemon_would_find_under_a_minute_away_is_too_soon() {
        let clock = utc(NOW);
        for text in ["+1m", "1 min", "a minute", "12:41"] {
            assert_eq!(parse_snooze(text, &clock), Err(SOON), "{text:?}");
        }
        assert_eq!(parse_snooze("+2m", &clock), Ok(NOW + 120));
    }

    #[test]
    fn text_that_is_not_a_time_gets_the_examples() {
        let clock = utc(NOW);
        let garbage = [
            "",
            "  ",
            "soon",
            "tu",
            "in",
            "next",
            "nov",
            "nov 32",
            "feb 29",
            "3 parsecs",
            "tomorrow tomorrow",
            "9am 10am",
            "18",
            "25:00",
            "24:00",
            "13pm",
            "0am",
            "6:5",
            "6:005",
            "+",
            "+h",
            "+3x",
            "+-3h",
            "+12345h",
            "+3é",
            "é",
            "2025-02-30",
            "2025-13-01",
            "2025-1-1",
        ];
        for text in garbage {
            assert_eq!(parse_snooze(text, &clock), Err(TRY), "{text:?}");
        }
    }

    #[test]
    fn spans_in_words_count_from_now() {
        parses(
            &utc(NOW),
            &[
                ("3 days", NOW + 3 * DAY),
                ("in 3 days", NOW + 3 * DAY),
                ("3d", NOW + 3 * DAY),
                ("3days", NOW + 3 * DAY),
                ("2 hours", NOW + 2 * HOUR),
                ("in 2 hrs", NOW + 2 * HOUR),
                ("an hour", NOW + HOUR),
                ("in a day", NOW + DAY),
                ("90 min", NOW + 90 * 60),
                ("30 minutes", NOW + 30 * 60),
                ("2 weeks", NOW + 14 * DAY),
                ("1 month", NOW + 31 * DAY),
            ],
        );
    }

    #[test]
    fn named_days_take_their_own_hour_or_the_time_given() {
        parses(
            &utc(NOW),
            &[
                ("tomorrow", MIDNIGHT + DAY + 8 * HOUR),
                ("Tomorrow 9am", MIDNIGHT + DAY + 9 * HOUR),
                ("tomorrow at 9:30 pm", MIDNIGHT + DAY + 21 * HOUR + 30 * 60),
                ("9am tomorrow", MIDNIGHT + DAY + 9 * HOUR),
                ("tonight", MIDNIGHT + 20 * HOUR),
                ("later today", MIDNIGHT + 18 * HOUR),
                ("today 5pm", MIDNIGHT + 17 * HOUR),
                ("this weekend", MIDNIGHT + DAY + 8 * HOUR),
                ("weekend", MIDNIGHT + DAY + 8 * HOUR),
                ("next week", MIDNIGHT + 3 * DAY + 8 * HOUR),
                ("next week 10am", MIDNIGHT + 3 * DAY + 10 * HOUR),
                ("next monday", MIDNIGHT + 3 * DAY + 8 * HOUR),
                ("mon 9:00", MIDNIGHT + 3 * DAY + 9 * HOUR),
                ("monday 9:30", MIDNIGHT + 3 * DAY + 9 * HOUR + 30 * 60),
                ("fri 5pm", MIDNIGHT + 17 * HOUR),
                ("noon", MIDNIGHT + DAY + 12 * HOUR),
                ("midnight", MIDNIGHT + DAY),
            ],
        );
        assert_eq!(
            parse_snooze("today 9am", &utc(NOW)),
            Err(PASSED),
            "a named day is that day"
        );
    }

    #[test]
    fn a_month_date_is_this_years_or_next_years_once_it_has_passed() {
        let nov_3 = MIDNIGHT + 24 * DAY + 8 * HOUR;
        parses(
            &utc(NOW),
            &[
                ("nov 3", nov_3),
                ("november 3", nov_3),
                ("3 nov", nov_3),
                ("nov 3rd", nov_3),
                ("nov 3, 2025", nov_3),
                ("nov 3 2pm", nov_3 + 6 * HOUR),
                ("3 nov at 9:30", nov_3 + 90 * 60),
                ("oct 10 2pm", MIDNIGHT + 14 * HOUR),
                ("oct 1", MIDNIGHT + 356 * DAY + 8 * HOUR),
                ("oct 10", MIDNIGHT + 365 * DAY + 8 * HOUR),
            ],
        );
    }

    #[test]
    fn words_keep_the_wall_clock_across_a_daylight_saving_change() {
        // Now is Fri 14:40 at UTC+2; by Monday the zone is at UTC+1.
        parses(
            &shifting(),
            &[
                ("tomorrow 9am", MIDNIGHT + DAY + 7 * HOUR),
                ("3 days", NOW + 3 * DAY + HOUR),
                ("next week", MIDNIGHT + 3 * DAY + 7 * HOUR),
            ],
        );
    }
}
