//! The snooze chooser's times: the choices it offers, and the times that can be typed into it
//! (the chooser is a picker, so this sits beside the clock rather than in the mail screen).
//! Both are wall-clock times in the clock's zone, resolved to the Unix seconds the daemon keeps,
//! and both come from the `Clock` alone, so tests fix the time and the zone.

use jiff::civil::{Date, Weekday};
use jiff::{ToSpan, Zoned};
use rail_core::ops::{MAX_SNOOZE_SECS, MIN_SNOOZE_SECS};

use crate::tui::clock::Clock;

/// What the chooser shows for text it cannot read as a time.
pub const TRY: &str = "Try 18:00, 6pm, 2026-11-03 09:30, +3h, +2d or mon.";
/// What it shows for a time that is not at least a minute ahead.
pub const PASSED: &str = "That time has passed.";
/// What it shows for a time the daemon would refuse as too far ahead.
pub const TOO_FAR: &str = "That is more than a year away.";

/// The hour a morning choice, and a typed day without a time, come back at.
const MORNING: i8 = 8;
/// The hour "Later today" comes back at.
const EVENING: i8 = 18;
/// "Later today" is offered before this hour only: later, an hour's snooze is not worth one.
const LATE: i8 = 17;
/// The most digits a typed span (`+3h`) has, so its arithmetic cannot overflow.
const SPAN_DIGITS: usize = 4;
/// The days a typed weekday names: three letters of the name or more (`tue`, `tues`).
const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
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

/// The time `text` names, or the message to show instead. It reads, whatever `time` is set to:
/// a time of day (`18:00`, `6pm`, `6:30 pm`: today, or tomorrow once it has passed), a date
/// (`2026-11-03` at 08:00, or with a time), a span (`+3h`, `+2d`, `+1w`) and a weekday (`mon`:
/// the next one, at 08:00). The time must be between a minute and a year ahead, as the daemon
/// requires.
pub fn parse_snooze(text: &str, clock: &Clock) -> Result<i64, &'static str> {
    let text = text.trim().to_ascii_lowercase();
    let now = clock.zoned(clock.now()).ok_or(TRY)?;
    let at = span(&text, &now)
        .or_else(|| weekday(&text, &now, clock))
        .or_else(|| dated(&text, clock))
        .or_else(|| time_of_day(&text).and_then(|(hour, minute)| next(clock, &now, hour, minute)))
        .ok_or(TRY)?;
    let now = clock.now();
    if at < now + MIN_SNOOZE_SECS {
        Err(PASSED)
    } else if at > now + MAX_SNOOZE_SECS {
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

/// `hour`:`minute` today, or tomorrow once today's has passed.
fn next(clock: &Clock, now: &Zoned, hour: i8, minute: i8) -> Option<i64> {
    let today = local(clock, now.date(), hour, minute)?;
    if today > clock.now() {
        return Some(today);
    }
    local(clock, now.date().tomorrow().ok()?, hour, minute)
}

/// `+3h`, `+2d` or `+1w`: hours of elapsed time, or calendar days and weeks, which keep the time
/// of day across a daylight-saving change.
fn span(text: &str, now: &Zoned) -> Option<i64> {
    let rest = text.strip_prefix('+')?;
    let unit = rest.chars().last()?;
    let count = &rest[..rest.len() - unit.len_utf8()];
    if count.is_empty() || count.len() > SPAN_DIGITS || !count.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let count: i64 = count.parse().ok()?;
    let later = match unit {
        'h' => now.checked_add(count.hours()),
        'd' => now.checked_add(count.days()),
        'w' => now.checked_add(count.weeks()),
        _ => return None,
    };
    Some(later.ok()?.timestamp().as_second())
}

/// `mon` to `sun`, or more of the day's name: the next such day after today, at 08:00.
fn weekday(text: &str, now: &Zoned, clock: &Clock) -> Option<i64> {
    if text.len() < 3 {
        return None;
    }
    let (_, day) = WEEKDAYS.iter().find(|(name, _)| name.starts_with(text))?;
    local(clock, now.date().nth_weekday(1, *day).ok()?, MORNING, 0)
}

/// `2026-11-03` (at 08:00), or that date and a time of day: `2026-11-03 09:30`.
fn dated(text: &str, clock: &Clock) -> Option<i64> {
    let (day, (hour, minute)) = match text.split_once(' ') {
        Some((day, time)) => (day, time_of_day(time.trim_start())?),
        None => (text, (MORNING, 0)),
    };
    local(clock, civil_date(day)?, hour, minute)
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
                ("12:41", NOW + 60),
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
    fn text_that_is_not_a_time_gets_the_examples() {
        let clock = utc(NOW);
        let garbage = [
            "",
            "  ",
            "soon",
            "tomorrow",
            "tu",
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
            "mon 9:00",
        ];
        for text in garbage {
            assert_eq!(parse_snooze(text, &clock), Err(TRY), "{text:?}");
        }
    }
}
