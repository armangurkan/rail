//! Graph's `DateTimeOffset` values, which it always writes in UTC (`2026-10-12T08:30:15Z`, with
//! up to seven fraction digits), to and from Unix milliseconds. Anything else is not a date.

/// `text` as Unix milliseconds: `YYYY-MM-DDTHH:MM:SS`, an optional `.` and digits, then `Z`.
pub(crate) fn parse_ms(text: &str) -> Option<i64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut ymd = date.splitn(3, '-');
    let (year, month, day) = (
        digits(ymd.next()?, 4)?,
        digits(ymd.next()?, 2)?,
        digits(ymd.next()?, 2)?,
    );
    let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut hms = clock.splitn(3, ':');
    let (hour, minute, second) = (
        digits(hms.next()?, 2)?,
        digits(hms.next()?, 2)?,
        digits(hms.next()?, 2)?,
    );
    let valid = (1..=12).contains(&month)
        && (1..=31).contains(&day)
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
        for bad in [
            "",
            "2026-10-12T08:30:15",
            "2026-10-12 08:30:15Z",
            "2026-13-01T00:00:00Z",
            "2026-10-12T24:00:00Z",
            "2026-10-12T08:30:15+01:00",
            "26-10-12T08:30:15Z",
            "2026-10-12T08:30:15.x1Z",
        ] {
            assert_eq!(parse_ms(bad), None, "{bad:?}");
        }
    }
}
