//! Whether an attachment's bytes are what its declared type says (#107). The type is the
//! sender's word; the system opener on Linux may sniff the content instead, so a part declared
//! `image/png` whose bytes are `<svg onload=…>` could open in a browser.

/// What each `OPENABLE` type's bytes have to look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signature {
    Pdf,
    Png,
    Jpeg,
    Gif,
    WebP,
    Bmp,
    Tiff,
    Heic,
    Ooxml,
    Odf,
    Ole,
    Rtf,
    Text,
}

/// How far into a text part a tag may start for Rail to refuse it: 64 KiB, sixteen times the
/// most a common sniffer reads (`file`'s HTML rules search 4096 bytes, shared-mime-info's 256),
/// so a sniffer that reads further one day is still covered, while a long CSV is not refused
/// for a tag deep inside it that no opener looks at. A tag that starts inside the window is
/// read to its end even past it, as `file` matches one whose start alone is in range.
pub const TEXT_SNIFF_WINDOW: usize = 64 * 1024;

/// The rule for `mime_type`, parameters stripped and case ignored; `None` outside `OPENABLE`.
#[must_use]
pub fn signature(mime_type: &str) -> Option<Signature> {
    super::attachments::openable_entry(mime_type).map(|(_, _, rule)| rule)
}

/// Whether `bytes` match `mime_type`'s signature. A type outside `OPENABLE` matches nothing,
/// and neither does an empty part.
#[must_use]
pub fn content_matches(mime_type: &str, bytes: &[u8]) -> bool {
    let Some((name, _, rule)) = super::attachments::openable_entry(mime_type) else {
        return false;
    };
    match rule {
        Signature::Pdf => bytes.starts_with(b"%PDF-"),
        Signature::Png => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        Signature::Jpeg => bytes.starts_with(b"\xff\xd8\xff"),
        Signature::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        Signature::WebP => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        Signature::Bmp => is_bmp(bytes),
        Signature::Tiff => bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*"),
        Signature::Heic => is_heic(bytes),
        Signature::Ooxml => bytes.starts_with(ZIP_ENTRY),
        Signature::Odf => is_odf(bytes, name),
        Signature::Ole => bytes.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"),
        Signature::Rtf => bytes.starts_with(b"{\\rtf"),
        Signature::Text => is_plain_text(bytes),
    }
}

/// A ZIP local file header, which every OOXML and ODF file starts with.
const ZIP_ENTRY: &[u8] = b"PK\x03\x04";
/// The DIB header sizes a BMP may carry (core, OS/2 2, info, v2 to v5), read at offset 14.
const BMP_HEADER_SIZES: [u32; 8] = [12, 16, 40, 52, 56, 64, 108, 124];
/// The `ftyp` major brands of HEIF images.
const HEIC_BRANDS: [&[u8; 4]; 6] = [b"heic", b"heix", b"mif1", b"msf1", b"heis", b"hevc"];

/// `BM`, then a DIB header of a size a BMP has: two bytes alone would let `BM<svg …>` through.
fn is_bmp(bytes: &[u8]) -> bool {
    let Some(size) = bytes.get(14..18) else {
        return false;
    };
    let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]);
    bytes.starts_with(b"BM") && BMP_HEADER_SIZES.contains(&size)
}

/// An ISO base media `ftyp` box whose major brand is a HEIF image's.
fn is_heic(bytes: &[u8]) -> bool {
    bytes.get(4..8) == Some(b"ftyp")
        && bytes
            .get(8..12)
            .is_some_and(|brand| HEIC_BRANDS.iter().any(|known| brand == known.as_slice()))
}

/// A ZIP's first entry, from its local file header: the entry's name and the bytes from its
/// data on. `None` when `bytes` is not a ZIP or the header is cut short.
fn first_zip_entry(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let field = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
    };
    if !bytes.starts_with(ZIP_ENTRY) {
        return None;
    }
    let (name_len, extra_len) = (field(26)?, field(28)?);
    let name = bytes.get(30..30 + name_len)?;
    let data = bytes.get(30 + name_len + extra_len..)?;
    Some((name, data))
}

