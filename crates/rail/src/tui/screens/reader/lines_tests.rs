//! The reader's lines (`cards::build`) at fixed widths on the fixed clock: the title block, the
//! newest message open or collapsed, the headers, outside text, and every width.

use std::collections::{HashMap, HashSet};

use rail_core::config::{Icons, ThreadOrder};
use rail_core::store::view::{AttachmentView, ThreadDetail};

use super::cards;
use super::lines::{LineKind, ReaderLine, State, Tone};
use super::test_threads::{SUBJECT, axon, clock, four, person};
use super::{ReaderAccount, unread_ids};
use crate::tui::screens::mail::Glyphs;
use crate::tui::text::width;

const TITLE: LineKind = LineKind::Title { message: 3 };

/// What the lines are built with besides the thread.
struct Setup {
    expanded: Vec<bool>,
    /// The ids unread at open.
    unread: HashSet<i64>,
    headers: bool,
    order: ThreadOrder,
    read_error: Option<String>,
    icons: Icons,
    account: ReaderAccount,
}

impl Setup {
    /// The newest message open and the others collapsed, the fixture's unread messages unread
    /// at open, no headers, Unicode, AXON.
    fn of(detail: &ThreadDetail) -> Self {
        let last = detail.messages.len().saturating_sub(1);
        Self {
            expanded: (0..detail.messages.len()).map(|i| i == last).collect(),
            unread: unread_ids(detail),
            headers: false,
            order: ThreadOrder::Oldest,
            read_error: None,
            icons: Icons::Unicode,
            account: axon(),
        }
    }

    fn lines(&self, detail: &ThreadDetail, cols: usize) -> Vec<ReaderLine> {
        let (unfolded, fetching, errors) = (HashSet::new(), HashSet::new(), HashMap::new());
        let (clock, glyphs) = (clock(), Glyphs::of(self.icons));
        let state = State {
            expanded: &self.expanded,
            unfolded: &unfolded,
            fetching: &fetching,
            errors: &errors,
            unread: &self.unread,
            clock: &clock,
            glyphs: &glyphs,
            account: &self.account,
            headers: self.headers,
            order: self.order,
            read_error: self.read_error.as_deref(),
        };
        cards::build(detail, &state, cols)
    }
}

fn texts(lines: &[ReaderLine]) -> Vec<&str> {
    lines.iter().map(|l| l.text()).collect()
}

fn kinds(lines: &[ReaderLine]) -> Vec<LineKind> {
    lines.iter().map(|l| l.kind).collect()
}

fn tones(line: &ReaderLine) -> Vec<Tone> {
    line.segments().iter().map(|(_, tone)| *tone).collect()
}

#[test]
fn the_title_block_reads_subject_sender_time_account_recipients_and_count() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 90);
    let rule = "─".repeat(90);
    assert_eq!(kinds(&lines[..4]), [TITLE, TITLE, TITLE, LineKind::Rule]);
    assert_eq!(
        texts(&lines[..4]),
        [
            SUBJECT,
            "David Hallas  04:32   A  AXON",
            "To: Vito, dana@example.com  /  4 messages",
            rule.as_str(),
        ]
    );
    assert_eq!(tones(&lines[0]), [Tone::Accent]);
    assert_eq!(
        tones(&lines[1]),
        [
            Tone::Strong,
            Tone::Plain,
            Tone::Muted,
            Tone::Plain,
            Tone::Badge,
            Tone::Plain,
            Tone::Muted
        ]
    );
    assert_eq!(
        lines[1].segments()[4].0,
        " A ",
        "the badge letter, a space each side"
    );
    assert_eq!(tones(&lines[2]), [Tone::Muted]);
    assert_eq!(tones(&lines[3]), [Tone::Muted]);
}

#[test]
fn the_newest_message_is_open_under_the_title_block_and_closed_by_a_rule() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(
        kinds(&lines[4..8]),
        [
            LineKind::Gap,
            LineKind::Body { message: 3 },
            LineKind::Gap,
            LineKind::Rule
        ]
    );
    assert_eq!(lines[5].text(), "Thursday works.");
}

