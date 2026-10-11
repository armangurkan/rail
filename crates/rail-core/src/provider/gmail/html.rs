//! HTML bodies as plain text, by Rail's own scanner. A browser parser builds a tree, and on
//! hostile mail its cost can grow faster than the mail (deep nesting, thousands of distinct
//! attributes, huge list numbers, indentation repeated on every line); this scanner builds
//! nothing. It reads each byte a constant number of times, keeps no stack and no lists,
//! fetches nothing and runs nothing:
//!
//! - text is copied, white space collapsed to one space;
//! - tags are read only for their first few name bytes (block tags make line breaks, cells a
//!   space); attributes are skipped, quotes honoured only to find the tag's end, and a tag
//!   longer than `MAX_TAG_BYTES` ends there;
//! - comments are dropped, and so is the content of `script`, `style`, `head`, `title`,
//!   `template` and `noscript`;
//! - character references are decoded: numeric ones (never to a control character other than
//!   a line break or tab) and a small table of common names; anything else stays as written.
//!
//! The text is never longer than the HTML.

/// Bytes one tag (or `<!...>` declaration) may span before Rail treats it as closed.
pub(crate) const MAX_TAG_BYTES: usize = 4096;
/// The longest character reference name Rail looks up.
const MAX_ENTITY_NAME: usize = 32;
/// Digits of a numeric character reference (more cannot be a code point).
const MAX_DIGITS: usize = 8;
/// Tag name bytes compared; longer names match nothing.
const NAME_BYTES: usize = 10;
/// Consecutive line breaks kept.
const MAX_NEWLINES: usize = 2;

const BLOCK: &[&[u8]] = &[
    b"p",
    b"div",
    b"br",
    b"li",
    b"tr",
    b"h1",
    b"h2",
    b"h3",
    b"h4",
    b"h5",
    b"h6",
    b"blockquote",
    b"pre",
    b"table",
    b"ul",
    b"ol",
    b"dl",
    b"dt",
    b"dd",
    b"hr",
    b"section",
    b"article",
    b"header",
    b"footer",
];
const CELL: &[&[u8]] = &[b"td", b"th"];
/// Elements whose content is dropped up to their own end tag.
const HIDDEN: &[&[u8]] = &[
    b"script",
    b"style",
    b"head",
    b"title",
    b"template",
    b"noscript",
];
/// Hidden elements that can stand inside `head` and hold text that looks like tags.
const HIDDEN_IN_HEAD: &[&[u8]] = &[b"script", b"style", b"title", b"template", b"noscript"];

/// The character references Rail decodes by name.
const ENTITIES: &[(&[u8], char)] = &[
    (b"amp", '&'),
    (b"lt", '<'),
    (b"gt", '>'),
    (b"quot", '"'),
    (b"apos", '\''),
    (b"nbsp", '\u{a0}'),
    (b"copy", '©'),
    (b"reg", '®'),
    (b"trade", '™'),
    (b"hellip", '…'),
    (b"mdash", '—'),
    (b"ndash", '–'),
    (b"lsquo", '‘'),
    (b"rsquo", '’'),
    (b"sbquo", '‚'),
    (b"ldquo", '“'),
    (b"rdquo", '”'),
    (b"bdquo", '„'),
    (b"laquo", '«'),
    (b"raquo", '»'),
    (b"bull", '•'),
    (b"middot", '·'),
    (b"deg", '°'),
    (b"euro", '€'),
    (b"pound", '£'),
    (b"yen", '¥'),
    (b"cent", '¢'),
    (b"sect", '§'),
    (b"para", '¶'),
    (b"times", '×'),
    (b"divide", '÷'),
    (b"plusmn", '±'),
    (b"frac12", '½'),
    (b"iexcl", '¡'),
    (b"iquest", '¿'),
    (b"aacute", 'á'),
    (b"agrave", 'à'),
    (b"acirc", 'â'),
    (b"auml", 'ä'),
    (b"aring", 'å'),
    (b"ccedil", 'ç'),
    (b"eacute", 'é'),
    (b"egrave", 'è'),
    (b"ecirc", 'ê'),
    (b"euml", 'ë'),
    (b"iacute", 'í'),
    (b"icirc", 'î'),
    (b"iuml", 'ï'),
    (b"ntilde", 'ñ'),
    (b"oacute", 'ó'),
    (b"ocirc", 'ô'),
    (b"ouml", 'ö'),
    (b"oslash", 'ø'),
    (b"uacute", 'ú'),
    (b"ucirc", 'û'),
    (b"uuml", 'ü'),
    (b"szlig", 'ß'),
    (b"Aacute", 'Á'),
    (b"Agrave", 'À'),
    (b"Auml", 'Ä'),
    (b"Ccedil", 'Ç'),
    (b"Eacute", 'É'),
    (b"Egrave", 'È'),
    (b"Ntilde", 'Ñ'),
    (b"Ouml", 'Ö'),
    (b"Uuml", 'Ü'),
];

