//! Link numbering, caps and schemes, and bare addresses in plain text.

use super::links::{MAX_LINK_BYTES, MAX_LINKS, Numbering, allowed, plain_links};

#[test]
fn only_web_and_mail_addresses_without_spaces_or_controls_are_kept() {
    for ok in [
        "https://example.com/a?b=c#d",
        "HTTP://EXAMPLE.COM",
        "mailto:dana@example.com",
        "https://ex\u{e4}mple.com/",
    ] {
        assert!(allowed(ok), "{ok:?}");
    }
    for refused in [
        "",
        "javascript:alert(1)",
        "JavaScript:alert(1)",
        "data:text/html,x",
        "file:///etc/passwd",
        "ftp://example.com",
        "/relative",
        "#top",
        "https://exa mple.com",
        "https://example.com/\u{1b}]8;;x",
        "https://example.com/\u{7}",
        "https://example.com/\u{7f}",
    ] {
        assert!(!allowed(refused), "{refused:?}");
    }
    assert!(
        allowed("https:"),
        "a bare scheme is kept here; the reader's URL parse refuses it"
    );
    let longest = format!("https://e.com/{}", "a".repeat(MAX_LINK_BYTES - 14));
    assert_eq!(longest.len(), MAX_LINK_BYTES);
    assert!(allowed(&longest));
    assert!(!allowed(&format!("{longest}a")));
}

#[test]
fn numbers_follow_first_appearance_and_repeats_share_one() {
    let mut numbering = Numbering::default();
    assert_eq!(numbering.number("https://a.example"), Some(1));
    assert_eq!(numbering.number("https://b.example"), Some(2));
    assert_eq!(numbering.number("https://a.example"), Some(1));
    assert_eq!(numbering.number("javascript:x"), None);
    assert_eq!(numbering.number("https://c.example"), Some(3));
    assert_eq!(
        numbering.into_links(),
        [
            "https://a.example",
            "https://b.example",
            "https://c.example"
        ]
    );
}

#[test]
fn at_most_max_links_are_numbered() {
    let mut numbering = Numbering::default();
    for n in 1..=MAX_LINKS {
        assert_eq!(numbering.number(&format!("https://e.com/{n}")), Some(n));
    }
    assert_eq!(numbering.number("https://e.com/one-too-many"), None);
    assert_eq!(
        numbering.number("https://e.com/1"),
        Some(1),
        "known ones keep theirs"
    );
    assert_eq!(numbering.into_links().len(), MAX_LINKS);
}

#[test]
fn bare_addresses_in_plain_text_get_markers() {
    let rendered = plain_links(
        "See https://example.com/a, then http://x.example/wiki/Rust_(language) or\n\
         <https://w.example/path>. Again: https://example.com/a. Not xhttps://no.example \
         nor ftp://f.example nor https:// alone.",
    );
    assert_eq!(
        rendered.text,
        "See https://example.com/a [1], then http://x.example/wiki/Rust_(language) [2] or\n\
         <https://w.example/path [3]>. Again: https://example.com/a [1]. Not xhttps://no.example \
         nor ftp://f.example nor https:// alone."
    );
    assert_eq!(
        rendered.links,
        [
            "https://example.com/a",
            "http://x.example/wiki/Rust_(language)",
            "https://w.example/path"
        ]
    );
}

#[test]
fn plain_text_without_addresses_is_unchanged_and_long_ones_get_no_marker() {
    let text = "Grüße, 日本 😀 (no links here) http:/broken";
    assert_eq!(plain_links(text).text, text);
    let long = format!("https://e.com/{}", "a".repeat(MAX_LINK_BYTES));
    let rendered = plain_links(&format!("x {long} y"));
    assert_eq!(rendered.text, format!("x {long} y"));
    assert!(rendered.links.is_empty());
}

#[test]
fn a_flood_of_addresses_stays_linear() {
    let text = "https://e.com/x ".repeat(700_000);
    let started = std::time::Instant::now();
    let rendered = plain_links(&text);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(rendered.links.len(), 1);
    assert!(
        rendered
            .text
            .starts_with("https://e.com/x [1] https://e.com/x [1] ")
    );
}
