//! The composer: the compose file, reply and forward prefill, the prompts and the review.

use rail_core::provider::Mailbox;
use rail_core::store::drafts::{ReplyAttachment, ReplySource};

use super::file::{ParseError, parse, write};
use super::model::Headers;
use super::prefill::{forward, fwd_subject, quoted, re_subject, reply, with_signature};

fn headers() -> Headers {
    Headers {
        to: vec!["Sam <sam@example.com>".into(), "x@example.com".into()],
        cc: vec![],
        bcc: vec!["b@example.com".into()],
        subject: "Hello".into(),
    }
}

#[test]
fn the_compose_file_round_trips_and_keeps_the_body_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "d7", &headers(), "Hi,\n\n> quoted\n\n-- \nme\n").unwrap();
    assert_eq!(path.file_name().unwrap(), "d7.eml");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with(
            "To: Sam <sam@example.com>, x@example.com\nCc: \nBcc: b@example.com\nSubject: Hello\n\nHi,\n"
        ),
        "{text}"
    );
    let (parsed, body) = parse(&text).unwrap();
    assert_eq!(parsed, headers());
    assert_eq!(body, "Hi,\n\n> quoted\n\n-- \nme\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(dir.path()), 0o700);
    }
}

#[test]
fn parsing_accepts_case_and_continuations_and_names_a_bad_line() {
    let (h, body) = parse("to: a@b.c,\n  c@d.e\nSUBJECT: x\nCc:\nBcc:\n\nbody").unwrap();
    assert_eq!(h.to, ["a@b.c", "c@d.e"]);
    assert_eq!(h.subject, "x");
    assert_eq!(body, "body");
    let error = parse("To: a@b.c\nX-Mailer: nope\n\nbody").unwrap_err();
    assert_eq!(
        error,
        ParseError {
            line: 2,
            message: "unknown header \"X-Mailer\"".into()
        }
    );
    let error = parse("To a@b.c\n\nbody").unwrap_err();
    assert_eq!(error.line, 1);
    let (h, body) = parse("To: a@b.c").unwrap();
    assert_eq!(h.to, ["a@b.c"]);
    assert_eq!(body, "", "no blank line: an empty body");
    let (_, body) = parse("To: a@b.c\n\n").unwrap();
    assert_eq!(body, "");
    let (h, body) = parse("To: a@b.c\r\nSubject: crlf\r\n\r\nline\r\n").unwrap();
    assert_eq!((h.subject.as_str(), body.as_str()), ("crlf", "line\r\n"));
    assert_eq!(parse("  stray\n\nbody").unwrap_err().line, 1);
}

fn source() -> ReplySource {
    ReplySource {
        message: 3,
        account_id: 1,
        thread_id: 5,
        remote_thread_id: "t1".into(),
        remote_id: "m1".into(),
        message_id: Some("<m1@example.com>".into()),
        references: vec![],
        subject: "Re: Lunch?".into(),
        from: vec![Mailbox {
            name: Some("Dana Reyes".into()),
            address: "dana@example.com".into(),
        }],
        reply_to: vec![],
        to: vec![
            Mailbox {
                name: None,
                address: "me@example.com".into(),
            },
            Mailbox {
                name: None,
                address: "sam@example.com".into(),
            },
        ],
        cc: vec![Mailbox {
            name: Some("Lee".into()),
            address: "lee@example.com".into(),
        }],
        date: 1_760_000_000,
        body_text: Some("Shall we?\n> earlier\nYes.".into()),
        attachments: vec![ReplyAttachment {
            part_id: "p1".into(),
            filename: "menu.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 10,
        }],
    }
}

