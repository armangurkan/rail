//! The time dates are shown against, in the local time zone. Tests fix both the time and the
//! zone, so nothing on screen depends on when or where a test runs.

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// Now and the time zone.
#[derive(Debug, Clone)]
pub struct Clock {
    /// Fixed "now" (Unix seconds) for tests; the system clock when `None`.
    now: Option<i64>,
    tz: TimeZone,
}

impl Clock {
    /// The system clock in the system's time zone.
    pub fn system() -> Self {
        Self {
            now: None,
            tz: TimeZone::system(),
        }
    }

    #[cfg(test)]
    pub fn fixed(now: i64, tz: TimeZone) -> Self {
        Self { now: Some(now), tz }
    }

    /// Unix seconds.
    pub fn now(&self) -> i64 {
        self.now.unwrap_or_else(rail_core::auth::unix_now)
    }

    /// The time zone's offset now, in seconds east of UTC (for the filter's dates).
    pub fn utc_offset(&self) -> i32 {
        self.zoned(self.now()).map_or(0, |z| z.offset().seconds())
    }

    fn zoned(&self, secs: i64) -> Option<Zoned> {
        Timestamp::from_second(secs)
            .ok()
            .map(|t| t.to_zoned(self.tz.clone()))
    }

    /// The list's date: `10:42` today, `Yesterday`, a weekday within six days, `Oct 03`
    /// this year, `2024-10-03` before; `?` for a time out of range.
    pub fn short(&self, secs: i64) -> String {
        let (Some(when), Some(now)) = (self.zoned(secs), self.zoned(self.now())) else {
            return "?".to_string();
        };
        let (day, today) = (when.date(), now.date());
        if day == today {
            return when.strftime("%H:%M").to_string();
        }
        if day.tomorrow().ok() == Some(today) {
            return "Yesterday".to_string();
        }
        let ago = today.since(day).ok().map(|span| span.get_days());
        if ago.is_some_and(|days| (2..=6).contains(&days)) {
            return when.strftime("%a").to_string();
        }
        if day.year() == today.year() {
            return when.strftime("%b %d").to_string();
        }
        when.strftime("%Y-%m-%d").to_string()
    }

    /// The reader's date: `Fri 10 Oct 2025 12:40`.
    pub fn long(&self, secs: i64) -> String {
        self.zoned(secs).map_or_else(
            || "?".to_string(),
            |when| when.strftime("%a %d %b %Y %H:%M").to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_760_100_000; // Friday 2025-10-10 12:40 UTC

    #[test]
    fn short_dates_read_like_a_mail_list() {
        let clock = Clock::fixed(NOW, TimeZone::UTC);
        assert_eq!(clock.short(NOW - 3_600), "11:40");
        assert_eq!(clock.short(NOW - 86_400), "Yesterday");
        assert_eq!(clock.short(NOW - 3 * 86_400), "Tue");
        assert_eq!(clock.short(NOW - 30 * 86_400), "Sep 10");
        assert_eq!(clock.short(NOW - 400 * 86_400), "2024-09-05");
        assert_eq!(
            clock.short(NOW + 86_400),
            "Oct 11",
            "a future date reads as a date"
        );
        assert_eq!(clock.long(NOW), "Fri 10 Oct 2025 12:40");
        assert_eq!(clock.utc_offset(), 0);
    }

    #[test]
    fn the_time_zone_moves_the_day() {
        let tokyo = Clock::fixed(NOW, TimeZone::fixed(jiff::tz::offset(9)));
        assert_eq!(tokyo.short(NOW - 3_600), "20:40");
        assert_eq!(tokyo.utc_offset(), 9 * 3_600);
        assert_eq!(Clock::fixed(NOW, TimeZone::UTC).short(i64::MAX), "?");
    }
}
