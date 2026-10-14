//! Graph's `DateTimeOffset` values, which it always writes in UTC (`2026-10-12T08:30:15Z`, with
//! up to seven fraction digits), to and from Unix milliseconds. Anything else, a day its month does not have included, is not a date.

/// `text` as Unix milliseconds: `YYYY-MM-DDTHH:MM:SS`, an optional `.` and digits, then `Z`.
pub(crate) fn parse_ms(text: &str) -> Option<i64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut ymd = date.splitn(3, '-');
    let (year, month, day) = (
        digits(ymd.next()?, 4)?,
        digits(ymd.next()?, 2)?,
        digits(ymd.next()?, 2)?,
    );
    let (clock, fraction) = match time.split_once('.') {
        Some((_, "")) => return None,
        Some(split) => split,
        None => (time, ""),
    };
    let mut hms = clock.splitn(3, ':');
    let (hour, minute, second) = (
        digits(hms.next()?, 2)?,
        digits(hms.next()?, 2)?,
        digits(hms.next()?, 2)?,
    );
    let valid = (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour < 24
        && minute < 60
        && second <= 60
        && fraction.bytes().all(|b| b.is_ascii_digit());
    if !valid {
        return None;
    }
    let millis: i64 = format!("{:0<3}", &fraction[..fraction.len().min(3)])
        .parse()
        .ok()?;
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000 + millis)
}

fn digits(text: &str, len: usize) -> Option<i64> {
    (text.len() == len && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The days of `month` (1 to 12) in `year`, leap years by the Gregorian rule.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Unix milliseconds in Graph's UTC form, to the second: `2026-10-12T08:30:15Z`.
pub(crate) fn iso(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_dates_read_as_unix_milliseconds() {
        assert_eq!(parse_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_ms("2026-10-12T08:30:15Z"), Some(1_791_793_815_000));
        assert_eq!(
            parse_ms("2026-10-12T08:30:15.123Z"),
            Some(1_791_793_815_123)
        );
        assert_eq!(
            parse_ms("2026-10-12T08:30:15.1234567Z"),
            Some(1_791_793_815_123)
        );
        assert_eq!(parse_ms("2000-02-29T23:59:59Z"), Some(951_868_799_000));
        assert!(parse_ms("2024-02-29T00:00:00Z").is_some(), "a leap day");
        for bad in [
            "",
            "2026-10-12T08:30:15",
            "2026-10-12 08:30:15Z",
            "2026-13-01T00:00:00Z",
            "2026-10-12T24:00:00Z",
            "2026-10-12T08:30:15+01:00",
            "26-10-12T08:30:15Z",
            "2026-10-12T08:30:15.x1Z",
            "2026-02-31T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2100-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-10-12T08:30:15.Z",
        ] {
            assert_eq!(parse_ms(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn unix_milliseconds_write_as_graph_dates_and_read_back() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_791_793_815_123), "2026-10-12T08:30:15Z");
        assert_eq!(iso(951_868_799_000), "2000-02-29T23:59:59Z");
        for ms in [0, 951_868_799_000, 1_791_793_815_000, 4_102_444_800_000] {
            assert_eq!(parse_ms(&iso(ms)), Some(ms));
        }
    }
}