#[test]
fn a_reply_quotes_and_addresses_the_sender_and_reply_all_adds_everyone_but_me() {
    let tz = jiff::tz::TimeZone::UTC;
    let (h, body) = reply(&source(), false, "me@example.com", &tz);
    assert_eq!(h.to, ["Dana Reyes <dana@example.com>"]);
    assert!(h.cc.is_empty());
    assert_eq!(h.subject, "Re: Lunch?", "one Re:");
    assert_eq!(
        body,
        "\n\nOn Thu, 9 Oct 2025 08:53:20 +0000, Dana Reyes wrote:\n> Shall we?\n>> earlier\n> Yes.\n"
    );
    let (h, _) = reply(&source(), true, "ME@example.com", &tz);
    assert_eq!(h.to, ["Dana Reyes <dana@example.com>", "sam@example.com"]);
    assert_eq!(h.cc, ["Lee <lee@example.com>"]);
    let mut with_reply_to = source();
    with_reply_to.reply_to = vec![Mailbox {
        name: None,
        address: "list@example.com".into(),
    }];
    let (h, _) = reply(&with_reply_to, false, "me@example.com", &tz);
    assert_eq!(h.to, ["list@example.com"]);
    assert_eq!(re_subject("lunch"), "Re: lunch");
    assert_eq!(re_subject("RE: lunch"), "RE: lunch");
    assert_eq!(fwd_subject("Fwd: x"), "Fwd: x");
    assert_eq!(fwd_subject("x"), "Fwd: x");
}

