//! HTML bodies as plain text, in bounded time. html2text renders offline (it parses with
//! html5ever and drops `head`, `script`, `style` and `link`; an `img` becomes its alt text;
//! nothing is fetched), but html5ever's tree building costs time quadratic in how deeply the
//! document nests, and nesting can come from end tags that close nothing or from formatting
//! elements html5ever reopens on its own. So html2text only sees HTML of at most
//! `HTML_RENDER_CAP` bytes whose shape one linear pass has bounded; anything else is stripped
//! to text by that same pass.
//!
//! The pass runs html5ever's own tokenizer (so it reads tags, attributes, comments and raw text
//! exactly as the parser will) and estimates the tree from the tokens, erring high: an end tag
//! only closes what the tree builder would let it close, void elements open nothing, and every
//! formatting element the builder could reopen counts. Content whose tree rules are too
//! intricate to follow (`svg`, `math`, `select`, `frameset`, `template`) is never rendered.

use std::cell::RefCell;

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::{Attribute, LocalName};

/// The most HTML html2text is given; larger bodies are stripped.
pub(crate) const HTML_RENDER_CAP: usize = 256 * 1024;
/// How deeply rendered HTML may nest. Real mail nests a few dozen levels (each table four:
/// table, tbody, tr, td).
pub(crate) const MAX_HTML_DEPTH: usize = 128;
/// Start tags in HTML that is rendered.
pub(crate) const MAX_START_TAGS: usize = 50_000;
/// Formatting elements the tree builder could reopen at once.
pub(crate) const MAX_ACTIVE_FORMATTING: usize = 64;
/// Elements the tree builder could reopen in all (each text and start tag may reopen every
/// active formatting element).
pub(crate) const MAX_REOPENED: usize = 200_000;
const TEXT_WIDTH: usize = 100;
/// Indenting elements (quotes, lists) open at once: html2text indents each line once per
/// level, so deep quotes multiply the text's size.
pub(crate) const MAX_INDENT_DEPTH: usize = 24;
const INDENTING: &[&str] = &["blockquote", "dd", "dl", "ol", "ul"];
/// Elements the adoption agency may create for one formatting end tag (eight outer passes,
/// three inner).
const ADOPTION_CLONES: usize = 24;

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "frame", "hr", "img", "image", "input", "keygen", "link",
    "meta", "param", "source", "track", "wbr",
];
/// Content Rail does not render through html2text at all.
const UNCERTAIN: &[&str] = &["svg", "math", "select", "frameset", "template"];
const FORMATTING: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];
/// Elements that put a marker in the list of active formatting elements.
const MARKERS: &[&str] = &["applet", "caption", "marquee", "object", "td", "th"];
/// An end tag never closes an element past one of these: the scope boundaries of every scope
/// the tree builder checks, together.
const BOUNDARY: &[&str] = &[
    "applet", "button", "caption", "html", "marquee", "object", "ol", "table", "tbody", "td",
    "tfoot", "th", "thead", "tr", "ul",
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
/// Raw text whose content the stripped text leaves out.
const HIDDEN_RAW: &[&str] = &["script", "style", "title", "iframe", "noembed", "noframes"];
/// What may stand in `head` without ending it.
const HEAD_CONTENT: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "noscript", "script", "style", "template",
    "title",
];
/// Tags that start or end a line of stripped text.
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];
const CELL: &[&str] = &["td", "th"];

/// `html` as plain text: rendered by html2text when it is small and its shape is bounded,
/// stripped otherwise.
pub(crate) fn html_text(html: &str) -> String {
    let scan = scan(html);
    if html.len() <= HTML_RENDER_CAP && scan.renderable() {
        let rendered = html2text::config::plain_no_decorate()
            .allow_width_overflow()
            .string_from_read(html.as_bytes(), TEXT_WIDTH);
        if let Ok(text) = rendered {
            return text.trim_end().to_string();
        }
    }
    scan.stripped()
}

/// Whether html2text would be given `html` (tests and callers that only ask).
#[cfg(test)]
pub(crate) fn renderable(html: &str) -> bool {
    html.len() <= HTML_RENDER_CAP && scan(html).renderable()
}