#[test]
fn a_thread_of_one_message_is_its_title_block_its_body_and_a_rule() {
    let mut detail = four(0);
    detail.messages.drain(..3);
    let lines = Setup::of(&detail).lines(&detail, 90);
    let title = LineKind::Title { message: 0 };
    assert_eq!(
        kinds(&lines),
        [
            title,
            title,
            title,
            LineKind::Rule,
            LineKind::Gap,
            LineKind::Body { message: 0 },
            LineKind::Gap,
            LineKind::Rule
        ]
    );
    assert_eq!(lines[2].text(), "To: Vito, dana@example.com  /  1 message");
}

/// Review Focus 1.
#[test]
fn the_title_block_without_recipients_or_a_senders_name_keeps_the_count() {
    let mut detail = four(0);
    let newest = detail.messages.last_mut().unwrap();
    newest.to.clear();
    newest.from = vec![person(None, "david@example.com")];
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines[1].text(), "david@example.com  04:32   A  AXON");
    assert_eq!(lines[2].text(), "/  4 messages");
    detail.messages.last_mut().unwrap().from.clear();
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines[1].text(), "(no sender)  04:32   A  AXON");
    let narrow = Setup::of(&detail).lines(&detail, 8);
    assert_eq!(
        narrow[2].text(),
        "/  4 me…",
        "narrower than the count: cut like any line"
    );
}

#[test]
fn the_names_are_cut_so_the_count_stays_whole() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 30);
    assert_eq!(lines[2].text(), "To: Vito, dana…  /  4 messages");
    assert_eq!(width(lines[2].text()), 30);
    let lines = Setup::of(&detail).lines(&detail, 12);
    assert_eq!(
        lines[2].text(),
        "To: Vito, d…",
        "narrower than the count: the line is cut"
    );
}

/// Spec §6 (D2): a row full exactly at a segment's end, with more to come, shows the cut on the
/// last kept segment.
#[test]
fn a_row_full_at_a_segments_end_shows_the_cut_on_that_segment() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 19);
    assert_eq!(
        lines[1].text(),
        "David Hallas  04:3…",
        "the badge and the label follow the time"
    );
    assert_eq!(width(lines[1].text()), 19);
}

/// Spec §6 (D2): under four cells of names the title drops them, never `…  /  4 messages`.
#[test]
fn a_narrow_title_drops_the_names_rather_than_show_an_ellipsis_alone() {
    let detail = four(0);
    for cols in 15..=18 {
        let lines = Setup::of(&detail).lines(&detail, cols);
        assert_eq!(lines[2].text(), "/  4 messages", "{cols} columns");
    }
    let lines = Setup::of(&detail).lines(&detail, 19);
    assert_eq!(
        lines[2].text(),
        "To:…  /  4 messages",
        "four cells of names"
    );
}

#[test]
fn the_count_includes_the_messages_left_out() {
    let detail = four(5);
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines[2].text(), "To: Vito, dana@example.com  /  9 messages");
}

/// Review Focus 2 (the newest message).
#[test]
fn a_collapsed_newest_shows_its_snippet_escaped_and_cut() {
    let mut detail = four(0);
    detail.messages[3].snippet = format!("pay\u{202e}fdp.exe \u{1b}[2J\u{2028}{}", "x".repeat(200));
    let mut setup = Setup::of(&detail);
    setup.expanded[3] = false;
    let lines = setup.lines(&detail, 40);
    assert_eq!(
        kinds(&lines[4..6]),
        [LineKind::Preview { message: 3 }, LineKind::Rule]
    );
    assert!(
        lines[4]
            .text()
            .starts_with("pay\\u{202e}fdp.exe \\u{1b}[2J\\u{2028}x"),
        "{}",
        lines[4].text()
    );
    assert!(lines[4].text().ends_with('…'), "{}", lines[4].text());
    assert_eq!(width(lines[4].text()), 40);
    assert_eq!(tones(&lines[4]), [Tone::Muted]);
    assert!(
        lines
            .iter()
            .all(|l| !l.text().contains('\u{1b}') && !l.text().contains('\u{202e}'))
    );
}