#[test]
fn a_forward_has_the_block_and_no_recipients_and_quoting_handles_empty_bodies() {
    let (h, body) = forward(&source());
    assert!(h.to.is_empty());
    assert_eq!(h.subject, "Fwd: Re: Lunch?");
    assert!(
        body.starts_with(
            "\n\n---------- Forwarded message ---------\nFrom: Dana Reyes <dana@example.com>\nDate: "
        ),
        "{body}"
    );
    assert!(
        body.contains(
            "\nSubject: Re: Lunch?\nTo: me@example.com, sam@example.com\nCc: Lee <lee@example.com>\n\nShall we?\n> earlier\nYes.\n"
        ),
        "{body}"
    );
    assert_eq!(quoted(""), "");
    assert_eq!(quoted("a\n\nb"), "> a\n>\n> b\n");
    assert_eq!(
        with_signature("body\n", Some("Dana\nTeam")),
        "body\n\n-- \nDana\nTeam\n"
    );
    assert_eq!(with_signature("body\n", None), "body\n");
    let mut none = source();
    none.body_text = None;
    let (_, body) = reply(&none, false, "me@example.com", &jiff::tz::TimeZone::UTC);
    assert!(body.ends_with("wrote:\n"), "nothing to quote: {body}");
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rail_core::store::addresses::AddressEntry;

use super::prompt::{HeaderPrompt, PathPrompt, PromptOutcome};

fn k(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn entries() -> Vec<AddressEntry> {
    vec![
        AddressEntry {
            name: Some("Dana Reyes".into()),
            address: "dana@example.com".into(),
        },
        AddressEntry {
            name: None,
            address: "dan@work.example".into(),
        },
    ]
}

#[test]
fn the_to_prompt_asks_for_suggestions_cycles_them_and_accepts_one_per_comma() {
    let mut p = HeaderPrompt::new("To", "", true);
    assert_eq!(
        p.key(k(KeyCode::Char('d'))),
        PromptOutcome::NeedSuggestions("d".into())
    );
    p.suggestions(entries());
    assert_eq!(p.highlighted(), None, "none until one is picked");
    assert_eq!(p.key(k(KeyCode::Tab)), PromptOutcome::Stay);
    assert_eq!(
        p.highlighted(),
        Some("Dana Reyes <dana@example.com>".to_string())
    );
    assert_eq!(p.key(k(KeyCode::Tab)), PromptOutcome::Stay);
    assert_eq!(p.highlighted(), Some("dan@work.example".to_string()));
    assert_eq!(
        p.key(k(KeyCode::Enter)),
        PromptOutcome::Done("dan@work.example".into())
    );
    // A comma takes the highlighted match and starts the next recipient.
    let mut p = HeaderPrompt::new("To", "", true);
    p.key(k(KeyCode::Char('d')));
    p.suggestions(entries());
    p.key(k(KeyCode::Down));
    assert_eq!(p.key(k(KeyCode::Char(','))), PromptOutcome::Stay);
    assert_eq!(p.value(), "Dana Reyes <dana@example.com>, ");
    assert_eq!(
        p.key(k(KeyCode::Char('s'))),
        PromptOutcome::NeedSuggestions("s".into()),
        "the prefix is the current item only"
    );
    assert_eq!(p.key(k(KeyCode::Esc)), PromptOutcome::Cancel);
    // Enter with no highlight takes the typed text as is.
    let mut p = HeaderPrompt::new("Subject", "old", false);
    assert_eq!(p.key(k(KeyCode::Char('!'))), PromptOutcome::Stay);
    assert_eq!(p.key(k(KeyCode::Enter)), PromptOutcome::Done("old!".into()));
}

/// Final fix 6: suggestions arrive with nothing highlighted, so a fully typed address that
/// is the start of a known one is taken as typed, and a comma is a comma.
#[test]
fn a_typed_address_is_kept_on_enter_until_a_suggestion_is_chosen() {
    let known = || {
        vec![AddressEntry {
            name: None,
            address: "pat@acme.com".into(),
        }]
    };
    let mut p = HeaderPrompt::new("To", "pat@acme.c", true);
    p.key(k(KeyCode::Char('o')));
    p.suggestions(known());
    assert_eq!(p.highlighted(), None, "nothing highlighted on arrival");
    assert_eq!(
        p.key(k(KeyCode::Enter)),
        PromptOutcome::Done("pat@acme.co".into())
    );
    let mut p = HeaderPrompt::new("To", "pat@acme.c", true);
    p.key(k(KeyCode::Char('o')));
    p.suggestions(known());
    p.key(k(KeyCode::Char(',')));
    assert_eq!(
        p.value(),
        "pat@acme.co,",
        "a comma with no highlight is typed"
    );
}

#[test]
fn the_path_prompt_completes_from_the_filesystem_and_expands_tilde() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("report.pdf"), b"x").unwrap();
    std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();
    std::fs::write(dir.path().join("other.txt"), b"x").unwrap();
    std::fs::create_dir(dir.path().join("rest")).unwrap();
    let mut p = PathPrompt::new(&format!("{}/re", dir.path().display()));
    assert_eq!(p.key(k(KeyCode::Tab)), PromptOutcome::Stay);
    assert_eq!(
        p.matches(),
        ["readme.txt", "report.pdf", "rest/"],
        "sorted, directories marked"
    );
    p.key(k(KeyCode::Tab));
    assert!(p.value().ends_with("/readme.txt"));
    assert!(
        matches!(p.key(k(KeyCode::Enter)), PromptOutcome::Done(ref v) if v.ends_with("/readme.txt"))
    );
    // A single match is filled at once.
    let mut p = PathPrompt::new(&format!("{}/ot", dir.path().display()));
    p.key(k(KeyCode::Tab));
    assert!(p.value().ends_with("/other.txt"), "{}", p.value());
    let home = std::env::var("HOME").unwrap();
    let p = PathPrompt::new("~/x");
    assert_eq!(p.value_expanded(), format!("{home}/x"));
}

use rail_core::config::Icons;
use rail_core::store::drafts::DraftKind;

use super::model::{AttachmentRow, Composing};
use super::{ComposeOutcome, Composer, RowKind};
use crate::tui::keys::Action;
use crate::tui::screens::mail::Glyphs;
use crate::tui::test_util::snapshot;
use crate::tui::theme::Theme;

fn composing() -> Composing {
    Composing {
        draft: Some(7),
        account_index: 0,
        account_address: "me@example.com".into(),
        kind: DraftKind::Reply,
        reply_to: Some(3),
        headers: Headers {
            to: vec!["Dana Reyes <dana@example.com>".into()],
            cc: vec!["lee@example.com".into()],
            bcc: vec![],
            subject: "Re: Lunch?".into(),
        },
        body: "Yes please.\n\nOn Thu, 9 Oct 2025, Dana Reyes wrote:\n> Shall we?\n> a\n> b\n> c\n> d\n"
            .into(),
        attachments: vec![
            AttachmentRow {
                name: "menu.pdf".into(),
                mime: "application/pdf".into(),
                size: 10_240,
                path: None,
                message: Some(3),
                part: Some("p1".into()),
            },
            AttachmentRow {
                name: "notes.txt".into(),
                mime: "text/plain".into(),
                size: 300,
                path: Some("/tmp/notes.txt".into()),
                message: None,
                part: None,
            },
        ],
        message_id: Some("<x@example.com>".into()),
        errors: vec![],
    }
}