/// One pass of html5ever's tokenizer over `html`.
fn scan(html: &str) -> Scan {
    let tokenizer = Tokenizer::new(Sink(RefCell::default()), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    // The sink never asks the tokenizer to pause for a script, so one feed reads it all.
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.take()
}

struct Sink(RefCell<Scan>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut scan = self.0.borrow_mut();
        match token {
            Token::TagToken(tag) => scan.tag(tag),
            Token::CharacterTokens(text) => {
                scan.characters(&text);
                TokenSinkResult::Continue
            }
            // Comments, doctypes, NULs and parse errors add no text and no elements.
            _ => TokenSinkResult::Continue,
        }
    }
}

/// What the tree builder would make the tokenizer do after start tag `name`.
fn tokenizer_state(name: &str) -> TokenSinkResult<()> {
    match name {
        "script" => TokenSinkResult::RawData(RawKind::ScriptData),
        "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
            TokenSinkResult::RawData(RawKind::Rawtext)
        }
        "textarea" | "title" => TokenSinkResult::RawData(RawKind::Rcdata),
        "plaintext" => TokenSinkResult::Plaintext,
        _ => TokenSinkResult::Continue,
    }
}

/// An entry of the list of active formatting elements, with the number of markers before it.
/// Markers are only counted, never cleared: Rail cannot always tell when the tree builder
/// clears one, and keeping entries errs high.
type Formatting = (LocalName, Vec<Attribute>, usize);

#[derive(Default)]
struct Scan {
    /// The open elements, as far as Rail can tell; `uncertain` once that is not far enough.
    stack: Vec<LocalName>,
    formatting: Vec<Formatting>,
    markers: usize,
    start_tags: usize,
    reopened: usize,
    uncertain: bool,
    text: String,
    hidden_raw: Option<LocalName>,
    templates: usize,
    in_head: bool,
}

impl Scan {
    fn renderable(&self) -> bool {
        !self.uncertain
    }

    fn tag(&mut self, tag: Tag) -> TokenSinkResult<()> {
        match tag.kind {
            TagKind::StartTag => {
                let state = tokenizer_state(&tag.name);
                self.strip_start(&tag.name);
                self.shape_start(tag);
                state
            }
            TagKind::EndTag => {
                self.strip_end(&tag.name);
                self.shape_end(&tag.name);
                TokenSinkResult::Continue
            }
        }
    }

    fn characters(&mut self, text: &str) {
        if !self.uncertain {
            self.reopened += self.active();
            self.check();
        }
        self.strip_text(text);
    }

    fn active(&self) -> usize {
        self.formatting.len()
    }

    fn check(&mut self) {
        self.uncertain |= self.start_tags > MAX_START_TAGS
            || self.stack.len() > MAX_HTML_DEPTH
            || self.active() > MAX_ACTIVE_FORMATTING
            || self.reopened > MAX_REOPENED;
    }

    fn shape_start(&mut self, tag: Tag) {
        if self.uncertain {
            return;
        }
        let name: &str = &tag.name;
        self.start_tags += 1;
        self.reopened += self.active();
        self.uncertain |= UNCERTAIN.contains(&name);
        if MARKERS.contains(&name) {
            self.markers += 1;
        }
        if FORMATTING.contains(&name) {
            if matches!(name, "a" | "nobr") && self.has_active(&tag.name) {
                // A second `a` (or `nobr`) runs the adoption agency for the first.
                self.reopened += ADOPTION_CLONES;
            }
            self.push_formatting(&tag);
        }
        let indenting = INDENTING.contains(&name);
        if !VOID.contains(&name) {
            self.stack.push(tag.name);
            self.uncertain |= indenting && self.indents() > MAX_INDENT_DEPTH;
        }
        self.check();
    }

    fn has_active(&self, name: &LocalName) -> bool {
        self.formatting
            .iter()
            .any(|(n, _, m)| *m == self.markers && n == name)
    }

    /// Open elements that indent what they hold.
    fn indents(&self) -> usize {
        self.stack
            .iter()
            .filter(|n| INDENTING.contains(&&***n))
            .count()
    }

    /// Add a formatting element, keeping at most three identical ones after the last marker
    /// (the tree builder's "Noah's Ark" rule).
    fn push_formatting(&mut self, tag: &Tag) {
        let mut attrs = tag.attrs.clone();
        attrs.sort();
        let same: Vec<usize> = (0..self.formatting.len())
            .filter(|&i| {
                let (n, a, m) = &self.formatting[i];
                *m == self.markers && *n == tag.name && *a == attrs
            })
            .collect();
        if same.len() >= 3 {
            self.formatting.remove(same[0]);
        }
        self.formatting
            .push((tag.name.clone(), attrs, self.markers));
    }

