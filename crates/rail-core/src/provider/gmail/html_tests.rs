//! Rail's HTML-to-text scanner: hostile shapes from both reviews at 256 KiB and 10 MiB, each
//! timed and size-checked, and what ordinary mail reads like.

use std::time::{Duration, Instant};

use super::html::{MAX_TAG_BYTES, html_text};

const SMALL: usize = 256 * 1024;
const LARGE: usize = 10 * 1024 * 1024;
/// Generous for a debug build on a loaded CI machine.
const SMALL_LIMIT: Duration = Duration::from_secs(1);
const LARGE_LIMIT: Duration = Duration::from_secs(5);
const TAIL: &str = "<p>zzend</p>";

/// `prefix`, then `pattern` repeated to about `size` bytes, then `TAIL`.
fn build(prefix: &str, pattern: &str, size: usize) -> String {
    let count = size.saturating_sub(prefix.len() + TAIL.len()) / pattern.len();
    let mut html = String::with_capacity(size);
    html.push_str(prefix);
    for _ in 0..count {
        html.push_str(pattern);
    }
    html.push_str(TAIL);
    html
}

/// Convert `html`, print the time and sizes, and check the bounds: in time, never longer
/// than the input (plus 4), and the text after the hostile part still there.
fn bounded(name: &str, html: &str, limit: Duration) -> String {
    let started = Instant::now();
    let text = html_text(html);
    let took = started.elapsed();
    eprintln!(
        "{name}: {} bytes in, {} bytes out, {took:?}",
        html.len(),
        text.len()
    );
    assert!(took < limit, "{name} took {took:?}");
    assert!(text.len() <= html.len() + 4, "{name}: {} bytes", text.len());
    text
}

fn both_sizes(name: &str, prefix: &str, pattern: &str, ends_visible: bool) {
    for (size, limit) in [(SMALL, SMALL_LIMIT), (LARGE, LARGE_LIMIT)] {
        let html = build(prefix, pattern, size);
        let text = bounded(&format!("{name} @{size}"), &html, limit);
        if ends_visible {
            assert!(text.ends_with("zzend"), "{name}: {:?}", tail(&text));
        }
    }
}

