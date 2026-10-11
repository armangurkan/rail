//! The one path outside text takes to the screen: escaping, width, cutting and padding to
//! columns, wrapping at a pane's width, tabs, quoted blocks, link markers, link validation and
//! OSC 8 hyperlinks. Every string from mail, Gmail or config that Rail draws goes through
//! here: controls, Unicode format characters and line separators are escaped first, and width
//! is measured on the escaped text, so no cell can ever carry a raw control.

#![allow(
    dead_code,
    reason = "the list uses escape, fit, pad and outside; wrapping, blocks, links and hyperlinks are for the reader and `rail` (Tasks 8 to 11)"
)]

use rail_core::provider::links::{MAX_LINK_BYTES, SCHEMES};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use url::Url;

use crate::cmd::account::escape_chars;
use crate::cmd::text::hides_or_breaks;

/// Columns between tab stops in a body.
pub const TAB_STOP: usize = 8;
/// Quoted lines in a row that fold to one `[n quoted lines]` line.
pub const QUOTE_FOLD_MIN: usize = 4;
const ELLIPSIS: &str = "…";

/// `text` with every control, Unicode format character and line separator written as its
/// escape (`\u{1b}`, `\u{202e}`, `\u{2028}`, `\0`), the rest unchanged.
pub fn escape(text: &str) -> String {
    escape_chars(text, hides_or_breaks)
}

/// Columns `text` takes on screen, as ratatui draws it: the sum of each grapheme's width (not
/// the sequence width of the whole string, which is narrower for joined scripts such as
/// Arabic). Only for escaped text.
pub fn width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// `text` escaped and cut to at most `cols` columns, ending in `…` when it was cut.
pub fn fit(text: &str, cols: usize) -> String {
    let escaped = escape(text);
    if width(&escaped) <= cols {
        return escaped;
    }
    let Some(room) = cols.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0;
    for grapheme in escaped.graphemes(true) {
        let w = width(grapheme);
        if used + w > room {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push_str(ELLIPSIS);
    out
}

/// `fit`, padded with spaces on the right to exactly `cols` columns.
pub fn pad(text: &str, cols: usize) -> String {
    let mut out = fit(text, cols);
    let short = cols.saturating_sub(width(&out));
    out.extend(std::iter::repeat_n(' ', short));
    out
}

/// `fit`, padded with spaces on the left to exactly `cols` columns.
pub fn pad_left(text: &str, cols: usize) -> String {
    let fitted = fit(text, cols);
    let short = cols.saturating_sub(width(&fitted));
    let mut out: String = std::iter::repeat_n(' ', short).collect();
    out.push_str(&fitted);
    out
}

/// Text from outside Rail (the provider's or the system's words), escaped and labelled.
pub fn outside(text: &str) -> String {
    format!(
        "from outside Rail (the provider or the system): \"{}\"",
        escape(text)
    )
}

/// `line` with each tab replaced by spaces up to the next `TAB_STOP` column.
pub fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + TAB_STOP);
    let mut col = 0;
    for grapheme in line.graphemes(true) {
        if grapheme == "\t" {
            let spaces = TAB_STOP - col % TAB_STOP;
            out.extend(std::iter::repeat_n(' ', spaces));
            col += spaces;
        } else {
            out.push_str(grapheme);
            col += width(grapheme);
        }
    }
    out
}

/// One escaped line as rows of at most `cols` columns (at least 1): broken at spaces, words
/// longer than a row split between graphemes. Leading spaces stay on the first row (cut to
/// leave room for a character), spaces between words stay within a row and are dropped at a
/// break, so no row is empty or only spaces unless the line is. A grapheme wider than `cols`
/// gets a row of its own.
pub fn wrap(row: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let rest = row.trim_start_matches(' ');
    let indent = (row.len() - rest.len()).min(cols - 1);
    let mut rows = Vec::new();
    let mut current = " ".repeat(indent);
    let mut used = indent;
    if rest.is_empty() {
        return vec![current];
    }
    let (mut has_word, mut gap) = (false, 0);
    for piece in rest.split(' ') {
        if piece.is_empty() {
            gap += 1;
            continue;
        }
        let sep = if has_word { gap + 1 } else { 0 };
        gap = 0;
        let w = width(piece);
        if used + sep + w <= cols {
            current.extend(std::iter::repeat_n(' ', sep));
            current.push_str(piece);
            used += sep + w;
        } else {
            if has_word {
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
            // Here `current` is empty or only the indentation.
            let mut content = false;
            for grapheme in piece.graphemes(true) {
                let gw = width(grapheme);
                if used + gw > cols && used > 0 {
                    // Indentation alone is dropped, never emitted as a row of spaces.
                    let flushed = std::mem::take(&mut current);
                    if content {
                        rows.push(flushed);
                    }
                    used = 0;
                }
                current.push_str(grapheme);
                used += gw;
                content = true;
            }
        }
        has_word = true;
    }
    current.extend(std::iter::repeat_n(' ', gap.min(cols.saturating_sub(used))));
    rows.push(current);
    rows
}

/// A line of a body, or a run of quoted lines long enough to fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block<'a> {
    Line(&'a str),
    Quote(Vec<&'a str>),
}

/// `body`'s lines, with every run of at least `QUOTE_FOLD_MIN` quoted lines (a line whose
/// first character after spaces and tabs is `>`) as one block.
pub fn blocks(body: &str) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    for line in body.lines() {
        if line.trim_start_matches([' ', '\t']).starts_with('>') {
            run.push(line);
            continue;
        }
        end_run(&mut out, &mut run);
        out.push(Block::Line(line));
    }
    end_run(&mut out, &mut run);
    out
}

fn end_run<'a>(out: &mut Vec<Block<'a>>, run: &mut Vec<&'a str>) {
    if run.len() >= QUOTE_FOLD_MIN {
        out.push(Block::Quote(std::mem::take(run)));
    } else {
        out.extend(run.drain(..).map(Block::Line));
    }
}