fn press(c: &mut Composer, action: Action) -> ComposeOutcome {
    c.key(
        Some(action),
        KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
    )
}

fn shown(c: &Composer, w: u16, h: u16) -> String {
    snapshot(w, h, |frame| {
        c.render(
            frame,
            frame.area(),
            &Theme::default(),
            &Glyphs::of(Icons::Unicode),
        )
    })
}

#[test]
fn review_keys_map_to_outcomes() {
    let mut c = Composer::new(composing());
    assert_eq!(press(&mut c, Action::ComposeSend), ComposeOutcome::Send);
    assert_eq!(press(&mut c, Action::ComposeEdit), ComposeOutcome::Edit);
    assert_eq!(press(&mut c, Action::ComposeClose), ComposeOutcome::Close);
    assert_eq!(
        press(&mut c, Action::ComposeDiscard),
        ComposeOutcome::Stay,
        "asks first"
    );
    assert!(c.confirming_discard());
    assert!(c.captures_text(), "the question takes raw keys");
    assert_eq!(c.key(None, k(KeyCode::Char('y'))), ComposeOutcome::Discard);
    press(&mut c, Action::ComposeDiscard);
    assert_eq!(c.key(None, k(KeyCode::Char('n'))), ComposeOutcome::Stay);
    assert!(!c.confirming_discard(), "n keeps the draft");
    let mut c = Composer::new(composing());
    assert_eq!(press(&mut c, Action::ComposeDown), ComposeOutcome::Stay);
    assert_eq!(c.cursor(), 1);
    assert_eq!(
        press(&mut c, Action::ComposeRemove),
        ComposeOutcome::Stay,
        "not on an attachment"
    );
    // Attachments are highlighted by moving onto their rows; x removes the highlighted one.
    while c.cursor_row_kind() != RowKind::Attachment(1) {
        press(&mut c, Action::ComposeDown);
    }
    assert_eq!(
        press(&mut c, Action::ComposeRemove),
        ComposeOutcome::Remove(1)
    );
    for _ in 0..100 {
        press(&mut c, Action::ComposePageDown);
    }
    assert_eq!(
        c.cursor_row_kind(),
        RowKind::Body,
        "the cursor stays inside the rows"
    );
    assert_eq!(press(&mut c, Action::ComposeAttach), ComposeOutcome::Stay);
    assert!(c.captures_text(), "the path prompt is open");
    assert_eq!(c.prompt_label(), Some("Attach"));
    for ch in "/tmp/x.pdf".chars() {
        c.key(None, k(KeyCode::Char(ch)));
    }
    assert_eq!(
        c.key(None, k(KeyCode::Enter)),
        ComposeOutcome::Attach("/tmp/x.pdf".into())
    );
    assert_eq!(press(&mut c, Action::ComposeHeaders), ComposeOutcome::Stay);
    assert!(c.captures_text());
    assert_eq!(
        c.key(None, k(KeyCode::Esc)),
        ComposeOutcome::Stay,
        "Esc in a prompt only closes it"
    );
    assert!(!c.captures_text());
}

