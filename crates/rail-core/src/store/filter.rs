//! The filter line's syntax as SQL over the store. Plain words become an FTS5 query (through
//! `fts_query`, so FTS syntax in them is matched as text); `from:`, `to:`, `subject:`,
//! `has:attachment`, `is:unread`, `is:starred`, `before:YYYY-MM-DD`, `after:YYYY-MM-DD` and
//! `account:<name>` become conditions. An unknown operator, an empty value or a date that is
//! not one is a word. Values may be quoted (`from:"Dana Reyes"`). A filter never reaches Gmail.

use std::fmt;

use rusqlite::types::Value;

use super::sync_read::fts_query;

/// Filter text Rail reads; the rest is left out.
pub const MAX_FILTER_CHARS: usize = 1024;
/// Terms Rail reads; the rest are left out.
pub const MAX_TERMS: usize = 32;

/// A calendar day, as `before:` and `after:` take it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Day {
    /// `YYYY-MM-DD`, a real date from 1970 to 9999.
    pub fn parse(text: &str) -> Option<Day> {
        let bytes = text.as_bytes();
        let shape = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
        if !shape {
            return None;
        }
        let year: i32 = text[..4].parse().ok()?;
        let month: u8 = text[5..7].parse().ok()?;
        let day: u8 = text[8..].parse().ok()?;
        let valid = (1970..=9999).contains(&year)
            && (1..=12).contains(&month)
            && (1..=days_in_month(year, month)).contains(&day);
        valid.then_some(Day { year, month, day })
    }

    /// Days from 1970-01-01 (Howard Hinnant's `days_from_civil`).
    fn days(self) -> i64 {
        let (month, day) = (i64::from(self.month), i64::from(self.day));
        let year = i64::from(self.year) - i64::from(month <= 2);
        let era = year.div_euclid(400);
        let yoe = year - era * 400;
        let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The local midnight that starts this day, in Unix milliseconds, for a time zone
    /// `utc_offset` seconds east of UTC.
    pub fn start_ms(self, utc_offset: i32) -> i64 {
        (self.days() * 86_400 - i64::from(utc_offset)) * 1000
    }
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A parsed filter. Values are the user's text: never shown without escaping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub words: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub has_attachment: bool,
    pub unread: bool,
    pub starred: bool,
    pub before: Option<Day>,
    pub after: Option<Day>,
    pub account: Option<String>,
    /// Text or terms past the caps were left out.
    pub truncated: bool,
}

/// `text` as a filter.
pub fn parse(text: &str) -> Filter {
    let mut filter = Filter::default();
    let kept: String = text.chars().take(MAX_FILTER_CHARS).collect();
    filter.truncated = kept.len() < text.len();
    let terms = tokens(&kept);
    filter.truncated |= terms.len() > MAX_TERMS;
    for term in terms.into_iter().take(MAX_TERMS) {
        filter.add(term);
    }
    filter
}

