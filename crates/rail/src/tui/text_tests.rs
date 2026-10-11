//! Escaping, width, cutting, wrapping, tabs, quote folding, link markers, link validation and
//! OSC 8, with the hostile inputs the spec names.

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
fn no_wrapped_row_is_wider_than_the_pane() {
    let line = escape(&"Grüße 日本語 😀 \u{1b}[31m a-very-long-word-without-breaks ".repeat(50));
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
