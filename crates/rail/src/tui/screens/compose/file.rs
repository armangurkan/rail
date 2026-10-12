//! The compose file: four headers, a blank line, the body.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

use rail_core::paths::create_private_dir;

use super::model::Headers;

const HEADERS: [&str; 4] = ["To", "Cc", "Bcc", "Subject"];

/// Why a compose file could not be read back, and on which line (from 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

/// `text` with every run of carriage returns before a line feed removed, so `\r\n` (what an
/// editor set to Windows line ends writes) and the extra `\r` each round used to add become
/// `\n`. A `\r` not followed by `\n` stays. Borrows when there is nothing to remove.
pub fn plain_line_ends(text: &str) -> Cow<'_, str> {
    if !text.contains("\r\n") && !text.contains("\r\r") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut pending = 0;
    for c in text.chars() {
        match c {
            '\r' => pending += 1,
            '\n' => {
                pending = 0;
                out.push('\n');
            }
            _ => {
                out.extend(std::iter::repeat_n('\r', pending));
                pending = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat_n('\r', pending));
    Cow::Owned(out)
}

/// The text of the compose file for `headers` and `body`: four header lines (each value kept
/// to one line), an empty line, the body with plain line ends.
pub fn render(headers: &Headers, body: &str) -> String {
    let list = |items: &[String]| {
        items
            .iter()
            .map(|i| one_line(i))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "To: {}\nCc: {}\nBcc: {}\nSubject: {}\n\n{}",
        list(&headers.to),
        list(&headers.cc),
        list(&headers.bcc),
        one_line(&headers.subject),
        plain_line_ends(body),
    )
}

/// Where the compose file of `key` lives in `dir`.
pub fn path_of(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.eml"))
}

/// Write `<dir>/<key>.eml` (directory 0700, file 0600) and return its path. The body is
/// written with plain line ends (the editor needs the real text); each header value is kept
/// to one line.
pub fn write(dir: &Path, key: &str, headers: &Headers, body: &str) -> std::io::Result<PathBuf> {
    create_private_dir(dir)?;
    let path = path_of(dir, key);
    let text = render(headers, body);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}

/// `text` with `\r`, `\n` and every other control character replaced by a space, so a header
/// value stays on its one line of the compose file.
pub fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// A comma-separated header value as trimmed, non-empty items, without the quote-aware
/// rules: the fallback for unbalanced quotes.
fn plain_split(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Byte offsets of the commas of `value` that separate recipients (not those inside `"…"`,
/// where `\"` does not close the quote), and whether the quotes balance.
pub(super) fn separators(value: &str) -> (Vec<usize>, bool) {
    let (mut found, mut quoted, mut escaped) = (Vec::new(), false, false);
    for (at, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => found.push(at),
            _ => {}
        }
    }
    (found, !quoted)
}

/// A header value's recipients: split at commas outside quotes (`"Reyes, Dana" <d@x>` is one),
/// trimmed, empty ones dropped; with unbalanced quotes, split at every comma.
pub fn split_addresses(value: &str) -> Vec<String> {
    let (commas, balanced) = separators(value);
    if !balanced {
        return plain_split(value);
    }
    let mut out = Vec::new();
    let mut start = 0;
    for at in commas.into_iter().chain(std::iter::once(value.len())) {
        let item = value[start..at].trim();
        if !item.is_empty() {
            out.push(item.to_string());
        }
        start = at + 1;
    }
    out
}

/// The header block (up to the first empty line) and the body after it. Without an empty
/// line the whole text is headers and the body is empty.
fn split(text: &str) -> (&str, &str) {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let end = at + line.len();
        if line.trim_end_matches(['\n', '\r']).is_empty() {
            return (&text[..at], &text[end..]);
        }
        at = end;
    }
    (text, "")
}

/// Headers up to the first empty line (names case-insensitive, continuation lines joined with
/// a space), then the body with plain line ends. An unknown header or a line without `Name:` is an error
/// naming its line.
pub fn parse(text: &str) -> Result<(Headers, String), ParseError> {
    let text = plain_line_ends(text);
    let (head, body) = split(&text);
    let mut fields: Vec<(usize, String, String)> = Vec::new();
    for (index, line) in head.lines().enumerate() {
        let number = index + 1;
        if line.starts_with([' ', '\t']) {
            let Some((_, _, value)) = fields.last_mut() else {
                return Err(ParseError {
                    line: number,
                    message: "a continuation line with no header before it".into(),
                });
            };
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ParseError {
                line: number,
                message: "expected \"Name: value\"".into(),
            });
        };
        fields.push((number, name.trim().to_string(), value.trim().to_string()));
    }
    let mut headers = Headers::default();
    for (line, name, value) in fields {
        match HEADERS.iter().find(|h| h.eq_ignore_ascii_case(&name)) {
            Some(&"To") => headers.to.extend(split_addresses(&value)),
            Some(&"Cc") => headers.cc.extend(split_addresses(&value)),
            Some(&"Bcc") => headers.bcc.extend(split_addresses(&value)),
            Some(_) => headers.subject = value,
            None => {
                return Err(ParseError {
                    line,
                    message: format!("unknown header {name:?}"),
                });
            }
        }
    }
    Ok((headers, body.to_string()))
}