/// Terms split at white space and controls; `"` starts and ends a stretch whose spaces are
/// kept (the quotes themselves are dropped). Controls inside quotes are dropped.
fn tokens(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_control() || (c.is_whitespace() && !quoted) => {
                if !quoted && !current.is_empty() {
                    terms.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms
}

impl Filter {
    fn add(&mut self, term: String) {
        let known = term
            .split_once(':')
            .is_some_and(|(op, value)| self.operator(op, value));
        if !known {
            self.words.push(term);
        }
    }

    /// Take `op:value` as an operator; false when it is not one (so it is a word).
    fn operator(&mut self, op: &str, value: &str) -> bool {
        if value.is_empty() {
            return false;
        }
        let lower = value.to_ascii_lowercase();
        match op.to_ascii_lowercase().as_str() {
            "from" => self.from.push(value.to_string()),
            "to" => self.to.push(value.to_string()),
            "subject" => self.subject.push(value.to_string()),
            "has" if lower == "attachment" => self.has_attachment = true,
            "is" if lower == "unread" => self.unread = true,
            "is" if lower == "starred" => self.starred = true,
            "before" | "after" => {
                let Some(day) = Day::parse(value) else {
                    return false;
                };
                if op.eq_ignore_ascii_case("before") {
                    self.before = Some(day);
                } else {
                    self.after = Some(day);
                }
            }
            "account" => self.account = Some(value.to_string()),
            _ => return false,
        }
        true
    }

    /// True when nothing filters.
    pub fn is_empty(&self) -> bool {
        *self == Filter::default()
    }

    /// The operators as Rail read them, for the status line (so a typo shows as a word).
    pub fn describe(&self) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.from.iter().map(|v| format!("from:{v}")));
        parts.extend(self.to.iter().map(|v| format!("to:{v}")));
        parts.extend(self.subject.iter().map(|v| format!("subject:{v}")));
        for (on, name) in [
            (self.has_attachment, "has:attachment"),
            (self.unread, "is:unread"),
            (self.starred, "is:starred"),
        ] {
            if on {
                parts.push(name.to_string());
            }
        }
        parts.extend(self.before.map(|d| format!("before:{d}")));
        parts.extend(self.after.map(|d| format!("after:{d}")));
        parts.extend(self.account.as_ref().map(|a| format!("account:{a}")));
        if !self.words.is_empty() {
            parts.push(format!("words: {}", self.words.join(" ")));
        }
        if self.truncated {
            parts.push(format!("(only the first {MAX_TERMS} terms are used)"));
        }
        parts
    }

    /// The message-level conditions as one `EXISTS` over the thread `t`'s messages `m`, with
    /// its parameters in order; `None` when there are none.
    pub(crate) fn message_sql(&self, utc_offset: i32) -> Option<(String, Vec<Value>)> {
        let mut conds: Vec<String> = Vec::new();
        let mut params: Vec<Value> = Vec::new();
        if let Some(query) = fts_query(&self.words.join(" ")) {
            conds
                .push("m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)".into());
            params.push(Value::Text(query));
        }
        for value in &self.from {
            conds.push(mailbox_like(&["m.from_json"]));
            params.extend(std::iter::repeat_n(Value::Text(pattern(value)), 2));
        }
        for value in &self.to {
            conds.push(mailbox_like(&["m.to_json", "m.cc_json", "m.bcc_json"]));
            params.extend(std::iter::repeat_n(Value::Text(pattern(value)), 6));
        }
        for value in &self.subject {
            conds.push(like("m.subject"));
            params.push(Value::Text(pattern(value)));
        }
        self.flag_conds(&mut conds);
        for (day, op) in [(self.before, "<"), (self.after, ">=")] {
            if let Some(day) = day {
                conds.push(format!("m.internal_date {op} ?"));
                params.push(Value::Integer(day.start_ms(utc_offset)));
            }
        }
        (!conds.is_empty()).then(|| {
            let all = conds.join(" AND ");
            (
                format!("EXISTS (SELECT 1 FROM messages m WHERE m.thread_id = t.id AND {all})"),
                params,
            )
        })
    }

    fn flag_conds(&self, conds: &mut Vec<String>) {
        if self.has_attachment {
            conds.push("EXISTS (SELECT 1 FROM attachments a WHERE a.message_id = m.id)".into());
        }
        if self.unread {
            conds.push("m.is_read = 0".into());
        }
        if self.starred {
            conds.push("m.is_starred = 1".into());
        }
    }
}

/// `column LIKE ? ESCAPE '\'`.
fn like(column: &str) -> String {
    format!("{column} LIKE ? ESCAPE '\\'")
}