#[test]
fn the_headers_prompts_run_to_cc_bcc_subject_in_order_and_report_the_change() {
    let mut c = Composer::new(composing());
    press(&mut c, Action::ComposeHeaders);
    assert_eq!(c.prompt_label(), Some("To"));
    assert_eq!(c.key(None, k(KeyCode::Enter)), ComposeOutcome::Stay);
    assert_eq!(c.prompt_label(), Some("Cc"));
    c.key(None, k(KeyCode::Enter));
    assert_eq!(c.prompt_label(), Some("Bcc"));
    c.key(None, k(KeyCode::Enter));
    assert_eq!(c.prompt_label(), Some("Subject"));
    for ch in " now".chars() {
        c.key(None, k(KeyCode::Char(ch)));
    }
    assert_eq!(
        c.key(None, k(KeyCode::Enter)),
        ComposeOutcome::HeadersChanged
    );
    assert_eq!(c.composing().headers.subject, "Re: Lunch? now");
    assert!(!c.captures_text());
}

#[test]
fn a_new_message_opens_on_its_prompts_with_suggestions_and_refuses_a_bad_address() {
    let mut fresh = composing();
    fresh.headers = Headers::default();
    fresh.draft = None;
    let mut c = Composer::new_with_prompts(fresh.clone(), &["To", "Subject"]);
    assert_eq!(c.prompt_label(), Some("To"));
    assert_eq!(
        c.key(None, k(KeyCode::Char('d'))),
        ComposeOutcome::SuggestionsWanted("d".into())
    );
    let stale = c.next_seq();
    let seq = c.next_seq();
    c.prompt_suggestions(stale, entries());
    assert_eq!(
        c.key(None, k(KeyCode::Enter)),
        ComposeOutcome::Stay,
        "a stale answer is ignored, and \"d\" is no address"
    );
    assert_eq!(c.prompt_label(), Some("To"), "refused: the prompt stays");
    assert!(shown(&c, 90, 24).contains("to: an address has no @"));
    c.prompt_suggestions(seq, entries());
    c.key(None, k(KeyCode::Char('a')));
    let seq = c.next_seq();
    c.prompt_suggestions(seq, entries());
    assert!(shown(&c, 90, 24).contains("Dana Reyes <dana@example.com>"));
    c.key(None, k(KeyCode::Tab));
    assert_eq!(c.key(None, k(KeyCode::Enter)), ComposeOutcome::Stay);
    assert_eq!(c.prompt_label(), Some("Subject"));
    for ch in "Hi".chars() {
        c.key(None, k(KeyCode::Char(ch)));
    }
    assert_eq!(
        c.key(None, k(KeyCode::Enter)),
        ComposeOutcome::HeadersChanged
    );
    assert_eq!(c.composing().headers.to, ["Dana Reyes <dana@example.com>"]);
    assert_eq!(c.composing().headers.subject, "Hi");
    let mut c = Composer::new_with_prompts(fresh, &["To", "Subject"]);
    assert_eq!(
        c.key(None, k(KeyCode::Esc)),
        ComposeOutcome::Discard,
        "Esc abandons a new message"
    );
}

#[test]
fn the_review_renders_at_both_sizes_with_folded_quotes_and_the_total() {
    let c = Composer::new(composing());
    for (w, h, name) in [
        (90, 24, "compose_review_90"),
        (150, 40, "compose_review_150"),
    ] {
        insta::assert_snapshot!(name, shown(&c, w, h));
    }
    let text = shown(&c, 90, 24);
    assert!(text.contains("From: me@example.com"), "{text}");
    assert!(
        text.contains("menu.pdf")
            && text.contains("10.0 KB")
            && text.contains("2 attachments, 10.3 KB"),
        "{text}"
    );
    assert!(text.contains("[5 quoted lines]"), "{text}");
    assert!(
        text.contains("y send · e edit · t headers · a attach · x remove · q keep · d discard"),
        "{text}"
    );
    let mut with_error = Composer::new(composing());
    with_error.set_error("to: an address is malformed".into());
    let text = shown(&with_error, 90, 24);
    assert!(text.contains("to: an address is malformed"), "{text}");
    with_error.composing_mut().attachments.clear();
    let text = shown(&with_error, 90, 24);
    assert!(
        !text.contains("attachments,"),
        "no attachments, no total: {text}"
    );
    assert_eq!(
        composing().recipients(),
        ["Dana Reyes <dana@example.com>", "lee@example.com"]
    );
    assert_eq!(composing().total_size(), 10_540);
    let idn = Composing {
        headers: Headers {
            to: vec!["x@bücher.example".into()],
            ..composing().headers
        },
        ..composing()
    };
    let text = shown(&Composer::new(idn), 150, 40);
    assert!(
        text.contains("x@bücher.example (xn--bcher-kva.example)"),
        "the ASCII form is shown beside the Unicode one: {text}"
    );
    let mut hostile = composing();
    hostile.headers.subject = "evil\u{1b}[2J\u{202e}".into();
    let text = shown(&Composer::new(hostile), 90, 24);
    assert!(text.contains("Subject: evil\\u{1b}[2J\\u{202e}"), "{text}");
    let mut c = Composer::new(composing());
    press(&mut c, Action::ComposeDiscard);
    assert!(shown(&c, 90, 24).contains("Discard this draft? (y/n)"));
}