/// `html` as plain text (see the module documentation).
pub(crate) fn html_text(html: &str) -> String {
    let mut scanner = Scanner {
        bytes: html.as_bytes(),
        out: Vec::with_capacity(html.len().min(1 << 20)),
        space: false,
        newlines: 0,
    };
    scanner.run();
    while scanner.out.last() == Some(&b'\n') {
        scanner.out.pop();
    }
    // Only whole UTF-8 sequences of the input and whole encoded characters are written.
    String::from_utf8(scanner.out)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

/// A byte that ends a tag name.
fn ends_name(b: u8) -> bool {
    is_space(b) || b == b'/' || b == b'>'
}

/// The first `NAME_BYTES` bytes of a tag name, lower-cased; `None` when it is longer.
struct Name {
    bytes: [u8; NAME_BYTES],
    len: usize,
    long: bool,
}

impl Name {
    fn get(&self) -> Option<&[u8]> {
        (!self.long).then(|| &self.bytes[..self.len])
    }

    fn is_in(&self, list: &[&[u8]]) -> bool {
        self.get().is_some_and(|name| list.contains(&name))
    }
}

struct Scanner<'a> {
    bytes: &'a [u8],
    out: Vec<u8>,
    /// White space seen since the last text.
    space: bool,
    /// Line breaks at the end of `out`.
    newlines: usize,
}

impl Scanner<'_> {
    fn run(&mut self) {
        let mut at = 0;
        while at < self.bytes.len() {
            at = match self.bytes[at] {
                b'<' => self.markup(at),
                b'&' => self.reference(at),
                b if is_space(b) => {
                    self.space = true;
                    at + 1
                }
                _ => self.text(at),
            };
        }
    }

    /// A run of text bytes from `at`.
    fn text(&mut self, at: usize) -> usize {
        let end = self.bytes[at..]
            .iter()
            .position(|&b| b == b'<' || b == b'&' || is_space(b))
            .map_or(self.bytes.len(), |p| at + p);
        self.visible(at, end);
        end
    }

    fn visible(&mut self, start: usize, end: usize) {
        self.flush_space();
        self.out.extend_from_slice(&self.bytes[start..end]);
        self.newlines = 0;
    }

    fn flush_space(&mut self) {
        if self.space && !self.out.is_empty() && self.newlines == 0 {
            self.out.push(b' ');
        }
        self.space = false;
    }

    fn push_char(&mut self, c: char) {
        if c == '\n' || c == '\t' {
            self.space = true;
            return;
        }
        self.flush_space();
        let mut buf = [0u8; 4];
        self.out
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.newlines = 0;
    }

    fn newline(&mut self) {
        self.space = false;
        if !self.out.is_empty() && self.newlines < MAX_NEWLINES {
            self.out.push(b'\n');
            self.newlines += 1;
        }
    }

    /// What starts with `<` at `at`: a comment, a declaration, a tag, or a plain `<`.
    fn markup(&mut self, at: usize) -> usize {
        let rest = &self.bytes[at..];
        if rest.starts_with(b"<!--") {
            return find(self.bytes, at + 2, b"-->").map_or(self.bytes.len(), |e| e + 3);
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"<?") {
            return self.tag_end(at + 2, at);
        }
        let closing = rest.get(1) == Some(&b'/');
        let name_at = at + 1 + usize::from(closing);
        if !self.bytes.get(name_at).is_some_and(u8::is_ascii_alphabetic) {
            self.visible(at, at + 1);
            return at + 1;
        }
        let (name, after_name) = self.name(name_at, at);
        let end = self.tag_end(after_name, at);
        if name.is_in(BLOCK) {
            self.newline();
        } else if name.is_in(CELL) {
            self.space = true;
        }
        match name.get() {
            Some(hidden) if !closing && HIDDEN.contains(&hidden) => self.skip_hidden(end, hidden),
            _ => end,
        }
    }

    /// The tag name at `at`, and where it ends (within the tag's byte cap from `start`).
    fn name(&self, at: usize, start: usize) -> (Name, usize) {
        let mut name = Name {
            bytes: [0; NAME_BYTES],
            len: 0,
            long: false,
        };
        let limit = self.bytes.len().min(start + MAX_TAG_BYTES);
        let mut end = at;
        while end < limit && !ends_name(self.bytes[end]) {
            if name.len < NAME_BYTES {
                name.bytes[name.len] = self.bytes[end].to_ascii_lowercase();
                name.len += 1;
            } else {
                name.long = true;
            }
            end += 1;
        }
        (name, end)
    }

    /// Just past the `>` that ends the tag begun at `start`, reading from `at`. A quote opens
    /// a value only after `=`; at `MAX_TAG_BYTES` (or the end of the text) the tag ends.
    fn tag_end(&self, at: usize, start: usize) -> usize {
        let limit = self.bytes.len().min(start + MAX_TAG_BYTES);
        let mut quote: Option<u8> = None;
        let mut after_equals = false;
        for index in at.min(limit)..limit {
            let b = self.bytes[index];
            match quote {
                Some(q) if b == q => quote = None,
                Some(_) => {}
                None if b == b'>' => return index + 1,
                None if b == b'=' => after_equals = true,
                None if (b == b'"' || b == b'\'') && after_equals => quote = Some(b),
                None if is_space(b) => {}
                None => after_equals = false,
            }
        }
        limit
    }

    /// Just past the end tag of hidden element `name` whose content starts at `at`. `head`
    /// also ends where `body` starts, and skips the hidden elements inside it whole.
    fn skip_hidden(&self, mut at: usize, name: &[u8]) -> usize {
        let head = name == b"head";
        while let Some(offset) = self.bytes[at..].iter().position(|&b| b == b'<') {
            let open = at + offset;
            if self.is_end_tag(open, name) {
                return self.tag_end(open + 2 + name.len(), open);
            }
            if head && self.starts_tag(open + 1, b"body") {
                return open;
            }
            at = open + 1;
            let inner = HIDDEN_IN_HEAD
                .iter()
                .filter(|_| head)
                .find(|n| self.starts_tag(open + 1, n));
            if let Some(inner) = inner {
                let end = self.tag_end(open + 1 + inner.len(), open);
                at = self.skip_hidden(end, inner);
            }
        }
        self.bytes.len()
    }

    /// Whether `</name` followed by the end of a name starts at `at`.
    fn is_end_tag(&self, at: usize, name: &[u8]) -> bool {
        self.bytes.get(at + 1) == Some(&b'/') && self.starts_tag(at + 2, name)
    }

    /// Whether `name` (case-insensitively) followed by the end of a name starts at `at`.
    fn starts_tag(&self, at: usize, name: &[u8]) -> bool {
        let Some(found) = self.bytes.get(at..at + name.len()) else {
            return false;
        };
        found.eq_ignore_ascii_case(name)
            && self
                .bytes
                .get(at + name.len())
                .is_none_or(|&b| ends_name(b))
    }

    /// A character reference at `at`, decoded; a plain `&` when it is not one Rail decodes.
    fn reference(&mut self, at: usize) -> usize {
        match decode_reference(self.bytes, at) {
            Some((c, end)) => {
                self.push_char(c);
                end
            }
            None => {
                self.visible(at, at + 1);
                at + 1
            }
        }
    }
}