/// A condition that some mailbox in one of the JSON `columns` has a name or an address
/// matching a `LIKE` pattern (two `?` per column: name, then address). The JSON's keys and
/// punctuation never match; a column that is not valid JSON reads as empty.
fn mailbox_like(columns: &[&str]) -> String {
    let any = columns
        .iter()
        .map(|c| {
            format!(
                "EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid({c}) THEN {c} ELSE '[]' END) j \
                 WHERE json_extract(j.value, '$.name') LIKE ? ESCAPE '\\' \
                 OR json_extract(j.value, '$.address') LIKE ? ESCAPE '\\')"
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("({any})")
}

/// `value` as a `LIKE` pattern matching it anywhere, its own `%`, `_` and `\` escaped.
fn pattern(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators_become_conditions_and_the_rest_words() {
        let f = parse(
            "budget from:dana TO:corp@example.com subject:\"Q4 plan\" has:attachment \
                       is:unread is:STARRED before:2026-10-01 after:2026-09-01 account:work q4",
        );
        assert_eq!(f.words, ["budget", "q4"]);
        assert_eq!(f.from, ["dana"]);
        assert_eq!(f.to, ["corp@example.com"]);
        assert_eq!(f.subject, ["Q4 plan"]);
        assert!(f.has_attachment && f.unread && f.starred);
        assert_eq!(
            f.before,
            Some(Day {
                year: 2026,
                month: 10,
                day: 1
            })
        );
        assert_eq!(
            f.after,
            Some(Day {
                year: 2026,
                month: 9,
                day: 1
            })
        );
        assert_eq!(f.account.as_deref(), Some("work"));
        assert_eq!(
            f.describe(),
            [
                "from:dana",
                "to:corp@example.com",
                "subject:Q4 plan",
                "has:attachment",
                "is:unread",
                "is:starred",
                "before:2026-10-01",
                "after:2026-09-01",
                "account:work",
                "words: budget q4"
            ]
        );
    }

    #[test]
    fn unknown_operators_bad_dates_and_empty_values_are_words() {
        let f = parse(
            "form:dana label:work is:important has:pdf before:2026-13-01 after:2026-02-30 \
                       before:yesterday from: :x",
        );
        assert_eq!(
            f.words,
            [
                "form:dana",
                "label:work",
                "is:important",
                "has:pdf",
                "before:2026-13-01",
                "after:2026-02-30",
                "before:yesterday",
                "from:",
                ":x"
            ]
        );
        assert!(f.from.is_empty() && f.before.is_none() && f.after.is_none());
        assert!(!f.is_empty());
        assert!(parse("   ").is_empty());
    }

    #[test]
    fn input_and_terms_are_capped() {
        let many = (0..40)
            .map(|n| format!("w{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let f = parse(&many);
        assert_eq!(f.words.len(), MAX_TERMS);
        assert!(f.truncated);
        assert_eq!(
            f.describe().last().unwrap(),
            "(only the first 32 terms are used)"
        );
        let long = parse(&"x".repeat(MAX_FILTER_CHARS * 10));
        assert_eq!(long.words[0].chars().count(), MAX_FILTER_CHARS);
        assert!(long.truncated);
        let nul = parse("a\0b \u{1b}[2J");
        assert_eq!(nul.words, ["a", "b", "[2J"]);
    }

    #[test]
    fn days_parse_strictly_and_count_from_the_epoch() {
        assert_eq!(Day::parse("1970-01-01").unwrap().start_ms(0), 0);
        assert_eq!(
            Day::parse("2023-11-14").unwrap().start_ms(0),
            19_675 * 86_400_000
        );
        assert_eq!(Day::parse("2024-02-29").unwrap().to_string(), "2024-02-29");
        assert_eq!(
            Day::parse("2026-10-10").unwrap().start_ms(3_600),
            (Day::parse("2026-10-10").unwrap().start_ms(0)) - 3_600_000,
            "local midnight in UTC+1 is an hour earlier"
        );
        for bad in [
            "2023-02-29",
            "2026-00-10",
            "2026-1-10",
            "26-10-10",
            "2026/10/10",
            "1969-12-31",
            "2026-10-10x",
            "",
        ] {
            assert_eq!(Day::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn like_patterns_escape_their_wildcards() {
        assert_eq!(pattern("50%_off\\"), "%50\\%\\_off\\\\%");
    }
}