/// A ZIP whose first entry is `mimetype`, holding the declared type: how ODF lays out every
/// file (OpenDocument 1.2, part 3, §3.3), read from the local header without a ZIP reader.
fn is_odf(bytes: &[u8], mime_type: &str) -> bool {
    first_zip_entry(bytes)
        .is_some_and(|(name, data)| name == b"mimetype" && data.starts_with(mime_type.as_bytes()))
}

/// Text a sniffing opener would still call text. Any 8-bit encoding is taken — UTF-8, or the
/// Latin-1 and Windows-1252 a spreadsheet exports — because every byte string without a NUL
/// is valid Latin-1, so requiring UTF-8 would refuse real files and keep nothing out. Refused:
/// a NUL (binary, or UTF-16 text, which hides markup from the rest of the check); a start,
/// after a UTF-8 byte-order mark and white space, of `<` (any markup), `#!` (a script),
/// `%PDF` or `%!` (PostScript); and a tag a sniffer takes for HTML, SVG or XML starting in
/// the first `TEXT_SNIFF_WINDOW` bytes.
fn is_plain_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.contains(&0) {
        return false;
    }
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let start = body.trim_ascii_start();
    let scripted = [b"<".as_slice(), b"#!", b"%PDF", b"%!"]
        .iter()
        .any(|prefix| start.starts_with(prefix));
    !scripted && !has_sniffed_tag(bytes)
}

/// White space as `file` and C's `isspace` count it: vertical tab and form feed included,
/// which Rust's `is_ascii_whitespace` leaves out.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Tag names a sniffer reads as HTML or SVG. Matched as a prefix, ASCII case ignored, after a
/// `<` and any white space: shared-mime-info matches `<HEAD` as a plain prefix, so requiring a
/// `>` or a space after the name would let `<header>` through.
const SNIFFED_NAMES: [&[u8]; 10] = [
    b"html", b"head", b"body", b"title", b"script", b"style", b"table", b"svg", b"iframe", b"h1",
];
/// Declarations a sniffer reads as HTML or XML, matched the same way.
const SNIFFED_DECLARATIONS: [&[u8]; 3] = [b"!doctype", b"?xml", b"!--"];

/// Whether a sniffed tag starts in the first `TEXT_SNIFF_WINDOW` bytes; it may run past them.
fn has_sniffed_tag(bytes: &[u8]) -> bool {
    let window = bytes.len().min(TEXT_SNIFF_WINDOW);
    (0..window).any(|at| bytes[at] == b'<' && is_sniffed_tag(&bytes[at + 1..]))
}

/// Whether `rest`, what follows a `<`, is a sniffed tag: white space, then a sniffed name or
/// declaration, or `a`, white space and `href` — the whitespace any run of the six kinds, as
/// `file`'s compact-whitespace match takes it.
fn is_sniffed_tag(rest: &[u8]) -> bool {
    let rest = skip_space(rest);
    let starts = |prefix: &[u8]| {
        rest.get(..prefix.len())
            .is_some_and(|found| found.eq_ignore_ascii_case(prefix))
    };
    if SNIFFED_NAMES
        .iter()
        .chain(&SNIFFED_DECLARATIONS)
        .any(|tag| starts(tag))
    {
        return true;
    }
    let Some(after_a) = rest.strip_prefix(b"a").or_else(|| rest.strip_prefix(b"A")) else {
        return false;
    };
    let after_space = skip_space(after_a);
    after_space.len() < after_a.len()
        && after_space
            .get(..4)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"href"))
}

/// `bytes` past any white space at its start.
fn skip_space(bytes: &[u8]) -> &[u8] {
    &bytes[bytes.iter().take_while(|b| is_space(**b)).count()..]
}
