//! Escaping, width, cutting, wrapping, tabs, quote folding, link markers, link validation and
//! OSC 8, with the hostile inputs the spec names.

use rail_core::provider::links::MAX_LINK_BYTES;
use url::Url;

use super::text::*;

#[test]
fn controls_format_characters_and_line_separators_are_escaped() {
    assert_eq!(escape("a\u{1b}[2Jb"), "a\\u{1b}[2Jb");
    assert_eq!(escape("pay\u{202E}fdp.exe"), "pay\\u{202e}fdp.exe");
    assert_eq!(
        escape("one\u{2028}two\u{2029}three"),
        "one\\u{2028}two\\u{2029}three"
    );
    assert_eq!(escape("nul\0here"), "nul\\0here");
    assert_eq!(escape("bell\u{7}\r\n\t"), "bell\\u{7}\\r\\n\\t");
    assert_eq!(escape("c1\u{9b}31m"), "c1\\u{9b}31m");
    assert_eq!(
        escape("zero\u{200B}width\u{FEFF}"),
        "zero\\u{200b}width\\u{feff}"
    );
    assert_eq!(escape("Grüße, 日本 😀"), "Grüße, 日本 😀");
}

#[test]
fn width_is_measured_after_escaping() {
    assert_eq!(width(&escape("a\u{1b}b")), 8);
    assert_eq!(width("日本"), 4);
    assert_eq!(width("e\u{301}"), 1);
    assert!(!fit("x\u{1b}y", 80).contains('\u{1b}'));
}

#[test]
fn fit_and_pad_count_columns_not_characters() {
    assert_eq!(fit("abc", 3), "abc");
    assert_eq!(fit("abcd", 3), "ab…");
    assert_eq!(fit("日本語テキスト", 5), "日本…");
    assert_eq!(fit("日本語", 4), "日…");
    assert_eq!(fit("anything", 0), "");
    assert_eq!(fit("\u{1b}\u{1b}", 8), "\\u{1b}\\…");
    assert_eq!(pad("日本", 5), "日本 ");
    assert_eq!(width(&pad("日本語テキスト", 7)), 7);
    assert_eq!(pad_left("9", 3), "  9");
    assert_eq!(pad("", 2), "  ");
}

#[test]
fn wrapping_breaks_at_spaces_splits_long_words_and_keeps_indentation() {
    assert_eq!(wrap("aaaa bbbb cccc", 9), ["aaaa bbbb", "cccc"]);
    assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    assert_eq!(wrap("日本語テキスト", 5), ["日本", "語テ", "キス", "ト"]);
    assert_eq!(wrap("  indented line", 40), ["  indented line"]);
    assert_eq!(wrap("a  b", 40), ["a  b"]);
    assert_eq!(wrap("", 10), [""]);
    assert_eq!(wrap("abc def", 0), ["a", "b", "c", "d", "e", "f"]);
    assert_eq!(
        wrap("aaaa  bbbb", 4),
        ["aaaa", "bbbb"],
        "no empty row at a double space"
    );
}

#[test]
fn wrapping_never_emits_blank_rows_and_keeps_leading_and_trailing_spaces() {
    assert_eq!(wrap("aaaa   bbbb", 4), ["aaaa", "bbbb"]);
    assert_eq!(wrap(" abcdefgh", 4), [" abc", "defg", "h"]);
    assert_eq!(wrap("  abcdefgh", 4), ["  ab", "cdef", "gh"]);
    assert_eq!(wrap("ab  ", 2), ["ab"]);
    assert_eq!(wrap("ab  ", 40), ["ab  "]);
    assert_eq!(wrap("   ", 40), ["   "]);
    assert_eq!(wrap("    x", 2), [" x"]);
    assert_eq!(wrap("日", 1), ["日"]);
    assert_eq!(wrap("日 ", 1), ["日"]);
    assert_eq!(wrap(" 日", 2), ["日"]);
    assert_eq!(wrap("ab", 0), ["a", "b"]);
}

#[test]
fn width_sums_graphemes_as_ratatui_draws_them() {
    assert_eq!(width("\u{644}\u{627}"), 2);
    assert_eq!(width("e\u{301}日"), 3);
}

