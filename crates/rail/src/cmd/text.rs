//! Outside text on one terminal line, and UTC dates without a date library.

use rail_core::ops::AccountSyncStatus;

use crate::cmd::account::escape_chars;

/// True for a character a mail field must not reach the terminal as: a control character, a
/// Unicode format character (bidi overrides, zero-width characters, the byte-order mark) or
/// the line and paragraph separators U+2028 and U+2029.
fn hides_or_breaks(c: char) -> bool {
    c.is_control() || rail_core::config::is_format_char(c) || matches!(c, '\u{2028}' | '\u{2029}')
}

/// `text` with control and invisible formatting characters escaped (`\n`, `\u{1b}`,
/// `\u{202e}`, `\u{2028}`) and cut to `max_chars` characters with an ellipsis. For mail
/// fields: never quoted, so a column of subjects stays readable.
pub fn escape_field(text: &str, max_chars: usize) -> String {
    let escaped = escape_chars(text, hides_or_breaks);
    if escaped.chars().count() <= max_chars {
        return escaped;
    }
    let mut cut: String = escaped.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `YYYY-MM-DD` in UTC.
pub fn utc_date(secs: i64) -> String {
    let (year, month, day) = civil(secs.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DD HH:MM UTC`.
pub fn utc_minute(secs: i64) -> String {
    let in_day = secs.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02} UTC",
        utc_date(secs),
        in_day / 3_600,
        in_day % 3_600 / 60
    )
}

/// `n` and the noun, singular for one: "1 message", "2 messages".
fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What the store holds for an account: "120 messages, 80 threads, 1 body".
pub fn stored(status: &AccountSyncStatus) -> String {
    format!(
        "{}, {}, {}",
        count(status.messages, "message", "messages"),
        count(status.threads, "thread", "threads"),
        count(status.bodies, "body", "bodies")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_escaped_and_cut() {
        assert_eq!(escape_field("Quarterly report", 40), "Quarterly report");
        assert_eq!(escape_field("a\u{1b}[2Jb\nc", 40), "a\\u{1b}[2Jb\\nc");
        assert_eq!(
            escape_field("pay\u{202E}fdp.exe", 40),
            "pay\\u{202e}fdp.exe"
        );
        assert_eq!(escape_field("abcdefghij", 5), "abcd…");
        assert_eq!(escape_field("", 5), "");
    }

    /// Format characters and the Unicode line and paragraph separators
    /// are escaped like controls, so a subject can neither reverse, hide nor break the line.
    #[test]
    fn invisible_and_line_breaking_characters_are_escaped() {
        let text = escape_field(
            "a\u{2028}b\u{2029}c\u{200B}d\u{FEFF}e\u{2066}f\u{200F}g",
            200,
        );
        assert_eq!(
            text,
            "a\\u{2028}b\\u{2029}c\\u{200b}d\\u{feff}e\\u{2066}f\\u{200f}g"
        );
        assert_eq!(escape_field("Grüße, 日本 😀", 40), "Grüße, 日本 😀");
    }

    #[test]
    fn a_long_subject_is_cut_after_escaping() {
        let long = "x".repeat(10_000);
        let cut = escape_field(&long, 120);
        assert_eq!(cut.chars().count(), 120);
        assert!(cut.ends_with('…'));
        assert_eq!(escape_field("\u{1b}\u{1b}\u{1b}", 8), "\\u{1b}\\…");
    }

    #[test]
    fn dates_are_utc_civil_dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_700_000_000), "2023-11-14");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(-86_400), "1969-12-31");
        assert_eq!(utc_minute(1_700_000_000), "2023-11-14 22:13 UTC");
    }
}
