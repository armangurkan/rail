//! Two edges of the reader's lines: a mailbox named by its own address, and a zero-width
//! segment at the edge of a cut row.

use super::*;

/// The shared `display_name` prints the bare address, so a mailbox whose name is its address
/// is not written twice in the `From:`, `To:` and `Cc:` lines.
#[test]
fn a_mailbox_named_by_its_address_is_not_repeated_in_the_header_fields() {
    let mut detail = four(0);
    detail.messages[3].from = vec![person(Some("a@b.example"), "a@b.example")];
    detail.messages[3].cc = vec![person(Some("Tom"), "tom@example.com")];
    let mut setup = Setup::of(&detail);
    setup.headers = true;
    let lines = setup.lines(&detail, 90);
    let shown = texts(&lines);
    assert!(shown.contains(&"From: a@b.example"), "{shown:?}");
    assert!(shown.contains(&"Cc: Tom <tom@example.com>"), "{shown:?}");
}

/// A zero-width segment that is the last one kept as the row fills must not take the `…` that
/// belongs to the last visible segment when more follows.
#[test]
fn a_zero_width_segment_at_the_edge_does_not_swallow_the_cut() {
    let segments = vec![
        ("abcd".to_string(), Tone::Plain),
        (String::new(), Tone::Plain),
        ("efgh".to_string(), Tone::Plain),
    ];
    let cut: Vec<String> = cards::cut(segments, 4)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    assert_eq!(cut, ["abc…"]);
}
