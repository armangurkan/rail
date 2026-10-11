//! A bound on how deeply HTML nests before html5ever sees it. html5ever's tree builder scans
//! the open-element stack for many start tags, so a body of nothing but nested `<div>`s costs
//! time quadratic in its depth (40,000 levels: about five seconds; 10 MiB of them: hours).
//! `limit_depth` keeps the start tags that would open an element deeper than the limit out of
//! the text html2text parses, and keeps everything else: the text, the end tags, and the
//! raw-text elements (`script`, `style`...) whole, so their contents never become visible text.
//!
//! The depth it tracks follows the HTML tree-building rules closely enough to never be lower
//! than html5ever's own: comments and raw text hide what looks like tags inside them, an end
//! tag only closes an element it could close (not across a scope boundary, and an ordinary end
//! tag not past a special element), and void elements open nothing.

use std::borrow::Cow;

/// How deeply rendered HTML may nest. Real mail nests a few dozen levels (each table four: table, tbody, tr, td).
pub(crate) const MAX_HTML_DEPTH: usize = 128;
/// Tag names longer than this are not ones html5ever treats specially.
const MAX_TAG_NAME: usize = 32;

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "frame", "hr", "img", "input", "keygen", "link", "meta",
    "param", "source", "track", "wbr",
];
/// Elements whose content is text up to their own end tag (with scripting off, as html2text
/// parses, `noscript` is an ordinary element).
const RAW_TEXT: &[&str] = &[
    "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes",
];
/// The default scope's boundaries: an end tag never closes an element past one of these.
const SCOPE_BOUNDARY: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template",
];
/// The HTML standard's special elements: an ordinary end tag stops at one.
const SPECIAL: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "search",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

fn is_special(name: &str) -> bool {
    SPECIAL.contains(&name)
}

/// One tag found in the text.
struct Tag {
    name: String,
    closing: bool,
    /// Just past its `>`.
    end: usize,
}

/// `html` with the start tags that would nest deeper than `max` left out.
pub(crate) fn limit_depth(html: &str, max: usize) -> Cow<'_, str> {
    let bytes = html.as_bytes();
    let mut stack: Vec<String> = Vec::new();
    let mut kept = String::new();
    let mut copied = 0;
    let mut at = 0;
    while let Some(offset) = bytes[at..].iter().position(|b| *b == b'<') {
        let start = at + offset;
        if let Some(end) = skip_markup(bytes, start) {
            at = end;
            continue;
        }
        let Some(tag) = scan_tag(bytes, start) else {
            at = start + 1;
            continue;
        };
        at = tag.end;
        if tag.closing {
            close(&mut stack, &tag.name);
        } else if tag.name == "plaintext" {
            break;
        } else if RAW_TEXT.contains(&tag.name.as_str()) {
            at = skip_raw_text(bytes, at, &tag.name);
        } else if VOID.contains(&tag.name.as_str()) {
            // Opens nothing.
        } else if stack.len() >= max {
            kept.push_str(&html[copied..start]);
            copied = tag.end;
        } else {
            stack.push(tag.name);
        }
    }
    if copied == 0 {
        return Cow::Borrowed(html);
    }
    kept.push_str(&html[copied..]);
    Cow::Owned(kept)
}

/// The end of a comment, doctype, CDATA section or processing instruction at `start`.
fn skip_markup(bytes: &[u8], start: usize) -> Option<usize> {
    let rest = &bytes[start..];
    if rest.starts_with(b"<!--") {
        let end = find(bytes, start + 4, b"-->").map_or(bytes.len(), |e| e + 3);
        return Some(end);
    }
    if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
        return Some(find(bytes, start + 2, b">").map_or(bytes.len(), |e| e + 1));
    }
    None
}

/// The tag at `start` (a `<`), if one starts there.
fn scan_tag(bytes: &[u8], start: usize) -> Option<Tag> {
    let mut at = start + 1;
    let closing = bytes.get(at) == Some(&b'/');
    if closing {
        at += 1;
    }
    if !bytes.get(at)?.is_ascii_alphabetic() {
        return None;
    }
    let name_start = at;
    while at < bytes.len()
        && !matches!(
            bytes[at],
            b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' | b'/' | b'>'
        )
    {
        at += 1;
    }
    let name = &bytes[name_start..at];
    let name = if name.len() > MAX_TAG_NAME {
        String::from("x-long")
    } else {
        String::from_utf8_lossy(name).to_ascii_lowercase()
    };
    let end = tag_end(bytes, at);
    Some(Tag { name, closing, end })
}

