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

    /// The time zone dates are shown in.
    pub fn zone(&self) -> &TimeZone {
        &self.tz
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
        let Some((when, now, days)) = self.days_back(secs) else {
            return "?".to_string();
        };
        match days {
            0 => when.strftime("%H:%M").to_string(),
            1 => "Yesterday".to_string(),
            2..=6 => when.strftime("%a").to_string(),
            _ => date(&when, &now),
        }
    }

    /// The thread list's group for `secs`: by calendar day in the clock's zone.
    pub fn group(&self, secs: i64) -> Group {
        match self.days_back(secs).map(|(_, _, days)| days) {
            // A later day sorts first in the newest-first list: it joins the newest group.
            Some(..=0) => Group::Today,
            Some(1) => Group::Yesterday,
            Some(2..=6) => Group::ThisWeek,
            _ => Group::Older,
        }
    }

    /// The time a list row shows at its right edge, by its group: `14:05` today and
    /// yesterday, `Mon 18:20` this week, `Oct 03` this year or `2024-10-03` before, and a later
    /// day's date (in `TODAY`); `?` for a time out of range.
    pub fn time_in_group(&self, secs: i64, group: Group) -> String {
        let Some((when, now, days)) = self.days_back(secs) else {
            return "?".to_string();
        };
        if days < 0 {
            return date(&when, &now);
        }
        match group {
            Group::Today | Group::Yesterday => when.strftime("%H:%M").to_string(),
            Group::ThisWeek => when.strftime("%a %H:%M").to_string(),
            Group::Older => date(&when, &now),
        }
    }

    /// `secs` and now in the clock's zone, and the calendar days from that day back to today
    /// (0 today, 1 yesterday, negative for a later day); `None` for a time out of range.
    fn days_back(&self, secs: i64) -> Option<(Zoned, Zoned, i32)> {
        let (when, now) = (self.zoned(secs)?, self.zoned(self.now())?);
        let days = now.date().since(when.date()).ok()?.get_days();
        Some((when, now, days))
    }

    /// The reader's date: `Fri 10 Oct 2025 12:40`.
    pub fn long(&self, secs: i64) -> String {
        self.zoned(secs).map_or_else(
            || "?".to_string(),
            |when| when.strftime("%a %d %b %Y %H:%M").to_string(),
        )
    }
}

/// `Oct 03` when `when` is in `now`'s year, `2024-10-03` before or after.
fn date(when: &Zoned, now: &Zoned) -> String {
    if when.date().year() == now.date().year() {
        when.strftime("%b %d").to_string()
    } else {
        when.strftime("%Y-%m-%d").to_string()
    }
}

/// The thread list's date groups, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// The same calendar day as now, or a later day.
    Today,
    /// The day before.
    Yesterday,
    /// Two to six days ago.
    ThisWeek,
    /// A week ago or more, or out of range.
    Older,
}

impl Group {
    /// The group row's text.
    pub fn name(self) -> &'static str {
        match self {
            Self::Today => "TODAY",
            Self::Yesterday => "YESTERDAY",
            Self::ThisWeek => "THIS WEEK",
            Self::Older => "OLDER",
        }
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

    /// Review Focus 3.
    #[test]
    fn groups_fall_on_the_zones_calendar_days() {
        // NOW is 2025-10-10 05:40 at UTC-7, so local midnight is 07:00 UTC.
        let clock = Clock::fixed(NOW, TimeZone::fixed(jiff::tz::offset(-7)));
        let midnight = NOW - 20_400;
        let day = 86_400;
        let cases = [
            (midnight + 60, Group::Today, "00:01"),
            (NOW + 3_600, Group::Today, "06:40"),
            (midnight - 60, Group::Yesterday, "23:59"),
            (midnight - day + 60, Group::Yesterday, "00:01"),
            (midnight - day - 60, Group::ThisWeek, "Wed 23:59"),
            (midnight - 6 * day + 60, Group::ThisWeek, "Sat 00:01"),
            (midnight - 6 * day - 60, Group::Older, "Oct 03"),
            (NOW + day, Group::Today, "Oct 11"),
            (NOW + 90 * day, Group::Today, "2026-01-08"),
            (NOW - 400 * day, Group::Older, "2024-09-05"),
        ];
        for (secs, group, time) in cases {
            assert_eq!(clock.group(secs), group, "{secs}");
            assert_eq!(clock.time_in_group(secs, group), time, "{secs}");
        }
        assert_eq!(
            Clock::fixed(NOW, TimeZone::UTC).group(midnight - 60),
            Group::Today,
            "06:59 UTC is today in UTC: the zone moves the day"
        );
        assert_eq!(clock.group(i64::MAX), Group::Older);
        assert_eq!(clock.time_in_group(i64::MAX, Group::Older), "?");
        assert_eq!(
            clock.short(midnight - 60),
            "Yesterday",
            "short is unchanged"
        );
        assert_eq!(clock.short(midnight - 6 * day + 60), "Sat");
        assert_eq!(clock.short(midnight - 6 * day - 60), "Oct 03");
    }

    #[test]
    fn group_names_are_the_list_headings() {
        let names: Vec<&str> = [
            Group::Today,
            Group::Yesterday,
            Group::ThisWeek,
            Group::Older,
        ]
        .into_iter()
        .map(Group::name)
        .collect();
        assert_eq!(names, ["TODAY", "YESTERDAY", "THIS WEEK", "OLDER"]);
    }
}