#[test]
fn headers_show_between_the_newest_messages_rule_and_its_body() {
    let mut detail = four(0);
    detail.messages[3].subject = "Re: Q4 roadmap review".into();
    detail.messages[3].cc = vec![person(Some("Tom"), "tom@example.com")];
    let mut setup = Setup::of(&detail);
    let off = setup.lines(&detail, 90);
    assert!(
        off.iter()
            .all(|l| !matches!(l.kind, LineKind::Field { .. })),
        "off by default"
    );
    setup.headers = true;
    let on = setup.lines(&detail, 90);
    let field = LineKind::Field { message: 3 };
    assert_eq!(
        kinds(&on[3..11]),
        [
            LineKind::Rule,
            field,
            field,
            field,
            field,
            field,
            LineKind::Gap,
            LineKind::Body { message: 3 }
        ]
    );
    assert_eq!(
        texts(&on[4..9]),
        [
            "From: David Hallas <david@example.com>",
            "To: Vito <vito@example.com>, dana@example.com",
            "Cc: Tom <tom@example.com>",
            "Date: Fri 10 Oct 2025 04:32",
            "Subject: Re: Q4 roadmap review",
        ]
    );
    assert!(on[4..9].iter().all(|l| tones(l) == [Tone::Muted]));
}

#[test]
fn a_failed_reread_takes_the_subject_line_in_the_error_tone() {
    let detail = four(0);
    let mut setup = Setup::of(&detail);
    setup.read_error = Some("The thread could not be read: busy".into());
    let lines = setup.lines(&detail, 90);
    assert_eq!(lines[0].kind, TITLE);
    assert_eq!(lines[0].text(), "The thread could not be read: busy");
    assert_eq!(tones(&lines[0]), [Tone::Error]);
    assert_eq!(
        lines[1].text(),
        "David Hallas  04:32   A  AXON",
        "the rest of the block stays"
    );
}

#[test]
fn outside_text_in_the_title_block_is_escaped() {
    let mut detail = four(0);
    detail.subject = "Q4 \u{1b}[2J plan".into();
    detail.messages[3].from = vec![person(Some("Evil\u{7}Corp"), "e@example.com")];
    detail.messages[3].to = vec![person(Some("Vi\u{202e}to"), "v@example.com")];
    let mut setup = Setup::of(&detail);
    setup.account.label = "ax\u{7}on".into();
    let lines = setup.lines(&detail, 90);
    assert_eq!(lines[0].text(), "Q4 \\u{1b}[2J plan");
    assert_eq!(lines[1].text(), "Evil\\u{7}Corp  04:32   A  AX\\u{7}ON");
    assert_eq!(lines[2].text(), "To: Vi\\u{202e}to  /  4 messages");
}

#[test]
fn an_unknown_account_has_no_badge_ascii_rules_are_dashes_and_an_empty_thread_is_its_subject() {
    let detail = four(0);
    let mut setup = Setup::of(&detail);
    setup.account = ReaderAccount::unknown();
    setup.icons = Icons::Ascii;
    let lines = setup.lines(&detail, 90);
    assert_eq!(lines[1].text(), "David Hallas  04:32");
    assert_eq!(lines[3].text(), "-".repeat(90));
    let mut empty = four(0);
    empty.messages.clear();
    let lines = Setup::of(&empty).lines(&empty, 90);
    assert_eq!(kinds(&lines), [LineKind::Note]);
    assert_eq!(texts(&lines), [SUBJECT]);
    assert_eq!(tones(&lines[0]), [Tone::Accent]);
}

/// Every combination of order, headers, glyphs and expansion (all open or all collapsed).
fn every_setup(detail: &ThreadDetail) -> Vec<Setup> {
    let mut out = Vec::new();
    for order in [ThreadOrder::Oldest, ThreadOrder::Newest] {
        for headers in [false, true] {
            for icons in [Icons::Unicode, Icons::Ascii] {
                for open in [false, true] {
                    out.push(Setup {
                        expanded: vec![open; detail.messages.len()],
                        headers,
                        order,
                        icons,
                        ..Setup::of(detail)
                    });
                }
            }
        }
    }
    out
}

/// Review Focus 3 (every width).
#[test]
fn no_line_is_wider_than_the_width_it_was_built_for() {
    let detail = four(5);
    for setup in every_setup(&detail) {
        for cols in 1..=100 {
            for line in setup.lines(&detail, cols) {
                assert!(
                    width(line.text()) <= cols,
                    "{cols} columns, {:?}, headers {}, {:?}: {:?} {}",
                    setup.order,
                    setup.headers,
                    setup.icons,
                    line.kind,
                    line.text()
                );
                let joined: String = line.segments().iter().map(|(t, _)| t.as_str()).collect();
                assert_eq!(line.text(), joined);
            }
        }
    }
}