/// Just past the `>` that ends a tag's attributes, honouring quoted values; the end of the
/// text when there is none (html5ever reads the rest as that tag).
fn tag_end(bytes: &[u8], mut at: usize) -> usize {
    let mut quote: Option<u8> = None;
    while at < bytes.len() {
        let b = bytes[at];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return at + 1,
            None => {}
        }
        at += 1;
    }
    bytes.len()
}

/// Just past the end tag of raw-text element `name` whose content starts at `at`.
fn skip_raw_text(bytes: &[u8], mut at: usize, name: &str) -> usize {
    while let Some(found) = find(bytes, at, b"</") {
        let after = found + 2;
        let candidate = bytes.get(after..after + name.len());
        let boundary = bytes.get(after + name.len());
        let ends = candidate.is_some_and(|c| c.eq_ignore_ascii_case(name.as_bytes()))
            && boundary
                .is_none_or(|b| matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' | b'/' | b'>'));
        if ends {
            return tag_end(bytes, after);
        }
        at = after;
    }
    bytes.len()
}

/// Close the element an end tag `name` would close, if any.
fn close(stack: &mut Vec<String>, name: &str) {
    let special = is_special(name);
    for index in (0..stack.len()).rev() {
        let open = stack[index].as_str();
        if open == name {
            stack.truncate(index);
            return;
        }
        if SCOPE_BOUNDARY.contains(&open) || (!special && is_special(open)) {
            return;
        }
    }
}

fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depth_kept(html: &str) -> usize {
        limit_depth(html, 8).matches("<div").count()
    }

    #[test]
    fn shallow_html_is_left_alone() {
        let html = "<html><body><div><p>Hi <b>there</b><br><img src=x></p></div></body></html>";
        assert!(matches!(limit_depth(html, 8), Cow::Borrowed(_)));
    }

    #[test]
    fn deep_nesting_is_cut_at_the_limit_and_text_kept() {
        let html = format!("{}deep{}", "<div>".repeat(100), "</div>".repeat(100));
        let kept = limit_depth(&html, 8);
        assert_eq!(kept.matches("<div").count(), 8);
        assert!(kept.contains("deep"));
        assert_eq!(kept.matches("</div>").count(), 100);
    }

    #[test]
    fn end_tags_that_close_nothing_do_not_lower_the_depth() {
        // html5ever ignores each `</span>`: a `div` (special) is above it.
        assert_eq!(depth_kept(&"<span><div></span>".repeat(50)), 4);
        // Nor does an end tag reach past a table cell.
        assert_eq!(depth_kept(&"<div><table><td></div>".repeat(50)), 3);
        // Nor a "tag" inside a comment, a quoted attribute or raw text.
        assert_eq!(depth_kept(&"<div><!--</div>-->".repeat(50)), 8);
        assert_eq!(depth_kept(&"<div title='</div>'>".repeat(50)), 8);
        assert_eq!(depth_kept(&"<div><script></div></script>".repeat(50)), 8);
    }

    #[test]
    fn raw_text_is_never_cut_open() {
        let html = format!(
            "{}<script>steal()</script><style>x</style>deep",
            "<div>".repeat(50)
        );
        let kept = limit_depth(&html, 8);
        assert!(kept.contains("<script>steal()</script><style>x</style>deep"));
    }

    #[test]
    fn unfinished_tags_and_odd_markup_end_cleanly() {
        for html in [
            "<div",
            "<!--",
            "<script>never ends",
            "</",
            "<1 <a b='",
            "<plaintext><div>",
        ] {
            let _ = limit_depth(html, 2);
        }
        let long = format!("<{}>", "a".repeat(100));
        assert!(matches!(limit_depth(&long.repeat(5), 2), Cow::Owned(_)));
    }
}