/// What a folded run shows.
pub fn fold_label(lines: usize) -> String {
    if lines == 1 {
        "[1 quoted line]".to_string()
    } else {
        format!("[{lines} quoted lines]")
    }
}

/// A link marker in a row: where it starts (in columns), how wide it is, and its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    pub col: usize,
    pub width: usize,
    pub number: usize,
}

/// The `[n]` markers of an escaped row, for `1 <= n <= links` without leading zeros.
pub fn markers(row: &str, links: usize) -> Vec<Marker> {
    let bytes = row.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = bytes[at..].iter().position(|&b| b == b'[').map(|p| at + p) {
        let digits = bytes[open + 1..]
            .iter()
            .take(4)
            .take_while(|b| b.is_ascii_digit())
            .count();
        let close = open + 1 + digits;
        let number = (digits > 0 && bytes[open + 1] != b'0' && bytes.get(close) == Some(&b']'))
            .then(|| row[open + 1..close].parse::<usize>().ok())
            .flatten()
            .filter(|n| (1..=links).contains(n));
        if let Some(number) = number {
            found.push(Marker {
                col: width(&row[..open]),
                width: close + 1 - open,
                number,
            });
        }
        at = open + 1;
    }
    found
}

/// `address` as a URL Rail may show as a hyperlink or open: at most `MAX_LINK_BYTES`, no space
/// or control byte (ASCII or Unicode), no format character or line separator, a scheme in `SCHEMES`, and a serialisation of printable ASCII only (so it
/// can stand inside an OSC 8 sequence).
pub fn safe_link(address: &str) -> Option<Url> {
    let clean =
        address.bytes().all(|b| b > b' ' && b != 0x7f) && !address.chars().any(hides_or_breaks);
    if address.len() > MAX_LINK_BYTES || !clean {
        return None;
    }
    let url = Url::parse(address).ok()?;
    let scheme = SCHEMES
        .iter()
        .any(|s| s.trim_end_matches(':') == url.scheme());
    let printable = url.as_str().bytes().all(|b| (0x21..0x7f).contains(&b));
    (scheme && printable).then_some(url)
}

/// `text` as an OSC 8 hyperlink to `url`. `url` must come from `safe_link` and `text` from
/// `escape`.
pub fn osc8(url: &Url, text: &str) -> String {
    format!("\u{1b}]8;;{}\u{1b}\\{text}\u{1b}]8;;\u{1b}\\", url.as_str())
}

/// Whether this terminal shows OSC 8 hyperlinks, judged from its environment (`var` reads a
/// variable; empty counts as unset). Off inside tmux and screen, where the sequence may not
/// pass through, and on terminals not known to support it.
pub fn hyperlinks_supported(var: impl Fn(&str) -> Option<String>) -> bool {
    let get = |name: &str| var(name).filter(|v| !v.is_empty());
    let term = get("TERM").unwrap_or_default();
    let multiplexed = get("TMUX").is_some()
        || get("STY").is_some()
        || term.starts_with("screen")
        || term.starts_with("tmux");
    if multiplexed || term == "dumb" || term == "linux" {
        return false;
    }
    let program = get("TERM_PROGRAM").unwrap_or_default();
    if matches!(
        program.as_str(),
        "iTerm.app" | "WezTerm" | "vscode" | "ghostty" | "Hyper"
    ) {
        return true;
    }
    if ["KITTY_WINDOW_ID", "WEZTERM_PANE", "WT_SESSION"]
        .iter()
        .any(|name| get(name).is_some())
    {
        return true;
    }
    if get("VTE_VERSION")
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|v| v >= 5000)
    {
        return true;
    }
    matches!(
        term.as_str(),
        "xterm-kitty" | "alacritty" | "foot" | "foot-extra" | "xterm-ghostty" | "wezterm"
    )
}
