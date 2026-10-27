//! `content_matches`: each openable type's own signature accepted, and markup, scripts and the
//! other types' bytes refused under every declared type (#107).

use super::attachment_content::*;
use super::attachments::OPENABLE;

const ODT: &str = "application/vnd.oasis.opendocument.text";
const ODS: &str = "application/vnd.oasis.opendocument.spreadsheet";
const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// A local file header whose first entry is `name`, stored, holding `data`: the start of a
/// ZIP as an ODF writer lays it out.
fn zip_entry(name: &str, data: &[u8]) -> Vec<u8> {
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(&[0; 12]);
    bytes.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(name.as_bytes());
    bytes.extend_from_slice(data);
    bytes
}

/// A BMP's 14-byte file header and the start of a 40-byte DIB header.
fn bmp() -> Vec<u8> {
    let mut bytes = b"BM".to_vec();
    bytes.extend_from_slice(&[0; 12]);
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);
    bytes
}

/// An ISO base media `ftyp` box naming `brand`.
fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 24];
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(brand);
    bytes.extend_from_slice(&[0; 12]);
    bytes
}

/// Bytes a well-formed file of `mime` starts with. Matched on the rule, so a new rule cannot be
/// left without a sample here.
fn good(mime: &str) -> Vec<u8> {
    let rule = signature(mime).unwrap_or_else(|| panic!("{mime} has no rule"));
    match rule {
        Signature::Pdf => b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec(),
        Signature::Png => b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(),
        Signature::Jpeg => b"\xff\xd8\xff\xe0\0\x10JFIF\0".to_vec(),
        Signature::Gif => b"GIF89a\x01\0\x01\0".to_vec(),
        Signature::WebP => b"RIFF\x24\0\0\0WEBPVP8 ".to_vec(),
        Signature::Bmp => bmp(),
        Signature::Tiff => b"II*\0\x08\0\0\0".to_vec(),
        Signature::Heic => ftyp(b"heic"),
        Signature::Ooxml => zip_entry("[Content_Types].xml", b"<?xml"),
        Signature::Odf => zip_entry("mimetype", mime.as_bytes()),
        Signature::Ole => b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\0\0".to_vec(),
        Signature::Rtf => b"{\\rtf1\\ansi hello}".to_vec(),
        Signature::Text => b"Name,Total\nDana,12\n".to_vec(),
    }
}

/// Bytes a sniffing opener would hand to a browser or an interpreter.
const HOSTILE: [&[u8]; 8] = [
    b"<svg onload=alert(1)>",
    b"<html><script>alert(1)</script></html>",
    b"<!DOCTYPE html><p>hi",
    b"\xef\xbb\xbf  \n<SCRIPT>alert(1)</SCRIPT>",
    b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    b"#!/bin/sh\nrm -rf ~\n",
    b"%!PS-Adobe-3.0\n",
    b"",
];

/// Every type the open list hands over has a rule, and the rule takes its own good bytes.
#[test]
fn every_openable_type_has_a_signature_rule() {
    for (mime, _, _) in OPENABLE {
        assert!(signature(mime).is_some(), "{mime}");
        assert!(content_matches(mime, &good(mime)), "{mime}");
        // Parameters and case are the declared type's, as `openable` takes them.
        let upper = format!("{}; charset=utf-8", mime.to_ascii_uppercase());
        assert!(content_matches(&upper, &good(mime)), "{upper}");
    }
}