#[test]
fn no_wrapped_row_is_wider_than_the_pane() {
    let line = escape(&"Grüße 日本語 😀 \u{1b}[31m a-very-long-word-without-breaks \u{645}\u{644}\u{627}\u{644}\u{627} ".repeat(50));
    for cols in [1, 2, 3, 7, 20, 79, 80, 150] {
        for row in wrap(&line, cols) {
            assert!(width(&row) <= cols.max(2), "{cols}: {row:?}");
        }
    }
}

#[test]
fn an_over_long_line_wraps_quickly() {
    let line = "a".repeat(1_000_000);
    let started = std::time::Instant::now();
    let rows = wrap(&line, 80);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "generous for a debug build"
    );
    assert_eq!(rows.len(), 12_500);
    assert!(rows.iter().all(|r| r.len() == 80));
}

#[test]
fn tabs_expand_to_eight_column_stops() {
    assert_eq!(expand_tabs("a\tb"), "a       b");
    assert_eq!(expand_tabs("\t"), "        ");
    assert_eq!(expand_tabs("日本\tx"), "日本    x");
    assert_eq!(expand_tabs("no tabs"), "no tabs");
}

#[test]
fn four_or_more_quoted_lines_fold() {
    let body = "Hi Dana,\n\n> one\n> two\n>> three\n  > four\nThanks\n> short\n> run\n";
    let folded = blocks(body);
    assert_eq!(
        folded,
        [
            Block::Line("Hi Dana,"),
            Block::Line(""),
            Block::Quote(vec!["> one", "> two", ">> three", "  > four"]),
            Block::Line("Thanks"),
            Block::Line("> short"),
            Block::Line("> run"),
        ]
    );
    assert_eq!(fold_label(4), "[4 quoted lines]");
    assert_eq!(fold_label(1), "[1 quoted line]");
    assert_eq!(
        blocks("> a\r\n> b\r\n> c\r\n> d"),
        [Block::Quote(vec!["> a", "> b", "> c", "> d"])]
    );
}

#[test]
fn markers_are_numbers_within_the_links() {
    let row = "see[1] and [2], not [3] [0] [01] [a] [] [12345]";
    let found = markers(row, 2);
    assert_eq!(
        found,
        [
            Marker {
                col: 3,
                width: 3,
                number: 1
            },
            Marker {
                col: 11,
                width: 3,
                number: 2
            },
        ]
    );
    let wide = markers("日本[1]", 1);
    assert_eq!(
        wide,
        [Marker {
            col: 4,
            width: 3,
            number: 1
        }]
    );
    assert!(markers("[1]", 0).is_empty());
}

#[test]
fn only_web_and_mail_links_rail_can_print_are_safe() {
    let ok = safe_link("https://example.com/a?b=c").unwrap();
    assert_eq!(ok.as_str(), "https://example.com/a?b=c");
    assert_eq!(
        safe_link("https://exämple.com/ü").unwrap().as_str(),
        "https://xn--exmple-cua.com/%C3%BC"
    );
    assert!(safe_link("mailto:dana@example.com").is_some());
    for refused in [
        "javascript:alert(1)",
        "data:text/html,<script>",
        "file:///etc/passwd",
        "https:",
        "https://example.com/\u{1b}]8;;https://evil\u{7}",
        "https://example.com/ space",
        "not a url",
        "",
        "https://example.com/a\u{9b}31m",
        "https://example.com/a\u{202e}fdp",
        "https://example.com/a\u{200b}b",
        "https://exa\u{202e}mple.com/",
        "https://example.com/a\u{2028}b",
        "https://example.com/a\u{1b}b",
        "https://example.com/a\u{7}b",
        "mailto:a@b.c\u{9b}",
    ] {
        assert!(safe_link(refused).is_none(), "{refused:?}");
    }
    let long = format!("https://e.com/{}", "a".repeat(2048));
    assert!(safe_link(&long).is_none());
}

#[test]
fn osc8_wraps_the_text_in_the_hyperlink_sequence() {
    let url = safe_link("https://example.com/x").unwrap();
    assert_eq!(
        osc8(&url, "[1]"),
        "\u{1b}]8;;https://example.com/x\u{1b}\\[1]\u{1b}]8;;\u{1b}\\"
    );
}