fn tail(text: &str) -> &str {
    let mut start = text.len().saturating_sub(60);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[test]
fn thousands_of_distinct_attributes_in_one_tag() {
    let attributes: String = (0..20_000).map(|i| format!(" a{i:05}")).collect();
    for (size, limit) in [(SMALL, SMALL_LIMIT), (LARGE, LARGE_LIMIT)] {
        let html = build("", &format!("<p{attributes}>"), size);
        let text = bounded(&format!("distinct attributes @{size}"), &html, limit);
        assert!(text.ends_with("zzend"));
    }
}

#[test]
fn stray_quotes_cannot_hide_what_follows_for_long() {
    both_sizes("stray quote", "<p a\">", "<div>", true);
    // The quotes pair across repetitions, so the last tag may run to the end of the text.
    both_sizes("unbalanced quotes", "", "<p title=\"x>word ", false);
}

#[test]
fn long_colliding_names() {
    let pattern = format!("<{}></{}>", "a".repeat(40), "b".repeat(40));
    both_sizes("colliding long names", "", &pattern, true);
}

#[test]
fn deep_nesting_and_nested_quotes() {
    both_sizes("deep divs", "", "<div>", true);
    both_sizes("deep quotes", "", "<blockquote>word ", true);
    both_sizes("span over div", "", "<span><div></span>", true);
}

#[test]
fn nested_tables_with_preformatted_newlines() {
    both_sizes("tables and pre", "", "<table><tr><td><pre>\n\n\n\n", true);
    let prefix = "<table><td><ol start=9223372036854775807><li>".repeat(24) + "<pre>";
    both_sizes("ol start=huge", &prefix, "\n", true);
}

#[test]
fn formatting_and_foreign_content() {
    let opens: String = (1..=128).map(|i| format!("<b x={i}>")).collect();
    both_sizes(
        "distinct-attribute formatting",
        "",
        &format!("<p>{opens}</p>text"),
        true,
    );
    both_sizes("unclosed b i a", "", "<b><i><a>x", true);
    both_sizes("svg", "", "<svg><g><text>t</text>", true);
}

#[test]
fn entity_floods() {
    both_sizes("amp flood", "", "&amp;", true);
    both_sizes("max code point", "", "&#x10FFFF;", true);
    both_sizes(
        "long unknown names",
        "",
        &format!("&{};", "a".repeat(40)),
        true,
    );
    both_sizes("huge numbers", "", "&#99999999999", true);
    both_sizes("bare ampersands", "", "&&&&#&#x", true);
}

#[test]
fn unterminated_markup() {
    for (size, limit) in [(SMALL, SMALL_LIMIT), (LARGE, LARGE_LIMIT)] {
        let comment = build("<!--", "text ", size);
        let text = bounded(&format!("unterminated comment @{size}"), &comment, limit);
        assert!(text.is_empty(), "a comment runs to the end");
        let tag = build("<div a=\"", "text ", size);
        let text = bounded(&format!("unterminated tag @{size}"), &tag, limit);
        assert!(
            text.ends_with("zzend"),
            "the tag ends after {MAX_TAG_BYTES} bytes"
        );
        let brackets = build("", "<", size);
        bounded(&format!("bare brackets @{size}"), &brackets, limit);
        let script = build("<script>", "x = 1; ", size);
        let text = bounded(&format!("unterminated script @{size}"), &script, limit);
        assert!(text.is_empty());
    }
}

#[test]
fn script_end_tag_tricks() {
    let html = "<script>document.write(\"</scr\" + \"ipt>\"); steal();</script>visible\
                <SCRIPT type=x>more()</SCRIPT >shown<style>p{color:red}</style x>also\
                <script>a</scriptx>b()</script>end";
    let text = bounded("script tricks", html, SMALL_LIMIT);
    assert_eq!(text, "visibleshownalsoend");
    both_sizes("script openers", "", "<script></scr", false);
}

#[test]
fn paragraphs_and_breaks_become_lines() {
    let html = "<p>One</p><p>Two<br>Three</p><br><br><br><br><div>Four</div>\
                <ul><li>a</li><li>b</li></ul><table><tr><td>c</td><td>d</td></tr></table>";
    assert_eq!(
        html_text(html),
        "One\n\nTwo\nThree\n\nFour\n\na\n\nb\n\nc d"
    );
}

#[test]
fn links_keep_their_text_and_references_decode() {
    let html = "<p>See <a href=\"https://example.com/?a=1&amp;b=2\">the  <b>report</b></a> &amp; \
                &lt;notes&gt; &eacute;t&eacute; &#233;&#x20AC; &nbsp;&hellip; &unknown; &amp</p>";
    assert_eq!(
        html_text(html),
        "See the report & <notes> été é€ \u{a0}… &unknown; &amp"
    );
}

#[test]
fn control_references_stay_as_written() {
    let html = "a&#0;b&#27;c&#x7f;d&#x85;e&#xD800;f&#x110000;g&#10;h&#9;i";
    assert_eq!(
        html_text(html),
        "a&#0;b&#27;c&#x7f;d&#x85;e&#xD800;f&#x110000;g h i"
    );
}

#[test]
fn head_title_style_and_comments_are_dropped() {
    let html = "<html><head><title>Subject</title><style>p{color:red}</style>\
                <script>var x = '</head>';</script><meta charset=utf-8></head>\
                <body><!-- hidden --><template><p>tpl</p></template><noscript>ns</noscript>\
                <p>Hello</p></body></html>";
    assert_eq!(html_text(html), "Hello");
    let no_end = "<head><title>T</title><body><p>Still shown</p>";
    assert_eq!(html_text(no_end), "Still shown");
}

#[test]
fn white_space_collapses_and_lines_are_capped() {
    let html = "  a \t\r\n b  <br>\n\n<br><br><br>  c  ";
    assert_eq!(html_text(html), "a b\n\nc");
}

#[test]
fn head_ends_where_body_content_starts() {
    let unclosed = "<html><head><meta charset=\"utf-8\"><p>Hello <b>world</b></p></html>";
    assert_eq!(html_text(unclosed), "Hello world");
    let bare_text = "<head><style>p{color:red}</style>Your invoice is attached";
    assert_eq!(html_text(bare_text), "Your invoice is attached");
    let upper = "<HEAD><META>Text<BODY>more";
    assert_eq!(html_text(upper), "Textmore");
    let kept = "<head> <!-- c --> <link rel=x><base href=y><title>T</title>\
                <script>s()</script><noscript>n</noscript><template>t</template></head>shown";
    assert_eq!(html_text(kept), "shown");
}

#[test]
fn head_floods_stay_linear() {
    both_sizes("head metas", "<head>", "<meta a=b>", true);
    both_sizes("head styles", "<head>", "<style></sty", false);
    both_sizes("head comments", "<head>", "<!-- x --> ", true);
}
