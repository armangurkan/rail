//! The vendored colour schemes: the iTerm2-Color-Schemes collection (MIT) as one
//! tab-separated table, parsed once. `scripts/update-schemes.sh` regenerates the table.

use std::sync::OnceLock;

use ratatui::style::Color;

/// The table `scripts/update-schemes.sh` writes.
const TABLE: &str = include_str!("schemes.txt");
/// How many schemes the table holds; the test pins it so a bad regeneration fails CI.
#[cfg(test)]
pub const COUNT: usize = 769;
/// Columns per line: name, background, foreground, selection bg, selection fg, 16 palette.
const COLUMNS: usize = 21;
/// The farthest a misspelling may be from a name and still be suggested.
const NEAREST_DISTANCE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `#rrggbb`, nothing else.
    pub fn parse(hex: &str) -> Option<Rgb> {
        let digits = hex.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    pub fn color(self) -> Color {
        Color::Rgb(self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheme {
    pub name: &'static str,
    pub background: Rgb,
    pub foreground: Rgb,
    pub selection_bg: Rgb,
    pub selection_fg: Rgb,
    pub palette: [Rgb; 16],
}

/// Every scheme, in the table's (case-insensitive) order. A table that does not parse is a
/// build defect the tests catch; at runtime it logs and yields no schemes.
pub fn schemes() -> &'static [Scheme] {
    static ALL: OnceLock<Vec<Scheme>> = OnceLock::new();
    ALL.get_or_init(|| match parse_table(TABLE) {
        Ok(all) => all,
        Err(error) => {
            tracing::error!(error, "the vendored scheme table does not parse");
            Vec::new()
        }
    })
}

/// The scheme called `name`, ignoring case.
pub fn scheme(name: &str) -> Option<&'static Scheme> {
    schemes().iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// The closest name within a few edits of `name`, for "did you mean".
pub fn nearest(name: &str) -> Option<&'static str> {
    let wanted = name.to_ascii_lowercase();
    schemes()
        .iter()
        .map(|s| (distance(&wanted, &s.name.to_ascii_lowercase()), s.name))
        .filter(|(d, _)| *d <= NEAREST_DISTANCE)
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

/// Parse the table text. Lines starting with `#` are comments. Errors name the line.
pub(crate) fn parse_table(text: &'static str) -> Result<Vec<Scheme>, String> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        let fields: Vec<&'static str> = line.split('\t').collect();
        if fields.len() != COLUMNS {
            return Err(format!(
                "line {number}: {} columns, not {COLUMNS}",
                fields.len()
            ));
        }
        let rgb = |i: usize| {
            Rgb::parse(fields[i])
                .ok_or_else(|| format!("line {number}: column {} is not #rrggbb", i + 1))
        };
        let mut palette = [Rgb(0, 0, 0); 16];
        for (slot, entry) in palette.iter_mut().enumerate() {
            *entry = rgb(5 + slot)?;
        }
        out.push(Scheme {
            name: fields[0],
            background: rgb(1)?,
            foreground: rgb(2)?,
            selection_bg: rgb(3)?,
            selection_fg: rgb(4)?,
            palette,
        });
    }
    Ok(out)
}

/// Levenshtein distance over chars.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            let value = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
            current.push(value);
        }
        previous = current;
    }
    previous[b.len()]
}
