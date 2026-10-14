//! The reader's lines (`cards::build`) at fixed widths on the fixed clock: the title block, the
//! newest message open or collapsed, the headers, outside text, and every width.

use std::collections::{HashMap, HashSet};

use rail_core::config::Icons;
use rail_core::store::view::ThreadDetail;

use super::ReaderAccount;
use super::cards;
use super::lines::{LineKind, ReaderLine, State, Tone};
use super::test_threads::{SUBJECT, axon, clock, four, person};
use crate::tui::screens::mail::Glyphs;
use crate::tui::text::width;

const TITLE: LineKind = LineKind::Title { message: 3 };

/// What the lines are built with besides the thread.
struct Setup {
    expanded: Vec<bool>,
    headers: bool,
    read_error: Option<String>,
    icons: Icons,
    account: ReaderAccount,
}

impl Setup {
    /// The newest message open and the others collapsed, no headers, Unicode, AXON.
    fn of(detail: &ThreadDetail) -> Self {
        let last = detail.messages.len().saturating_sub(1);
        Self {
            expanded: (0..detail.messages.len()).map(|i| i == last).collect(),
            headers: false,
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
            clock: &clock,
            glyphs: &glyphs,
            account: &self.account,
            headers: self.headers,
            read_error: self.read_error.as_deref(),
        };
        cards::build(detail, &state, cols)
    }
}

fn texts(lines: &[ReaderLine]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

fn kinds(lines: &[ReaderLine]) -> Vec<LineKind> {
    lines.iter().map(|l| l.kind).collect()
}

fn tones(line: &ReaderLine) -> Vec<Tone> {
    line.segments.iter().map(|(_, tone)| *tone).collect()
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
        lines[1].segments[4].0, " A ",
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
    assert_eq!(lines[5].text, "Thursday works.");
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
    assert_eq!(lines[2].text, "To: Vito, dana@example.com  /  1 message");
}

/// Review Focus 1.
#[test]
fn the_title_block_without_recipients_or_a_senders_name_keeps_the_count() {
    let mut detail = four(0);
    let newest = detail.messages.last_mut().unwrap();
    newest.to.clear();
    newest.from = vec![person(None, "david@example.com")];
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines[1].text, "david@example.com  04:32   A  AXON");
    assert_eq!(lines[2].text, "/  4 messages");
    detail.messages.last_mut().unwrap().from.clear();
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines[1].text, "(no sender)  04:32   A  AXON");
    let narrow = Setup::of(&detail).lines(&detail, 8);
    assert_eq!(
        narrow[2].text, "/  4 me…",
        "narrower than the count: cut like any line"
    );
}

#[test]
fn the_names_are_cut_so_the_count_stays_whole() {
    let detail = four(0);
    let lines = Setup::of(&detail).lines(&detail, 30);
    assert_eq!(lines[2].text, "To: Vito, dana…  /  4 messages");
    assert_eq!(width(&lines[2].text), 30);
    let lines = Setup::of(&detail).lines(&detail, 12);
    assert_eq!(
        lines[2].text, "To: Vito, d…",
        "narrower than the count: the line is cut"
    );
}

#[test]
fn the_count_includes_the_messages_left_out() {
    let detail = four(5);
    let lines = Setup::of(&detail).lines(&detail, 90);
    assert_eq!(lines[2].text, "To: Vito, dana@example.com  /  9 messages");
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
            .text
            .starts_with("pay\\u{202e}fdp.exe \\u{1b}[2J\\u{2028}x"),
        "{}",
        lines[4].text
    );
    assert!(lines[4].text.ends_with('…'), "{}", lines[4].text);
    assert_eq!(width(&lines[4].text), 40);
    assert_eq!(tones(&lines[4]), [Tone::Muted]);
    assert!(
        lines
            .iter()
            .all(|l| !l.text.contains('\u{1b}') && !l.text.contains('\u{202e}'))
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
    assert_eq!(lines[0].text, "The thread could not be read: busy");
    assert_eq!(tones(&lines[0]), [Tone::Error]);
    assert_eq!(
        lines[1].text, "David Hallas  04:32   A  AXON",
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
    assert_eq!(lines[0].text, "Q4 \\u{1b}[2J plan");
    assert_eq!(lines[1].text, "Evil\\u{7}Corp  04:32   A  AX\\u{7}ON");
    assert_eq!(lines[2].text, "To: Vi\\u{202e}to  /  4 messages");
}

#[test]
fn an_unknown_account_has_no_badge_ascii_rules_are_dashes_and_an_empty_thread_is_its_subject() {
    let detail = four(0);
    let mut setup = Setup::of(&detail);
    setup.account = ReaderAccount::unknown();
    setup.icons = Icons::Ascii;
    let lines = setup.lines(&detail, 90);
    assert_eq!(lines[1].text, "David Hallas  04:32");
    assert_eq!(lines[3].text, "-".repeat(90));
    let mut empty = four(0);
    empty.messages.clear();
    let lines = Setup::of(&empty).lines(&empty, 90);
    assert_eq!(kinds(&lines), [LineKind::Note]);
    assert_eq!(texts(&lines), [SUBJECT]);
    assert_eq!(tones(&lines[0]), [Tone::Accent]);
}

/// Every combination of headers, glyphs and expansion (all open or all collapsed).
fn every_setup(detail: &ThreadDetail) -> Vec<Setup> {
    let mut out = Vec::new();
    for headers in [false, true] {
        for icons in [Icons::Unicode, Icons::Ascii] {
            for open in [false, true] {
                out.push(Setup {
                    expanded: vec![open; detail.messages.len()],
                    headers,
                    icons,
                    ..Setup::of(detail)
                });
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
                    width(&line.text) <= cols,
                    "{cols} columns, headers {}, {:?}: {:?} {}",
                    setup.headers,
                    setup.icons,
                    line.kind,
                    line.text
                );
                let joined: String = line.segments.iter().map(|(t, _)| t.as_str()).collect();
                assert_eq!(line.text, joined);
            }
        }
    }
}