#[test]
fn hyperlink_support_comes_from_the_environment() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        }
    };
    assert!(hyperlinks_supported(env(&[("TERM_PROGRAM", "iTerm.app")])));
    assert!(hyperlinks_supported(env(&[("TERM", "xterm-kitty")])));
    assert!(hyperlinks_supported(env(&[
        ("VTE_VERSION", "7200"),
        ("TERM", "xterm-256color")
    ])));
    assert!(hyperlinks_supported(env(&[("WT_SESSION", "x")])));
    assert!(!hyperlinks_supported(env(&[("VTE_VERSION", "4200")])));
    assert!(!hyperlinks_supported(env(&[("TERM", "xterm-256color")])));
    assert!(!hyperlinks_supported(env(&[
        ("TERM_PROGRAM", "iTerm.app"),
        ("TMUX", "/tmp/t")
    ])));
    assert!(!hyperlinks_supported(env(&[
        ("TERM", "screen-256color"),
        ("KITTY_WINDOW_ID", "1")
    ])));
    assert!(!hyperlinks_supported(env(&[("TERM", "dumb")])));
    assert!(!hyperlinks_supported(env(&[("TERM_PROGRAM", "")])));
}

#[test]
fn outside_text_is_labelled_and_escaped() {
    assert_eq!(
        outside("bad\u{1b}[2J \u{202e}x"),
        "from outside Rail (the provider or the system): \"bad\\u{1b}[2J \\u{202e}x\""
    );
}

#[test]
fn a_link_is_measured_after_it_is_normalised() {
    // 1,414 bytes as written, 4,214 once each `ü` is percent-encoded.
    let grows = format!("https://e.com/{}", "ü".repeat(700));
    assert!(grows.len() <= MAX_LINK_BYTES);
    assert!(safe_link(&grows).is_none());
    // 1,814 bytes as written, 1,814 serialised: still fine.
    let fits = format!("https://e.com/{}", "a".repeat(1_800));
    assert!(safe_link(&fits).is_some());
}

#[test]
fn the_confirmation_names_the_host_warns_about_userinfo_and_names_a_mail_recipient() {
    let lines = |address: &str| link_lines(&safe_link(address).unwrap());
    assert_eq!(
        lines("https://docs.example/plan"),
        ["Host: docs.example", "https://docs.example/plan"]
    );
    let warning =
        "Warning: the part before \"@\" is not where this link goes. It goes to evil.example.";
    assert_eq!(
        lines("https://bank.com@evil.example/x"),
        [
            warning,
            "Host: evil.example",
            "https://bank.com@evil.example/x"
        ]
    );
    assert_eq!(lines("https://user:pw@evil.example/")[0], warning);
    assert_eq!(lines("https://:pw@evil.example/")[0], warning);
    assert_eq!(
        lines("mailto:dana@example.com"),
        ["To: dana@example.com", "mailto:dana@example.com"]
    );
    let url = Url::parse("https://docs.example/").unwrap();
    assert!(link_lines(&url).iter().all(|l| !l.contains("Warning")));
}

/// Review Focus 5.
#[test]
fn an_idn_or_lookalike_host_is_shown_in_ascii_and_a_long_punycode_host_is_refused() {
    for (written, host) in [
        ("https://bücher.example/", "xn--bcher-kva.example"),
        ("https://xn--bcher-kva.example/", "xn--bcher-kva.example"),
        ("https://\u{430}pple.com/", "xn--pple-43d.com"),
    ] {
        let url = safe_link(written).unwrap();
        assert_eq!(link_lines(&url)[0], format!("Host: {host}"), "{written}");
        assert!(link_lines(&url).iter().all(|l| l.is_ascii()), "{written}");
    }
    // 1,516 bytes as written, 2,716 once every label is punycode (`xn--80ac`).
    let long = format!("https://{}example/", "\u{430}\u{431}.".repeat(300));
    assert!(long.len() <= MAX_LINK_BYTES);
    assert!(safe_link(&long).is_none());
}

fn mail(address: &str) -> Vec<String> {
    link_lines(&safe_link(address).unwrap())
}

