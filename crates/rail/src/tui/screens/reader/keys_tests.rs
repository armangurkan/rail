//! The reader's cursor over the title block and the cards: where it starts, `n`/`p` over the
//! message starts in both orders, `Tab` and `Enter` on a card, the subject and a preview, a
//! re-read's expansions and cursor, and a failed re-read on the subject line.

use std::rc::Rc;

use rail_core::config::ThreadOrder;

use super::test_threads::{four, message, person, reader_of};
use super::{LineKind, Reader, Tone};
use crate::tui::keys::Action;

/// The kind of the line under the cursor.
fn at(reader: &Reader) -> LineKind {
    reader.cursor_line().expect("a line under the cursor").kind
}

/// The text of message `message`'s card at 80 columns.
fn card_text(reader: &Reader, message: usize) -> String {
    reader
        .lines(80)
        .iter()
        .find(|l| l.kind == LineKind::Card { message })
        .map(|l| l.text.clone())
        .expect("the message's card")
}

fn act(reader: &mut Reader, actions: &[Action]) {
    for &action in actions {
        reader.act(action);
    }
}

#[test]
fn the_cursor_starts_on_the_subject_line() {
    let reader = reader_of(four(0), ThreadOrder::Oldest);
    assert_eq!(reader.cursor, 0);
    assert_eq!(at(&reader), LineKind::Title { message: 3 });
    assert_eq!(reader.cursor_line().unwrap().text, "Q4 roadmap review");
}

#[test]
fn n_and_p_jump_between_the_subject_and_the_cards_in_drawn_order() {
    let card = |message| LineKind::Card { message };
    for (order, cards) in [
        (ThreadOrder::Oldest, [0, 1, 2]),
        (ThreadOrder::Newest, [2, 1, 0]),
    ] {
        let mut reader = reader_of(four(0), order);
        let mut seen = Vec::new();
        for _ in 0..4 {
            reader.act(Action::ReaderNext);
            seen.push(at(&reader));
        }
        assert_eq!(
            seen,
            [
                card(cards[0]),
                card(cards[1]),
                card(cards[2]),
                card(cards[2])
            ],
            "{order:?}: n stops on the last card"
        );
        act(&mut reader, &[Action::ReaderPrev; 3]);
        assert_eq!(reader.cursor, 0, "{order:?}: back on the subject");
        reader.act(Action::ReaderPrev);
        assert_eq!(reader.cursor, 0, "p stops on the subject");
        act(&mut reader, &[Action::ReaderDown; 5]);
        assert_eq!(at(&reader), LineKind::Body { message: 3 });
        reader.act(Action::ReaderNext);
        assert_eq!(
            at(&reader),
            card(cards[0]),
            "from the newest's body to the first card"
        );
        act(&mut reader, &[Action::ReaderDown, Action::ReaderPrev]);
        assert_eq!(
            at(&reader),
            card(cards[0]),
            "from a preview back to its card"
        );
    }
}

#[test]
fn tab_opens_a_card_in_place_and_closes_it_from_any_of_its_lines() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    act(&mut reader, &[Action::ReaderNext, Action::ReaderNext]);
    let tom = reader.cursor;
    assert_eq!(at(&reader), LineKind::Card { message: 1 });
    reader.act(Action::ReaderExpand);
    assert_eq!(reader.cursor, tom, "the cursor stays on the card");
    assert!(card_text(&reader, 1).starts_with("▾   2   Tom"));
    act(&mut reader, &[Action::ReaderDown, Action::ReaderDown]);
    assert_eq!(at(&reader), LineKind::Body { message: 1 });
    reader.act(Action::ReaderExpand);
    assert_eq!(reader.cursor, tom, "back on the card");
    assert!(card_text(&reader, 1).starts_with("▸   2   Tom"));
    reader.act(Action::ReaderUp);
    assert_eq!(at(&reader), LineKind::Rule);
    let before = reader.lines(80);
    reader.act(Action::ReaderExpand);
    assert!(
        Rc::ptr_eq(&before, &reader.lines(80)),
        "Tab on a rule does nothing"
    );
    assert_eq!(at(&reader), LineKind::Rule);
}

