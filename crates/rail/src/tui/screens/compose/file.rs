//! The compose file: four headers, a blank line, the body.

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

/// Write `<dir>/<key>.eml` (directory 0700, file 0600) and return its path. The text is
/// written as is: the editor needs the real text.
pub fn write(dir: &Path, key: &str, headers: &Headers, body: &str) -> std::io::Result<PathBuf> {
    create_private_dir(dir)?;
    let path = dir.join(format!("{key}.eml"));
    let text = format!(
        "To: {}\nCc: {}\nBcc: {}\nSubject: {}\n\n{body}",
        headers.to.join(", "),
        headers.cc.join(", "),
        headers.bcc.join(", "),
        headers.subject,
    );
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

/// A comma-separated header value as trimmed, non-empty items.
pub fn items(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
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
/// a space), then the body verbatim. An unknown header or a line without `Name:` is an error
/// naming its line.
pub fn parse(text: &str) -> Result<(Headers, String), ParseError> {
    let (head, body) = split(text);
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
            Some(&"To") => headers.to.extend(items(&value)),
            Some(&"Cc") => headers.cc.extend(items(&value)),
            Some(&"Bcc") => headers.bcc.extend(items(&value)),
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