#[test]
fn a_mailto_link_lists_every_recipient_and_warns_when_the_query_adds_some() {
    let warning = "Warning: this link adds recipients beyond the address in its path.";
    assert_eq!(
        mail(
            "mailto:boss@company.com?bcc=evil@x.example&CC=a@b.example,c@d.example&to=e@f.example"
        ),
        [
            warning,
            "To: boss@company.com",
            "To: e@f.example",
            "Cc: a@b.example",
            "Cc: c@d.example",
            "Bcc: evil@x.example",
            "mailto:boss@company.com?bcc=evil@x.example&CC=a@b.example,c@d.example&to=e@f.example",
        ]
    );
    // A subject is not a recipient.
    assert!(
        mail("mailto:a@b.example?subject=hi")
            .iter()
            .all(|l| !l.contains("Warning"))
    );
}

#[test]
fn a_mailto_path_is_decoded_and_an_empty_one_says_so() {
    assert_eq!(
        mail("mailto:ceo%40bank.com@evil.example")[0],
        "To: ceo@bank.com@evil.example"
    );
    assert_eq!(
        mail("mailto:a@b.example,c@d.example")[..2],
        ["To: a@b.example", "To: c@d.example"]
    );
    assert_eq!(mail("mailto:?subject=hi")[0], "To: (no recipient)");
    assert_eq!(
        mail("mailto:?bcc=x@y.example")[1..3],
        ["To: (no recipient)", "Bcc: x@y.example"]
    );
}

#[test]
fn a_decoded_recipient_is_escaped_and_bad_utf8_never_panics() {
    let lines = mail("mailto:a%1B%5B2J%E2%80%AEb@x.example,%FF@y.example");
    assert_eq!(lines[0], "To: a\\u{1b}[2J\\u{202e}b@x.example");
    assert_eq!(lines[1], "To: \u{fffd}@y.example");
    let lines = mail("mailto:x@y.example?bcc=%0Aevil@z.example%E2%80%AE");
    assert!(lines.iter().all(|l| !l.contains(['\n', '\u{202e}'])));
}

#[test]
fn a_long_address_is_cut_after_the_lines_that_say_where_it_goes() {
    let url = safe_link(&format!(
        "https://bank.com@evil.example/{}",
        "a".repeat(1_300)
    ))
    .unwrap();
    let lines = link_lines(&url);
    assert_eq!(lines.len(), 3);
    assert!(lines[2].ends_with('…') && width(&lines[2]) <= 150);
}

#[test]
fn percent_decode_leaves_a_plus_or_short_escape_literal() {
    assert_eq!(super::text::percent_decode("a%+4b%4"), "a%+4b%4");
    assert_eq!(super::text::percent_decode("a%41%2c"), "aA,");
}

#[test]
fn a_long_host_is_shown_by_its_end_and_the_warning_names_only_the_last_two_labels() {
    let label = "a".repeat(63);
    let host = format!("bank.com.secure.{label}.{label}.{label}.evil.example");
    let lines = link_lines(&safe_link(&format!("https://bank.com@{host}/")).unwrap());
    assert!(lines[0].starts_with("Warning:") && lines[0].ends_with("It goes to evil.example."));
    assert!(lines[1].starts_with("Host: …") && lines[1].ends_with("evil.example"));
    assert!(width(&lines[1]) <= 106);
    let ip = link_lines(&safe_link("https://u@192.0.2.7/").unwrap());
    assert!(ip[0].ends_with("It goes to 192.0.2.7."));
}

#[test]
fn recipients_are_capped_with_a_count_and_the_warning_comes_first() {
    let bcc: String = (0..190).map(|_| "&bcc=u@x.y").collect();
    let lines = mail(&format!("mailto:a@b.example?subject=s{bcc}"));
    assert!(lines[0].starts_with("Warning: this link adds recipients"));
    assert_eq!(lines.len(), 1 + 5 + 1 + 1);
    assert_eq!(lines[6], "… and 186 more recipients");
    let long = format!("mailto:a@b.example?bcc={}", "z".repeat(1_500));
    assert!(mail(&long).iter().all(|l| width(l) <= 150));
}