#[test]
fn tab_on_the_newest_closes_it_to_its_preview_and_lands_on_the_subject() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    act(&mut reader, &[Action::ReaderDown; 5]);
    assert_eq!(at(&reader), LineKind::Body { message: 3 });
    reader.act(Action::ReaderExpand);
    assert_eq!(reader.cursor, 0);
    let lines = reader.lines(80);
    assert_eq!(lines[4].kind, LineKind::Preview { message: 3 });
    assert_eq!(lines[4].text, "Thursday works.");
    assert_eq!(lines[5].kind, LineKind::Rule);
    act(&mut reader, &[Action::ReaderDown; 4]);
    assert_eq!(at(&reader), LineKind::Preview { message: 3 });
    reader.act(Action::ReaderExpand);
    assert_eq!(reader.cursor, 0, "open again, the cursor on the subject");
    assert_eq!(reader.lines(80)[5].kind, LineKind::Body { message: 3 });
}

#[test]
fn enter_on_a_card_the_subject_or_a_preview_does_what_tab_does() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    reader.act(Action::ReaderNext);
    reader.act(Action::ReaderUnfold);
    assert!(
        card_text(&reader, 0).starts_with("▾   1   Priya Nair"),
        "Enter on a card opens it"
    );
    assert_eq!(at(&reader), LineKind::Card { message: 0 });
    act(&mut reader, &[Action::ReaderDown, Action::ReaderDown]);
    assert_eq!(at(&reader), LineKind::Body { message: 0 });
    let before = reader.lines(80);
    reader.act(Action::ReaderUnfold);
    assert!(
        Rc::ptr_eq(&before, &reader.lines(80)),
        "Enter on a body line does nothing"
    );
    act(&mut reader, &[Action::ReaderTop, Action::ReaderUnfold]);
    assert_eq!(
        reader.lines(80)[4].kind,
        LineKind::Preview { message: 3 },
        "Enter on the subject closes the newest"
    );
    act(&mut reader, &[Action::ReaderDown; 4]);
    reader.act(Action::ReaderUnfold);
    assert_eq!(
        reader.lines(80)[4].kind,
        LineKind::Gap,
        "Enter on its preview opens it again"
    );
    let preview = reader
        .lines(80)
        .iter()
        .position(|l| l.kind == LineKind::Preview { message: 1 })
        .unwrap();
    reader.cursor = preview;
    reader.act(Action::ReaderUnfold);
    assert_eq!(at(&reader), LineKind::Card { message: 1 });
    assert!(
        card_text(&reader, 1).starts_with("▾   2   Tom"),
        "Enter on a card's preview opens the card"
    );
}

#[test]
fn a_reread_keeps_each_cards_expansion_by_id_and_a_new_message_takes_the_default() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    act(
        &mut reader,
        &[Action::ReaderNext, Action::ReaderNext, Action::ReaderExpand],
    );
    let mut earlier = four(0);
    let ann = person(Some("Ann"), "ann@example.com");
    earlier
        .messages
        .insert(0, message(9, ann, 1_759_400_000, "Earlier."));
    reader.refresh(earlier);
    assert!(
        card_text(&reader, 0).starts_with("▸   1   Ann"),
        "new and older: collapsed"
    );
    assert!(card_text(&reader, 1).starts_with("▸   2   Priya Nair"));
    assert!(
        card_text(&reader, 2).starts_with("▾   3   Tom"),
        "Tom stays open, now the third message"
    );

    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    let mut later = four(0);
    let mut sam = message(
        5,
        person(Some("Sam"), "sam@example.com"),
        1_760_090_000,
        "Late.",
    );
    sam.is_read = false;
    later.messages.push(sam);
    reader.refresh(later);
    let lines = reader.lines(80);
    assert!(lines[1].text.starts_with("Sam  09:53"), "{}", lines[1].text);
    assert_eq!(
        lines[5].kind,
        LineKind::Body { message: 4 },
        "the new newest is open"
    );
    assert!(
        card_text(&reader, 3).starts_with("▾   4   David Hallas"),
        "the old newest keeps its open state as a card: {}",
        card_text(&reader, 3)
    );
}

