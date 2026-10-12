use mail_parser::{MessageParser, MimeHeaders};

use super::mime::{MAX_MESSAGE_BYTES, build, sanitize_filename};
use crate::provider::{Mailbox, Outgoing, OutgoingAttachment, ProviderError};

fn outgoing() -> Outgoing {
    Outgoing {
        from_name: Some("Dana Reyes".into()),
        from: "dana@example.com".into(),
        to: vec![Mailbox {
            name: Some("Sam".into()),
            address: "sam@example.com".into(),
        }],
        cc: vec![Mailbox {
            name: None,
            address: "cc@example.com".into(),
        }],
        bcc: vec![Mailbox {
            name: None,
            address: "bcc@example.com".into(),
        }],
        subject: "Re: Hello ✓".into(),
        message_id: "<11111111-2222-4333-8444-555555555555@example.com>".into(),
        in_reply_to: Some("<m1@example.com>".into()),
        references: vec!["<m0@example.com>".into(), "<m1@example.com>".into()],
        body: "Hi Sam,\n\n> quoted\n\n-- \nDana\n".into(),
        attachments: vec![
            OutgoingAttachment {
                name: "notes.txt".into(),
                mime: "text/plain".into(),
                bytes: b"notes".to_vec(),
            },
            OutgoingAttachment {
                name: "../evil\u{202E}.pdf".into(),
                mime: "application/pdf".into(),
                bytes: vec![1, 2, 3],
            },
        ],
    }
}

#[test]
fn a_message_round_trips_through_mail_parser() {
    let raw = build(&outgoing()).unwrap();
    let parsed = MessageParser::default().parse(&raw).expect("parses");
    let from = parsed.from().and_then(|a| a.first()).unwrap();
    assert_eq!(from.name(), Some("Dana Reyes"));
    assert_eq!(from.address(), Some("dana@example.com"));
    let first = |a: Option<&mail_parser::Address<'_>>| {
        a.and_then(|a| a.first())
            .and_then(|a| a.address())
            .map(str::to_string)
    };
    assert_eq!(first(parsed.to()).as_deref(), Some("sam@example.com"));
    assert_eq!(first(parsed.cc()).as_deref(), Some("cc@example.com"));
    assert_eq!(first(parsed.bcc()).as_deref(), Some("bcc@example.com"));
    assert_eq!(parsed.subject(), Some("Re: Hello ✓"));
    assert_eq!(
        parsed.message_id(),
        Some("11111111-2222-4333-8444-555555555555@example.com")
    );
    assert_eq!(parsed.in_reply_to().as_text(), Some("m1@example.com"));
    let refs = parsed.references().as_text_list().unwrap_or_default();
    assert_eq!(refs, ["m0@example.com", "m1@example.com"]);
    assert!(parsed.date().is_some(), "Date is set");
    // On the wire line ends are CRLF; the text is otherwise as written, signature rule included.
    let text = parsed.body_text(0).unwrap().replace("\r\n", "\n");
    assert_eq!(text, "Hi Sam,\n\n> quoted\n\n-- \nDana\n");
    let names: Vec<String> = parsed
        .attachments()
        .map(|a| a.attachment_name().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, ["notes.txt", "evil.pdf"], "file names are sanitised");
    assert_eq!(parsed.attachments().nth(1).unwrap().contents(), [1, 2, 3]);
}

#[test]
fn a_bare_from_and_no_threading_headers_are_fine() {
    let plain = Outgoing {
        from_name: None,
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
        ..outgoing()
    };
    let raw = build(&plain).unwrap();
    let parsed = MessageParser::default().parse(&raw).unwrap();
    assert_eq!(
        parsed.from().and_then(|a| a.first()).and_then(|a| a.name()),
        None
    );
    assert!(parsed.in_reply_to().as_text().is_none());
    assert!(parsed.references().as_text_list().is_none());
    assert_eq!(parsed.attachment_count(), 0);
}

#[test]
fn a_message_over_the_cap_is_refused_before_any_call() {
    let big = Outgoing {
        attachments: vec![OutgoingAttachment {
            name: "big.bin".into(),
            mime: "application/octet-stream".into(),
            bytes: vec![0; MAX_MESSAGE_BYTES],
        }],
        ..outgoing()
    };
    let error = build(&big).unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("25 MiB"), "{error}");
}

#[test]
fn file_names_lose_paths_controls_and_bidi_and_keep_a_length() {
    assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
    assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
    assert_eq!(sanitize_filename("a\u{202E}fdp.exe"), "afdp.exe");
    assert_eq!(sanitize_filename("tab\there\n"), "tab here");
    assert_eq!(sanitize_filename(""), "attachment");
    assert_eq!(sanitize_filename("   "), "attachment");
    assert!(sanitize_filename(&"x".repeat(400)).len() <= 255);
}