/// A card line: `head`, then `time` right-aligned at `cols`.
fn card_text(head: &str, time: &str, cols: usize) -> String {
    format!(
        "{head}{}{time}",
        " ".repeat(cols - width(head) - width(time))
    )
}

fn card_of(lines: &[ReaderLine], message: usize) -> &ReaderLine {
    lines
        .iter()
        .find(|l| l.kind == LineKind::Card { message })
        .expect("the message's card")
}

#[test]
fn the_four_message_thread_oldest_first_reads_as_the_concept() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines.len(), 17);
    let card = |message| LineKind::Card { message };
    let preview = |message| LineKind::Preview { message };
    assert_eq!(
        kinds(&lines[8..]),
        [
            card(0),
            preview(0),
            LineKind::Rule,
            card(1),
            preview(1),
            LineKind::Rule,
            card(2),
            preview(2),
            LineKind::Rule
        ]
    );
    let rule = "─".repeat(90);
    let expected = [
        card_text("▸   1   Priya Nair", "Oct 03", 90),
        "        Agenda attached.".to_string(),
        rule.clone(),
        card_text("▸   2   Tom", "Mon 22:11", 90),
        "        Sounds good.".to_string(),
        rule.clone(),
        card_text("▸   3   David Hallas ★", "Wed 18:20", 90),
        "        Moving it to Thursday?".to_string(),
        rule,
    ];
    assert_eq!(
        texts(&lines[8..]),
        expected.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(
        tones(&lines[8]),
        [
            Tone::Muted,
            Tone::Plain,
            Tone::Number,
            Tone::Plain,
            Tone::Plain,
            Tone::Plain,
            Tone::Muted
        ],
        "a read message's sender is plain"
    );
    assert_eq!(lines[8].segments()[2].0, " 1 ", "the number in its box");
    assert_eq!(
        tones(&lines[14])[4],
        Tone::Strong,
        "an unread one's is bold"
    );
    assert_eq!(tones(&lines[9]), [Tone::Muted]);
    assert_eq!(width(lines[8].text()), 90, "the time ends at the edge");
}

#[test]
fn newest_first_runs_the_cards_the_other_way_under_the_same_open_message() {
    let detail = four(0);
    let oldest = Setup::of(&detail).lines(&detail, 90);
    let mut setup = Setup::of(&detail);
    setup.order = ThreadOrder::Newest;
    let lines = setup.lines(&detail, 90);
    assert_eq!(
        texts(&lines[..8]),
        texts(&oldest[..8]),
        "the title block and the newest"
    );
    assert_eq!(
        kinds(&lines[8..11]),
        [
            LineKind::Card { message: 2 },
            LineKind::Preview { message: 2 },
            LineKind::Rule
        ]
    );
    let cards: Vec<&str> = lines
        .iter()
        .filter(|l| matches!(l.kind, LineKind::Card { .. }))
        .map(|l| l.text())
        .collect();
    assert_eq!(
        cards,
        [
            card_text("▸   3   David Hallas ★", "Wed 18:20", 90),
            card_text("▸   2   Tom", "Mon 22:11", 90),
            card_text("▸   1   Priya Nair", "Oct 03", 90),
        ]
    );
    assert_eq!(lines.last().unwrap().kind, LineKind::Rule);
}

#[test]
fn the_left_out_note_sits_where_those_messages_would_be_and_the_numbers_count_them() {
    let detail = four(5);
    let note = "[5 older messages of this thread are not shown]";
    let oldest = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(
        kinds(&oldest[7..11]),
        [
            LineKind::Rule,
            LineKind::Note,
            LineKind::Rule,
            LineKind::Card { message: 0 }
        ]
    );
    assert_eq!(oldest[8].text(), note);
    assert_eq!(tones(&oldest[8]), [Tone::Muted]);
    assert!(
        oldest[10].text().starts_with("▸   6   Priya Nair"),
        "{}",
        oldest[10].text()
    );
    let mut setup = Setup::of(&detail);
    setup.order = ThreadOrder::Newest;
    let newest = setup.lines(&detail, 90);
    let n = newest.len();
    assert_eq!(kinds(&newest[n - 2..]), [LineKind::Rule, LineKind::Note]);
    assert_eq!(newest[n - 1].text(), note);
    assert!(
        newest[8].text().starts_with("▸   8   David Hallas ★"),
        "{}",
        newest[8].text()
    );
}