/// Review Focus 5.
#[test]
fn a_reread_that_removes_the_message_under_the_cursor_keeps_the_cursor_on_a_drawn_line() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    act(&mut reader, &[Action::ReaderNext, Action::ReaderNext]);
    assert_eq!(at(&reader), LineKind::Card { message: 1 });
    let line = reader.cursor;
    let mut without_tom = four(0);
    without_tom.messages.remove(1);
    reader.refresh(without_tom);
    assert_eq!(reader.cursor, line, "the cursor keeps its line");
    assert_eq!(
        at(&reader),
        LineKind::Card { message: 1 },
        "now the next card"
    );
    assert_eq!(
        reader.message_at_cursor().map(|m| m.id),
        Some(3),
        "David's Wednesday message"
    );
    let mut only_the_newest = four(0);
    only_the_newest.messages.drain(..3);
    reader.refresh(only_the_newest);
    let last = reader.lines(80).len() - 1;
    assert_eq!(reader.cursor, line.min(last), "clamped to the last line");
    assert!(reader.cursor_line().is_some());
}

#[test]
fn a_failed_reread_shows_on_the_subject_line_until_a_read_succeeds() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    let _ = reader.lines(80);
    reader.read_failed("The thread could not be read: busy".into());
    let lines = reader.lines(80);
    assert_eq!(lines[0].text, "The thread could not be read: busy");
    assert_eq!(lines[0].segments[0].1, Tone::Error);
    reader.refresh(four(0));
    assert_eq!(
        reader.lines(80)[0].text,
        "Q4 roadmap review",
        "the same thread read again clears it"
    );
}

#[test]
fn h_shows_and_hides_every_open_messages_headers_and_keeps_the_cursor_on_its_message() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    act(
        &mut reader,
        &[
            Action::ReaderNext,
            Action::ReaderExpand,
            Action::ReaderDown,
            Action::ReaderDown,
        ],
    );
    assert_eq!(
        at(&reader),
        LineKind::Body { message: 0 },
        "in Priya's open card"
    );
    reader.act(Action::ReaderHeaders);
    assert_eq!(at(&reader), LineKind::Card { message: 0 }, "on its card");
    let lines = reader.lines(80);
    let fields: Vec<&str> = lines
        .iter()
        .filter(|l| matches!(l.kind, LineKind::Field { .. }))
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        fields,
        [
            "From: David Hallas <david@example.com>",
            "To: Vito <vito@example.com>, dana@example.com",
            "Date: Fri 10 Oct 2025 04:32",
            "From: Priya Nair <priya@example.com>",
            "Date: Fri 03 Oct 2025 09:15",
        ],
        "the newest and the open card; the collapsed cards show none"
    );
    reader.act(Action::ReaderHeaders);
    assert!(
        reader
            .lines(80)
            .iter()
            .all(|l| !matches!(l.kind, LineKind::Field { .. }))
    );
    assert_eq!(at(&reader), LineKind::Card { message: 0 });
}

/// Review Focus 4.
#[test]
fn set_order_turns_the_cards_round_and_keeps_the_cursor_on_its_message() {
    let mut reader = reader_of(four(0), ThreadOrder::Oldest);
    reader.act(Action::ReaderNext);
    let first = reader.cursor;
    assert_eq!(
        at(&reader),
        LineKind::Card { message: 0 },
        "Priya's, the first card"
    );
    reader.set_order(ThreadOrder::Newest);
    assert_eq!(
        at(&reader),
        LineKind::Card { message: 0 },
        "still on Priya's card"
    );
    assert!(reader.cursor > first, "which is the last card now");
    let cards: Vec<LineKind> = reader
        .lines(80)
        .iter()
        .map(|l| l.kind)
        .filter(|k| matches!(k, LineKind::Card { .. }))
        .collect();
    assert_eq!(
        cards,
        [
            LineKind::Card { message: 2 },
            LineKind::Card { message: 1 },
            LineKind::Card { message: 0 }
        ]
    );
    act(
        &mut reader,
        &[Action::ReaderExpand, Action::ReaderDown, Action::ReaderDown],
    );
    assert_eq!(at(&reader), LineKind::Body { message: 0 });
    reader.set_order(ThreadOrder::Oldest);
    assert_eq!(reader.cursor, first, "from inside its body: on its card");
    act(&mut reader, &[Action::ReaderTop]);
    act(&mut reader, &[Action::ReaderDown; 5]);
    assert_eq!(at(&reader), LineKind::Body { message: 3 });
    reader.set_order(ThreadOrder::Newest);
    assert_eq!(reader.cursor, 0, "from the newest's body: on the subject");
    let before = reader.lines(80);
    reader.set_order(ThreadOrder::Newest);
    assert!(
        Rc::ptr_eq(&before, &reader.lines(80)),
        "the same order rebuilds nothing"
    );
}