    fn shape_end(&mut self, name: &LocalName) {
        if self.uncertain {
            return;
        }
        let tag: &str = name;
        match tag {
            // The tree builder keeps `html` and `body` open to the end.
            "html" | "body" => {}
            // `</form>` takes the form out of the stack and leaves what is above it.
            "form" => {
                if let Some(index) = self.in_scope(name) {
                    self.stack.remove(index);
                }
            }
            _ if FORMATTING.contains(&tag) => {
                self.end_formatting(name);
                self.check();
            }
            _ => {
                self.close(name);
            }
        }
    }

    /// The index of the open `name` an end tag could reach, if any.
    fn in_scope(&self, name: &LocalName) -> Option<usize> {
        let special = SPECIAL.contains(&&**name);
        for index in (0..self.stack.len()).rev() {
            let open = &self.stack[index];
            if open == name {
                return Some(index);
            }
            let open: &str = open;
            if BOUNDARY.contains(&open) || (!special && SPECIAL.contains(&open)) {
                return None;
            }
        }
        None
    }

    /// Close `name` and what is above it, if an end tag could; true when it did.
    fn close(&mut self, name: &LocalName) -> bool {
        match self.in_scope(name) {
            Some(index) => {
                self.stack.truncate(index);
                true
            }
            None => false,
        }
    }

    /// A formatting end tag: the list entry goes only when the tree builder would drop it.
    fn end_formatting(&mut self, name: &LocalName) {
        let entry = self
            .formatting
            .iter()
            .rposition(|(n, _, m)| *m == self.markers && n == name);
        let Some(entry) = entry else {
            self.close(name);
            return;
        };
        if !self.stack.contains(name) || self.close(name) {
            self.formatting.remove(entry);
        } else {
            // A special element stands above it: the adoption agency clones elements.
            self.reopened += ADOPTION_CLONES;
        }
    }

    fn hidden(&self) -> bool {
        self.hidden_raw.is_some() || self.templates > 0
    }

    fn strip_start(&mut self, name: &LocalName) {
        let tag: &str = name;
        if HIDDEN_RAW.contains(&tag) {
            self.hidden_raw = Some(name.clone());
            return;
        }
        if tag == "template" {
            self.templates += 1;
        } else if tag == "head" {
            self.in_head = true;
        } else if self.in_head && !HEAD_CONTENT.contains(&tag) {
            self.in_head = false;
        }
        if self.hidden() {
            return;
        }
        if BLOCK.contains(&tag) {
            self.text.push('\n');
        } else if CELL.contains(&tag) {
            self.push_space();
        }
    }

    fn strip_end(&mut self, name: &LocalName) {
        if self.hidden_raw.as_ref() == Some(name) {
            self.hidden_raw = None;
            return;
        }
        let tag: &str = name;
        if tag == "template" {
            self.templates = self.templates.saturating_sub(1);
        } else if tag == "head" {
            self.in_head = false;
        } else if BLOCK.contains(&tag) && !self.hidden() {
            self.text.push('\n');
        }
    }

    /// Text outside hidden raw text and `head`, with runs of white space made one space.
    fn strip_text(&mut self, text: &str) {
        if self.hidden() {
            return;
        }
        if self.in_head {
            if text.trim().is_empty() {
                return;
            }
            self.in_head = false;
        }
        for c in text.chars() {
            if c.is_whitespace() {
                self.push_space();
            } else {
                self.text.push(c);
            }
        }
    }

    fn push_space(&mut self) {
        if !self.text.ends_with([' ', '\n']) && !self.text.is_empty() {
            self.text.push(' ');
        }
    }

    /// The stripped text: each line trimmed, at most one blank line in a row.
    fn stripped(self) -> String {
        let mut out = String::with_capacity(self.text.len());
        let mut blank = 0;
        for line in self.text.split('\n').map(str::trim) {
            if line.is_empty() {
                blank += 1;
                continue;
            }
            if !out.is_empty() {
                out.push_str(if blank > 1 { "\n\n" } else { "\n" });
            }
            out.push_str(line);
            blank = 0;
        }
        out
    }
}
