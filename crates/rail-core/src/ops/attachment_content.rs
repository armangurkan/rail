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

/// How far into a text part Rail looks for markup: the most a common sniffer reads
/// (`file`'s HTML rules search the first 4096 bytes; shared-mime-info's, 256).
pub const TEXT_SNIFF_WINDOW: usize = 4096;

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

/// A ZIP whose first entry is `mimetype`, holding the declared type: how ODF lays out every
/// file (OpenDocument 1.2, part 3, §3.3), read from the local header without a ZIP reader.
fn is_odf(bytes: &[u8], mime_type: &str) -> bool {
    let field = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
    };
    let (Some(name_len), Some(extra_len)) = (field(26), field(28)) else {
        return false;
    };
    let data = 30 + name_len + extra_len;
    bytes.starts_with(ZIP_ENTRY)
        && bytes.get(30..30 + name_len) == Some(b"mimetype".as_slice())
        && bytes
            .get(data..data + mime_type.len())
            .is_some_and(|found| found == mime_type.as_bytes())
}

/// Text a sniffing opener would still call text. Any 8-bit encoding is taken — UTF-8, or the
/// Latin-1 and Windows-1252 a spreadsheet exports — because every byte string without a NUL
/// is valid Latin-1, so requiring UTF-8 would refuse real files and keep nothing out. Refused:
/// a NUL (binary, or UTF-16 text, which hides markup from the rest of the check); a start,
/// after a UTF-8 byte-order mark and white space, of `<` (any markup), `#!` (a script),
/// `%PDF` or `%!` (PostScript); and, in the first `TEXT_SNIFF_WINDOW` bytes, a tag a sniffer
/// takes for HTML, SVG or XML, case ignored.
fn is_plain_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.contains(&0) {
        return false;
    }
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let start = body.trim_ascii_start();
    let scripted = [b"<".as_slice(), b"#!", b"%PDF", b"%!"]
        .iter()
        .any(|prefix| start.starts_with(prefix));
    !scripted && !has_sniffed_tag(&body[..body.len().min(TEXT_SNIFF_WINDOW)])
}

/// Tags `file` and shared-mime-info read as HTML, SVG or XML anywhere near the start.
const SNIFFED_TAGS: [&[u8]; 14] = [
    b"<!doctype",
    b"<?xml",
    b"<!--",
    b"<html",
    b"<head",
    b"<body",
    b"<title",
    b"<script",
    b"<style",
    b"<table",
    b"<svg",
    b"<iframe",
    b"<h1",
    b"<a href",
];

/// Whether `window` holds one of `SNIFFED_TAGS`, ASCII case ignored.
fn has_sniffed_tag(window: &[u8]) -> bool {
    window.iter().enumerate().any(|(at, &byte)| {
        byte == b'<'
            && SNIFFED_TAGS.iter().any(|tag| {
                window
                    .get(at..at + tag.len())
                    .is_some_and(|found| found.eq_ignore_ascii_case(tag))
            })
    })
}