#[test]
fn a_two_digit_number_moves_the_sender_and_its_preview_one_cell_right() {
    let detail = four(9);
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert!(card_of(&lines, 0).text().starts_with("▸   10   Priya Nair"));
    let preview = lines
        .iter()
        .find(|l| l.kind == LineKind::Preview { message: 0 })
        .unwrap();
    assert_eq!(preview.text(), "         Agenda attached.");
}

#[test]
fn an_open_card_shows_its_text_and_with_h_its_headers_before_a_rule() {
    let detail = four(0);
    let mut setup = Setup::of(&detail);
    setup.expanded[1] = true;
    let lines = setup.lines(&detail, 90);
    let at = lines
        .iter()
        .position(|l| l.kind == LineKind::Card { message: 1 })
        .unwrap();
    assert!(
        lines[at].text().starts_with("▾   2   Tom"),
        "{}",
        lines[at].text()
    );
    assert_eq!(
        kinds(&lines[at + 1..at + 5]),
        [
            LineKind::Gap,
            LineKind::Body { message: 1 },
            LineKind::Gap,
            LineKind::Rule
        ]
    );
    assert_eq!(lines[at + 2].text(), "Sounds good.");
    setup.headers = true;
    let lines = setup.lines(&detail, 90);
    let at = lines
        .iter()
        .position(|l| l.kind == LineKind::Card { message: 1 })
        .unwrap();
    assert_eq!(
        texts(&lines[at + 1..at + 3]),
        ["From: Tom <tom@example.com>", "Date: Mon 06 Oct 2025 22:11"]
    );
    assert_eq!(
        kinds(&lines[at + 3..at + 7]),
        [
            LineKind::Gap,
            LineKind::Body { message: 1 },
            LineKind::Gap,
            LineKind::Rule
        ]
    );
}

#[test]
fn a_long_sender_is_cut_so_the_time_stays_whole_at_the_edge() {
    let mut detail = four(0);
    detail.messages[1].from = vec![person(
        Some("Bartholomew Alexander Fitzgerald-Smythe"),
        "b@example.com",
    )];
    let lines = Setup::of(&detail).lines(&detail, 40);
    let card = card_of(&lines, 1);
    assert_eq!(card.text(), "▸   2   Bartholomew Alexande…  Mon 22:11");
    assert_eq!(width(card.text()), 40);
    detail.messages[1].is_starred = true;
    let lines = Setup::of(&detail).lines(&detail, 40);
    let card = card_of(&lines, 1);
    assert_eq!(
        card.text(),
        "▸   2   Bartholomew Alexan… ★  Mon 22:11",
        "the name is cut, the star kept whole"
    );
    assert_eq!(width(card.text()), 40);
}

/// Review Focus 3.
#[test]
fn a_card_at_15_columns_keeps_three_cells_of_sender_and_cuts_the_time() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 15);
    assert_eq!(card_of(&lines, 0).text(), "▸   1   Pr… Oc…");
    assert_eq!(card_of(&lines, 1).text(), "▸   2   Tom Mo…");
    assert!(lines.iter().all(|l| width(l.text()) <= 15));
    let preview = lines
        .iter()
        .find(|l| l.kind == LineKind::Preview { message: 0 })
        .unwrap();
    assert_eq!(preview.text(), "        Agenda…");
    let wider = Setup::of(&detail).lines(&detail, 22);
    assert_eq!(
        card_of(&wider, 1).text(),
        "▸   2   Tom  Mon 22:11",
        "at 22 columns the time is whole again"
    );
}

/// Review Focus 2 (a card).
#[test]
fn a_cards_preview_is_escaped_and_cut_under_its_sender() {
    let mut detail = four(0);
    detail.messages[1].snippet = format!("Sounds \u{1b}[2J good\u{2028}{}", "y".repeat(300));
    let lines = Setup::of(&detail).lines(&detail, 60);
    let preview = lines
        .iter()
        .find(|l| l.kind == LineKind::Preview { message: 1 })
        .unwrap();
    assert!(
        preview
            .text()
            .starts_with("        Sounds \\u{1b}[2J good\\u{2028}y"),
        "{}",
        preview.text()
    );
    assert!(preview.text().ends_with('…'), "{}", preview.text());
    assert_eq!(width(preview.text()), 60);
}

