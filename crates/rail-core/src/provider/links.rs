//! Link footnotes: the addresses a message links to, numbered by first appearance, and the
//! `[n]` markers that stand for them in its text. Shared by the HTML scanner and plain-text
//! bodies. Addresses are outside text: the reader validates each one again before it shows or
//! opens it.

use std::collections::HashMap;

/// Links kept per message; later ones get no marker.
pub const MAX_LINKS: usize = 200;
/// The longest address kept, in bytes.
pub const MAX_LINK_BYTES: usize = 2048;
/// The schemes Rail keeps, and the only ones the reader opens.
pub const SCHEMES: [&str; 3] = ["http:", "https:", "mailto:"];

/// True when Rail keeps `address`: 1 to `MAX_LINK_BYTES` bytes, no space or control byte, and
/// one of `SCHEMES` (in any case).
pub fn allowed(address: &str) -> bool {
    let bytes = address.as_bytes();
    let fits = !bytes.is_empty() && bytes.len() <= MAX_LINK_BYTES;
    let clean = bytes.iter().all(|&b| b > b' ' && b != 0x7f);
    let scheme = SCHEMES.iter().any(|s| {
        bytes
            .get(..s.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(s.as_bytes()))
    });
    fits && clean && scheme
}

/// A body's text with its markers, and the addresses they stand for (link `n` is
/// `links[n - 1]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub links: Vec<String>,
}

/// The addresses of one message, numbered by first appearance.
#[derive(Debug, Default)]
pub(crate) struct Numbering {
    links: Vec<String>,
    numbers: HashMap<String, usize>,
}

impl Numbering {
    /// The marker number of `address` (from 1), given the next one on first sight. `None` when
    /// Rail does not keep the address or `MAX_LINKS` are numbered already.
    pub(crate) fn number(&mut self, address: &str) -> Option<usize> {
        if !allowed(address) {
            return None;
        }
        if let Some(&known) = self.numbers.get(address) {
            return Some(known);
        }
        if self.links.len() >= MAX_LINKS {
            return None;
        }
        self.links.push(address.to_string());
        let number = self.links.len();
        self.numbers.insert(address.to_string(), number);
        Some(number)
    }

    pub(crate) fn into_links(self) -> Vec<String> {
        self.links
    }
}

/// `text` with ` [n]` after each bare `http://` or `https://` address that starts a word.
/// Linear: each byte is looked at a constant number of times.
pub(crate) fn plain_links(text: &str) -> Rendered {
    let mut numbering = Numbering::default();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut at) = (0, 0);
    while at < text.len() {
        let Some(len) = address_at(text.as_bytes(), at) else {
            at += 1;
            continue;
        };
        let end = at + len;
        out.push_str(&text[copied..end]);
        if let Some(number) = numbering.number(&text[at..end]) {
            out.push_str(&format!(" [{number}]"));
        }
        copied = end;
        at = end;
    }
    out.push_str(&text[copied..]);
    Rendered {
        text: out,
        links: numbering.into_links(),
    }
}

/// The length of the address starting a word at `at`, if one does. An address runs to white
/// space, a control byte, `<`, `>` or `"`, less trailing punctuation. Every byte it ends on
/// is ASCII, so `at` and the end are character boundaries.
fn address_at(bytes: &[u8], at: usize) -> Option<usize> {
    let rest = &bytes[at..];
    let scheme = ["https://", "http://"].into_iter().find(|s| {
        rest.get(..s.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(s.as_bytes()))
    })?;
    if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
        return None;
    }
    let len = rest
        .iter()
        .position(|&b| b <= b' ' || b == 0x7f || matches!(b, b'<' | b'>' | b'"'))
        .unwrap_or(rest.len());
    let kept = without_trailing_punctuation(&rest[..len]);
    (kept > scheme.len()).then_some(kept)
}

/// The length of `address` without trailing `.,;:!?'`, and without a trailing `)`, `]` or
/// `}` when the address has no matching opener at all.
fn without_trailing_punctuation(address: &[u8]) -> usize {
    let opens = |b: u8| address.contains(&b);
    let (round, square, curly) = (opens(b'('), opens(b'['), opens(b'{'));
    let mut end = address.len();
    while let Some(&last) = address[..end].last() {
        let strip = match last {
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b'\'' => true,
            b')' => !round,
            b']' => !square,
            b'}' => !curly,
            _ => false,
        };
        if !strip {
            break;
        }
        end -= 1;
    }
    end
}
