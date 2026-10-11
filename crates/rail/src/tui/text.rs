//! The one path outside text takes to the screen: escaping, width, cutting and padding to
//! columns, wrapping at a pane's width, tabs, quoted blocks, link markers, link validation and
//! OSC 8 hyperlinks. Every string from mail, Gmail or config that Rail draws goes through
//! here: controls, Unicode format characters and line separators are escaped first, and width
//! is measured on the escaped text, so no cell can ever carry a raw control.

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

/// `address` as a URL Rail may show as a hyperlink or open: at most `MAX_LINK_BYTES` both as
/// written and once normalised (percent-encoding and punycode can make it longer), no space or
/// control byte (ASCII or Unicode), no format character or line separator, a scheme in
/// `SCHEMES`, and a serialisation of printable ASCII only (so it can stand inside an OSC 8
/// sequence).
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
    let serialised = url.as_str();
    let printable = serialised.bytes().all(|b| (0x21..0x7f).contains(&b));
    let fits = serialised.len() <= MAX_LINK_BYTES;
    (scheme && printable && fits).then_some(url)
}

/// The longest address the `g x` confirmation shows, in columns: about three rows of the box.
const SHOWN_ADDRESS_COLS: usize = 150;
/// The longest host (or host in a warning) the confirmation shows; a longer one is cut from the
/// left so its registrable end stays visible.
const SHOWN_HOST_COLS: usize = 100;
/// The longest recipient the confirmation shows.
const SHOWN_RECIPIENT_COLS: usize = 100;
/// How many recipient lines the confirmation shows before `... and K more recipients`.
const SHOWN_RECIPIENTS: usize = 5;

/// What the `g x` confirmation shows for `url` (from `safe_link`), each line escaped and bounded:
/// first any warning (a user name or password, which reads like a host before `@`; recipients a
/// `mailto:` query adds); then where it goes (the host as the URL serialises it, so an IDN or
/// lookalike host shows in its ASCII `xn--` form, cut from the left when long; or the recipients
/// of a `mailto:` link); then the address, cut to about three rows. The confirmation box also
/// always keeps its answer line, so a warning or the hint cannot be pushed off by what follows.
pub fn link_lines(url: &Url) -> Vec<String> {
    let mut lines = if url.scheme() == "mailto" {
        mail_lines(url)
    } else {
        host_lines(url)
    };
    lines.push(fit(url.as_str(), SHOWN_ADDRESS_COLS));
    lines
}

/// `text` escaped and, when wider than `cols`, cut from the left with a leading `…`.
fn fit_end(text: &str, cols: usize) -> String {
    let escaped = escape(text);
    if width(&escaped) <= cols {
        return escaped;
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 1;
    for grapheme in escaped.graphemes(true).rev() {
        let w = width(grapheme);
        if used + w > cols {
            break;
        }
        kept.push(grapheme);
        used += w;
    }
    kept.push(ELLIPSIS);
    kept.reverse();
    kept.concat()
}

/// The last two labels of `host`, or all of it for an address or a single label.
fn registrable_end(host: &str) -> &str {
    let labels: Vec<&str> = host.split('.').collect();
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') || labels.len() < 3 {
        return host;
    }
    let skip: usize = labels[..labels.len() - 2].iter().map(|l| l.len() + 1).sum();
    &host[skip..]
}

fn host_lines(url: &Url) -> Vec<String> {
    let Some(host) = url.host_str() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if !url.username().is_empty() || url.password().is_some() {
        lines.push(format!(
            "Warning: the part before \"@\" is not where this link goes. It goes to {}.",
            fit_end(registrable_end(host), SHOWN_HOST_COLS / 2)
        ));
    }
    lines.push(format!("Host: {}", fit_end(host, SHOWN_HOST_COLS)));
    lines
}

/// A warning when the parameters of a `mailto:` link add recipients, then `To:`, `Cc:` and
/// `Bcc:` lines (at most `SHOWN_RECIPIENTS`, then a count of the rest) for every recipient of its
/// path and its `to`, `cc` and `bcc` parameters, decoded.
fn mail_lines(url: &Url) -> Vec<String> {
    let mut to = recipients(&percent_decode(url.path()));
    let (mut cc, mut bcc) = (Vec::new(), Vec::new());
    let mut added = false;
    for (name, value) in url.query_pairs() {
        let list = match name.to_ascii_lowercase().as_str() {
            "to" => &mut to,
            "cc" => &mut cc,
            "bcc" => &mut bcc,
            _ => continue,
        };
        let found = recipients(&value);
        added |= !found.is_empty();
        list.extend(found);
    }
    let mut lines = Vec::new();
    if added {
        lines
            .push("Warning: this link adds recipients beyond the address in its path.".to_string());
    }
    if to.is_empty() {
        lines.push("To: (no recipient)".to_string());
    }
    let all: Vec<(&str, &String)> = [("To", &to), ("Cc", &cc), ("Bcc", &bcc)]
        .into_iter()
        .flat_map(|(label, list)| list.iter().map(move |r| (label, r)))
        .collect();
    for (label, recipient) in all.iter().take(SHOWN_RECIPIENTS) {
        lines.push(format!("{label}: {}", fit(recipient, SHOWN_RECIPIENT_COLS)));
    }
    if all.len() > SHOWN_RECIPIENTS {
        lines.push(format!(
            "{ELLIPSIS} and {} more recipients",
            all.len() - SHOWN_RECIPIENTS
        ));
    }
    lines
}

/// The non-empty, comma-separated entries of `list`.
fn recipients(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect()
}

/// `text` with `%XX` escapes decoded; bytes that are not UTF-8 become U+FFFD.
pub(super) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