/// Markup, scripts and an empty part are refused whatever type they were declared as.
#[test]
fn markup_and_scripts_are_refused_under_every_type() {
    for (mime, _, _) in OPENABLE {
        for bytes in HOSTILE {
            assert!(
                !content_matches(mime, bytes),
                "{mime} took {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }
}

/// A type outside the open list matches nothing, whatever its bytes.
#[test]
fn a_type_outside_the_list_matches_nothing() {
    for mime in ["text/html", "image/svg+xml", "application/zip", ""] {
        assert!(!content_matches(mime, b"%PDF-1.7"), "{mime}");
        assert!(!content_matches(mime, b"plain words"), "{mime}");
    }
}

/// Each binary type takes only its own signature: another type's good bytes are refused.
#[test]
fn a_binary_type_refuses_another_type_s_bytes() {
    let binary = OPENABLE
        .iter()
        .filter(|(_, _, rule)| *rule != Signature::Text);
    for (mime, _, rule) in binary {
        for (other, _, other_rule) in OPENABLE {
            // Two ZIP-based Office formats can share a container; ODF's own rule is below.
            let zips = [Signature::Ooxml, Signature::Odf];
            let both_zips = zips.contains(rule) && zips.contains(&other_rule);
            if other_rule == *rule || other_rule == Signature::Text || both_zips {
                continue;
            }
            assert!(
                !content_matches(mime, &good(other)),
                "{mime} took {other}'s bytes"
            );
        }
    }
}

/// The variants each rule allows, and the near misses it does not.
#[test]
fn signature_variants_and_near_misses() {
    assert!(content_matches("image/gif", b"GIF87a\x01\0"));
    assert!(!content_matches("image/gif", b"GIF88a\x01\0"));
    assert!(content_matches("image/tiff", b"MM\0*\0\0\0\x08"));
    assert!(!content_matches("image/tiff", b"II\0*"));
    assert!(!content_matches("image/webp", b"RIFF\x24\0\0\0WAVEfmt "));
    for brand in [b"heix", b"mif1", b"msf1", b"heis", b"hevc"] {
        assert!(content_matches("image/heic", &ftyp(brand)), "{brand:?}");
    }
    assert!(!content_matches("image/heic", &ftyp(b"isom")));
    assert!(!content_matches("image/bmp", b"BM<svg onload=alert(1)>"));
    assert!(!content_matches("image/png", b"\x89PNG"));
    assert!(!content_matches("application/pdf", b" %PDF-1.7"));
}

/// An ODF file's first entry is its `mimetype`, naming the declared type; an OOXML file is a
/// ZIP of any first entry.
#[test]
fn an_odf_file_names_its_own_type_first() {
    assert!(content_matches(ODT, &zip_entry("mimetype", ODT.as_bytes())));
    assert!(!content_matches(
        ODT,
        &zip_entry("mimetype", ODS.as_bytes())
    ));
    assert!(!content_matches(
        ODT,
        &zip_entry("content.xml", ODT.as_bytes())
    ));
    assert!(!content_matches(ODT, b"PK\x03\x04"));
    assert!(content_matches(DOCX, &zip_entry("word/document.xml", b"x")));
    assert!(!content_matches(DOCX, b"PK\x05\x06"));
}

/// Plain text: any 8-bit text without NUL, with markup only past the window a sniffer reads.
#[test]
fn text_takes_words_and_refuses_what_a_sniffer_would_reroute() {
    for mime in ["text/plain", "text/csv", "text/tab-separated-values"] {
        assert!(content_matches(mime, b"a\tb\n1\t2\n"), "{mime}");
        assert!(content_matches(mime, "caf\u{e9} \u{2014} ok".as_bytes()));
        // Windows-1252 from a spreadsheet export: not UTF-8, still text.
        assert!(content_matches(mime, b"Caf\xe9;12\r\n"), "{mime}");
        assert!(content_matches(mime, b"\xef\xbb\xbfName,Total\n"), "{mime}");
        assert!(content_matches(mime, b"Reply to Dana <dana@example.com>"));
        assert!(content_matches(mime, b"if a < b then c > d"), "{mime}");
        assert!(!content_matches(mime, b"line\0with a NUL"), "{mime}");
        assert!(!content_matches(mime, b" %PDF-1.4 as text"), "{mime}");
        assert!(!content_matches(mime, b"\xff\xfeh\0i\0"), "{mime}: UTF-16");
        assert!(!content_matches(mime, b"notes first\n<html><body>"));
        assert!(!content_matches(mime, b"see below\n\n<Script src=x>"));
        let mut late = vec![b'x'; TEXT_SNIFF_WINDOW];
        late.extend_from_slice(b"<html>");
        assert!(content_matches(mime, &late), "{mime}: past the window");
    }
}

/// `filler` bytes of plain text, then `tail`.
fn after(filler: usize, tail: &[u8]) -> Vec<u8> {
    let mut bytes = vec![b'x'; filler];
    bytes.extend_from_slice(tail);
    bytes
}

/// A tag that starts inside the window is found even when it runs past it: `file` needs only
/// the tag's start inside its 4096 bytes, and 4093 bytes of filler before `<html>` was taken.
#[test]
fn a_tag_straddling_a_window_edge_is_still_found() {
    for filler in 4085..=4100 {
        assert!(
            !content_matches("text/plain", &after(filler, b"<html>x")),
            "<html> at {filler}"
        );
    }
    for start in [TEXT_SNIFF_WINDOW - 5, TEXT_SNIFF_WINDOW - 1] {
        assert!(
            !content_matches("text/plain", &after(start, b"<script>")),
            "<script> at {start}, running past the window"
        );
    }
    assert!(content_matches(
        "text/plain",
        &after(TEXT_SNIFF_WINDOW, b"<script>")
    ));
}

/// Whitespace inside a tag is whatever `file`'s compact-whitespace match takes: a tab, several
/// spaces or a line break between `<a` and `href`, space after `<`, any case.
#[test]
fn a_tag_with_odd_whitespace_or_case_is_still_found() {
    let words = b"Notes for Friday.\n";
    for tag in [
        b"<a\thref=x>".as_slice(),
        b"<a  href=x>",
        b"<a\nhref=x>",
        b"<a\r\n\thref=x>",
        b"<A HREF=x>",
        b"< html>",
        b"<HTML\n>",
        b"<title/>",
        b"<Script\tsrc=x>",
        b"<!DocType html>",
        b"<svg\nonload=x>",
        b"<header>",
    ] {
        let mut bytes = words.to_vec();
        bytes.extend_from_slice(tag);
        assert!(
            !content_matches("text/plain", &bytes),
            "{:?}",
            String::from_utf8_lossy(tag)
        );
    }
    for fine in [
        b"<a>".as_slice(),
        b"<abc@example.com>",
        b"< 3 hrefs",
        b"a <b> c",
    ] {
        let mut bytes = words.to_vec();
        bytes.extend_from_slice(fine);
        assert!(
            content_matches("text/plain", &bytes),
            "{:?}",
            String::from_utf8_lossy(fine)
        );
    }
}

/// The start-of-text check skips every byte-order mark and every ASCII white space, vertical
/// tab and form feed included.
#[test]
fn repeated_marks_and_odd_white_space_do_not_hide_a_start() {
    for start in [
        b"\x0b<p>".as_slice(),
        b"\x0c\x0b #!/bin/sh",
        b"\xef\xbb\xbf\xef\xbb\xbf<p>",
        b"\xef\xbb\xbf \xef\xbb\xbf\t%PDF-1.4",
        b"\xef\xbb\xbf\x0b\xef\xbb\xbf%!PS",
    ] {
        assert!(
            !content_matches("text/plain", start),
            "{:?}",
            String::from_utf8_lossy(start)
        );
    }
}

/// An OOXML file starts with one of the parts Office writers put first; a JAR (or any other
/// ZIP) renamed `.docx` does not.
#[test]
fn an_ooxml_file_starts_with_an_office_part() {
    for first in [
        "[Content_Types].xml",
        "_rels/.rels",
        "docProps/app.xml",
        "word/document.xml",
        "xl/workbook.xml",
        "ppt/presentation.xml",
    ] {
        assert!(content_matches(DOCX, &zip_entry(first, b"<")), "{first}");
    }
    for first in [
        "META-INF/MANIFEST.MF",
        "META-INF/",
        "com/example/Main.class",
        "mimetype",
        "",
    ] {
        assert!(!content_matches(DOCX, &zip_entry(first, b"x")), "{first}");
    }
}