/// The reader has no `YESTERDAY` heading, so yesterday takes the week form; today keeps the
/// bare time.
#[test]
fn a_message_from_yesterday_shows_its_weekday_and_one_from_today_only_its_time() {
    // Thu 09 Oct 2025 08:55 UTC, the day before the fixed clock's NOW.
    let yesterday = 1_760_000_100;
    let mut detail = four(0);
    detail.messages[1].internal_date = yesterday * 1000;
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(
        card_of(&lines, 1).text(),
        card_text("▸   2   Tom", "Thu 08:55", 90)
    );
    assert_eq!(lines[1].text(), "David Hallas  04:32   A  AXON", "today");
    detail.messages[3].internal_date = yesterday * 1000;
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines[1].text(), "David Hallas  Thu 08:55   A  AXON");
}

/// Spec §7 (D4): cards with `icons = "ascii"`.
#[test]
fn ascii_cards_use_the_ascii_arrows_and_star() {
    let detail = four(0);
    let mut setup = Setup::of(&detail);
    setup.icons = Icons::Ascii;
    setup.expanded[1] = true;
    let lines = setup.lines(&detail, 90);
    assert_eq!(
        card_of(&lines, 0).text(),
        card_text(">   1   Priya Nair", "Oct 03", 90)
    );
    assert_eq!(
        card_of(&lines, 1).text(),
        card_text("v   2   Tom", "Mon 22:11", 90)
    );
    assert_eq!(
        card_of(&lines, 2).text(),
        card_text(">   3   David Hallas +", "Wed 18:20", 90)
    );
}

/// Spec §7 (D4): newest first, an open card's text sits between its neighbours.
#[test]
fn newest_first_an_open_card_shows_its_text_between_its_neighbours() {
    let detail = four(0);
    let mut setup = Setup::of(&detail);
    setup.order = ThreadOrder::Newest;
    setup.expanded[1] = true;
    let lines = setup.lines(&detail, 90);
    assert_eq!(
        kinds(&lines[8..]),
        [
            LineKind::Card { message: 2 },
            LineKind::Preview { message: 2 },
            LineKind::Rule,
            LineKind::Card { message: 1 },
            LineKind::Gap,
            LineKind::Body { message: 1 },
            LineKind::Gap,
            LineKind::Rule,
            LineKind::Card { message: 0 },
            LineKind::Preview { message: 0 },
            LineKind::Rule
        ]
    );
    assert!(
        lines[11].text().starts_with("▾   2   Tom"),
        "{}",
        lines[11].text()
    );
    assert_eq!(lines[13].text(), "Sounds good.");
}

/// Spec §7 (D4): an open card's attachments line follows its text.
#[test]
fn an_open_cards_attachments_follow_its_text() {
    let mut detail = four(0);
    detail.messages[1].attachments = vec![AttachmentView {
        filename: "plan.pdf".into(),
        mime_type: "application/pdf".into(),
        size: 2_048,
    }];
    for (icons, expected) in [
        (Icons::Unicode, "📎 plan.pdf"),
        (Icons::Ascii, "@ plan.pdf"),
    ] {
        let mut setup = Setup::of(&detail);
        setup.icons = icons;
        setup.expanded[1] = true;
        let lines = setup.lines(&detail, 90);
        let at = lines
            .iter()
            .position(|l| l.kind == LineKind::Card { message: 1 })
            .unwrap();
        assert_eq!(
            kinds(&lines[at + 1..at + 6]),
            [
                LineKind::Gap,
                LineKind::Body { message: 1 },
                LineKind::Field { message: 1 },
                LineKind::Gap,
                LineKind::Rule
            ]
        );
        assert_eq!(lines[at + 3].text(), expected);
        assert_eq!(tones(&lines[at + 3]), [Tone::Muted]);
    }
}

/// Spec §7 (D4): the star takes the sender's tone.
#[test]
fn the_star_takes_the_senders_tone() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(
        card_of(&lines, 2).segments()[4],
        ("David Hallas ★".to_string(), Tone::Strong),
        "unread at open: bold, the star too"
    );
    let mut setup = Setup::of(&detail);
    setup.unread.clear();
    let lines = setup.lines(&detail, 90);
    assert_eq!(card_of(&lines, 2).segments()[4].1, Tone::Plain);
}