/// The character and the end of the reference starting with `&` at `at`.
fn decode_reference(bytes: &[u8], at: usize) -> Option<(char, usize)> {
    if bytes.get(at + 1) == Some(&b'#') {
        return decode_numeric(bytes, at + 2);
    }
    let start = at + 1;
    let len = bytes[start..]
        .iter()
        .take(MAX_ENTITY_NAME + 1)
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();
    if len == 0 || len > MAX_ENTITY_NAME || bytes.get(start + len) != Some(&b';') {
        return None;
    }
    let name = &bytes[start..start + len];
    let (_, c) = ENTITIES.iter().find(|(n, _)| *n == name)?;
    Some((*c, start + len + 1))
}

/// `&#N;` or `&#xH;` (the `;` optional), starting just past `#`.
fn decode_numeric(bytes: &[u8], at: usize) -> Option<(char, usize)> {
    let hex = matches!(bytes.get(at), Some(b'x' | b'X'));
    let start = at + usize::from(hex);
    let radix = if hex { 16 } else { 10 };
    let len = bytes[start.min(bytes.len())..]
        .iter()
        .take(MAX_DIGITS + 1)
        .take_while(|b| char::from(**b).is_digit(radix))
        .count();
    if len == 0 || len > MAX_DIGITS {
        return None;
    }
    let digits = std::str::from_utf8(&bytes[start..start + len]).ok()?;
    let code = u32::from_str_radix(digits, radix).ok()?;
    let allowed = code == 0x09 || code == 0x0a || (code >= 0x20 && !(0x7f..=0x9f).contains(&code));
    let c = char::from_u32(code).filter(|_| allowed)?;
    let end = start + len;
    Some((c, end + usize::from(bytes.get(end) == Some(&b';'))))
}

/// The first `needle` at or after `from`.
fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}
