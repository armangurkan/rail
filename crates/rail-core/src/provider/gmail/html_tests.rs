//! HTML to text in bounded time: hostile shapes near `HTML_RENDER_CAP`, and the stripper.

use std::time::{Duration, Instant};

use super::html::{
    HTML_RENDER_CAP, MAX_HTML_DEPTH, MAX_INDENT_DEPTH, MAX_START_TAGS, html_text, renderable,
};

/// Generous for a debug build on a loaded CI machine; the quadratic shapes took seconds to
/// hours before the bound.
const LIMIT: Duration = Duration::from_secs(1);
const TAIL: &str = "<p>zzend</p>";

/// `pattern` repeated to just under `HTML_RENDER_CAP` (with `TAIL`), so the size alone never
/// sends it to the stripper.
fn fill(pattern: &str) -> String {
    fill_after("", pattern)
}

fn fill_after(prefix: &str, pattern: &str) -> String {
    let count = (HTML_RENDER_CAP - TAIL.len() - prefix.len()) / pattern.len();
    let mut html = prefix.to_string();
    html.push_str(&pattern.repeat(count));
    html.push_str(TAIL);
    assert!(html.len() <= HTML_RENDER_CAP);
    html
}

/// Render `html`, report the time taken, and check it finished in time with its last words.
fn bounded(name: &str, html: &str) -> String {
    let started = Instant::now();
    let text = html_text(html);
    let took = started.elapsed();
    eprintln!("{name}: {} bytes in {took:?}", html.len());
    assert!(took < LIMIT, "{name} took {took:?}");
    let letters: String = text.chars().filter(char::is_ascii_lowercase).collect();
    assert!(
        letters.ends_with("zzend"),
        "{name}: {:?}",
        &letters[letters.len().saturating_sub(40)..]
    );
    text
}

#[test]
fn deep_nesting_is_stripped_in_time() {
    let html = fill("<div>");
    assert!(!renderable(&html));
    bounded("deep divs", &html);
}

#[test]
fn a_stray_quote_cannot_hide_the_tags_after_it() {
    let html = fill_after("<p a\">", "<div>");
    assert!(!renderable(&html));
    bounded("stray quote", &html);
}

#[test]
fn long_names_that_share_a_prefix_do_not_close_each_other() {
    let pattern = format!("<{}></{}>", "a".repeat(40), "b".repeat(40));
    let html = fill(&pattern);
    assert!(!renderable(&html));
    bounded("colliding long names", &html);
}

#[test]
fn formatting_elements_the_parser_reopens_are_counted() {
    let opens: String = (1..=128).map(|i| format!("<b x={i}>")).collect();
    let html = fill(&format!("<p>{opens}</p>text"));
    assert!(
        !renderable(&html),
        "html5ever reopens these without start tags"
    );
    bounded("distinct-attribute formatting", &html);
}

#[test]
fn thousands_of_unclosed_formatting_elements_are_stripped() {
    let html = fill("<b><i><a>x");
    assert!(!renderable(&html));
    bounded("unclosed b i a", &html);
}

#[test]
fn nested_tables_are_stripped() {
    let html = fill("<table><tr><td>cell");
    assert!(!renderable(&html));
    bounded("nested tables", &html);
}

#[test]
fn foreign_content_is_stripped() {
    let svg = fill("<svg><g><text>t</text>");
    assert!(!renderable(&svg));
    bounded("svg", &svg);
    let math = fill("<math><mi>x</mi><mrow>");
    assert!(!renderable(&math));
    bounded("math", &math);
}

#[test]
fn end_tags_that_close_nothing_do_not_lower_the_count() {
    for (name, pattern) in [
        ("span over div", "<span><div></span>"),
        ("div past a cell", "<div><table><td></div>"),
        ("li past a list", "<li><ul></li>"),
        ("body end", "<div></body>"),
        ("form end", "<form><div></form>"),
        ("comment", "<div><!--</div>-->"),
        ("raw text", "<div><script></div></script>"),
        ("quoted attribute", "<div title='</div>'>"),
    ] {
        let html = pattern.repeat(MAX_HTML_DEPTH + 1);
        assert!(!renderable(&html), "{name}");
        bounded(name, &fill(pattern));
    }
}

#[test]
fn ordinary_mail_is_rendered_by_html2text() {
    let html = "<html><head><title>T</title><style>p{color:red}</style></head><body>\
                <table><tr><td><p>Hello <b>Dana</b></p><ul><li>one</li><li>two</li></ul>\
                </td></tr></table><p>zzend</p></body></html>";
    assert!(renderable(html));
    let text = bounded("ordinary", html);
    assert!(
        text.contains("Hello Dana") && !text.contains("color"),
        "{text:?}"
    );
    let long = fill("<p>Some words of an <a href=\"https://example.com\">ordinary</a> letter.</p>");
    assert!(renderable(&long));
    bounded("ordinary long", &long);
}

#[test]
fn start_tags_past_the_budget_are_stripped() {
    let html = "<br>".repeat(MAX_START_TAGS + 1);
    assert!(!renderable(&html));
}

#[test]
fn deep_quotes_are_stripped_before_their_indent_multiplies_the_text() {
    assert!(renderable(&"<blockquote>".repeat(MAX_INDENT_DEPTH)));
    let html = fill_after(&"<blockquote>".repeat(MAX_INDENT_DEPTH + 1), "word ");
    assert!(!renderable(&html));
    let text = bounded("deep quotes", &html);
    assert!(text.len() <= html.len(), "{}", text.len());
}

#[test]
fn a_large_body_takes_the_stripper_and_finishes_quickly() {
    let pattern = "<div class=\"row\"><p>Line of a big newsletter &amp; more</p></div>\n";
    let mut html = pattern.repeat(5 * 1024 * 1024 / pattern.len());
    html.push_str(TAIL);
    assert!(html.len() > HTML_RENDER_CAP);
    let started = Instant::now();
    let text = html_text(&html);
    let took = started.elapsed();
    eprintln!("5 MiB stripper: {took:?}");
    assert!(took < Duration::from_secs(20), "{took:?}");
    assert!(
        text.starts_with("Line of a big newsletter & more\n"),
        "{:?}",
        &text[..80]
    );
    assert!(text.ends_with("zzend"));
}

#[test]
fn the_stripper_drops_hidden_content_and_keeps_lines() {
    let html = format!(
        "<html><head><title>Subject</title><style>p{{x:y}}</style><script>steal()</script>\
         </head><body><!-- a comment --><template><p>hidden</p></template>\
         <h1>Title</h1><p>One &lt;two&gt; &eacute;</p><br><br><br><br>\
         <div>Three<br>Four</div><table><tr><td>a</td><td>b</td></tr></table>{}</body></html>",
        " ".repeat(HTML_RENDER_CAP)
    );
    let text = html_text(&html);
    assert_eq!(text, "Title\nOne <two> é\n\nThree\nFour\n\na b");
}