#[test]
fn a_quoted_name_with_a_comma_stays_one_recipient() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = headers();
    h.to = vec!["\"Reyes, Dana\" <d@x.example>".into(), "s@x.example".into()];
    let path = write(dir.path(), "q", &h, "").unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("To: \"Reyes, Dana\" <d@x.example>, s@x.example\n"),
        "{text}"
    );
    let (parsed, _) = parse(&text).unwrap();
    assert_eq!(parsed.to, ["\"Reyes, Dana\" <d@x.example>", "s@x.example"]);
    assert_eq!(
        super::file::split_addresses(r#""A \"q\", B" <a@x.example>, b@x.example"#),
        [r#""A \"q\", B" <a@x.example>"#, "b@x.example"],
        "an escaped quote does not close the quote"
    );
    assert_eq!(
        super::file::split_addresses("\"open, a@x.example"),
        ["\"open", "a@x.example"],
        "unbalanced quotes: a plain split"
    );
    let mut p = HeaderPrompt::new("To", "", true);
    for ch in "\"Reyes, Dana\" <d@x.example>, sa".chars() {
        p.key(k(KeyCode::Char(ch)));
    }
    assert_eq!(p.current_item(), "sa");
    // The header prompts take the quoted name as one valid recipient.
    let mut c = Composer::new(composing());
    press(&mut c, Action::ComposeHeaders);
    for _ in 0.."Dana Reyes <dana@example.com>".len() {
        c.key(None, k(KeyCode::Backspace));
    }
    for ch in "\"Reyes, Dana\" <d@x.example>, s@x.example".chars() {
        c.key(None, k(KeyCode::Char(ch)));
    }
    assert_eq!(c.key(None, k(KeyCode::Enter)), ComposeOutcome::Stay);
    assert_eq!(c.prompt_label(), Some("Cc"), "accepted");
}

#[test]
fn header_values_cannot_break_out_of_their_line() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = headers();
    h.subject = "x\r\nBcc: a@evil.com".into();
    h.bcc = vec![];
    let path = write(dir.path(), "s", &h, "body\n").unwrap();
    let (parsed, body) = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.subject, "x  Bcc: a@evil.com");
    assert!(parsed.bcc.is_empty());
    assert_eq!(body, "body\n");
    let mut evil = source();
    evil.from[0].name = Some("Dana\nBcc: a@evil.com".into());
    evil.subject = "Lunch\r\nBcc: a@evil.com".into();
    let (h, body) = reply(&evil, false, "me@example.com", &jiff::tz::TimeZone::UTC);
    assert_eq!(h.to, ["Dana Bcc: a@evil.com <dana@example.com>"]);
    assert_eq!(h.subject, "Re: Lunch  Bcc: a@evil.com");
    assert!(body.contains("Dana Bcc: a@evil.com wrote:"), "{body}");
    let (h, body) = forward(&evil);
    assert_eq!(h.subject, "Fwd: Lunch  Bcc: a@evil.com");
    assert!(
        body.contains("\nFrom: Dana Bcc: a@evil.com <dana@example.com>\n"),
        "{body}"
    );
    assert!(
        body.contains("\nSubject: Lunch  Bcc: a@evil.com\n"),
        "{body}"
    );
}
