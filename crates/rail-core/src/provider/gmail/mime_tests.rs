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
    let text = error.to_string();
    assert!(text.contains("25 MiB"), "{text}");
    assert!(text.contains("33.3 MiB"), "the size is named: {text}");
}

const INJECTION: &str = "\r\nBcc: x@evil.com\r\nX-Evil: 1";

/// The message parses with one subject, only the real Bcc and no injected header.
fn assert_not_injected(outgoing: &Outgoing) {
    let raw = build(outgoing).unwrap();
    let parsed = MessageParser::default().parse(&raw).unwrap();
    let count = |name: &str| {
        parsed
            .headers()
            .iter()
            .filter(|h| h.name().eq_ignore_ascii_case(name))
            .count()
    };
    assert_eq!(count("Subject"), 1);
    assert_eq!(count("Bcc"), 1);
    assert_eq!(count("X-Evil"), 0);
    let bcc = parsed
        .bcc()
        .and_then(|a| a.first())
        .and_then(|a| a.address());
    assert_eq!(bcc, Some("bcc@example.com"));
}

#[test]
fn header_injection_in_subject_names_and_file_names_is_neutralised() {
    assert_not_injected(&Outgoing {
        subject: format!("Hi{INJECTION}"),
        ..outgoing()
    });
    assert_not_injected(&Outgoing {
        to: vec![Mailbox {
            name: Some(format!("Sam{INJECTION}")),
            address: "sam@example.com".into(),
        }],
        ..outgoing()
    });
    let mut with_file = outgoing();
    with_file.attachments[0].name = format!("notes{INJECTION}.txt");
    assert_not_injected(&with_file);
}

fn refused(outgoing: &Outgoing) -> String {
    let error = build(outgoing).unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
    let text = error.to_string();
    assert!(!text.contains("evil"), "the value is not echoed: {text}");
    text
}

#[test]
fn header_injection_in_addresses_ids_and_mime_types_is_refused() {
    let mut to = outgoing();
    to.to[0].address = format!("sam@example.com{INJECTION}");
    refused(&to);
    let mut from = outgoing();
    from.from = format!("dana@example.com{INJECTION}");
    refused(&from);
    refused(&Outgoing {
        in_reply_to: Some(format!("<m1@example.com{INJECTION}>")),
        ..outgoing()
    });
    refused(&Outgoing {
        references: vec![format!("<m0@example.com{INJECTION}>")],
        ..outgoing()
    });
    refused(&Outgoing {
        message_id: format!("<a@example.com{INJECTION}>"),
        ..outgoing()
    });
    let mut mime = outgoing();
    mime.attachments[0].mime = format!("text/plain{INJECTION}");
    refused(&mime);
}

#[test]
fn malformed_addresses_ids_and_mime_types_are_refused() {
    for bad in ["no-at", "a@b@c", "a b@c", "a@b\u{7}", "@c", "a@"] {
        let mut o = outgoing();
        o.cc[0].address = bad.into();
        refused(&o);
    }
    for bad in ["<no-at>", "<a@b c>", "<a@<b>", "<>"] {
        refused(&Outgoing {
            references: vec![bad.into()],
            ..outgoing()
        });
    }
    for bad in [
        "text",
        "text/",
        "/plain",
        "a/b/c",
        "te xt/plain",
        "text/pl;ain",
        "text/plain; x=1",
    ] {
        let mut o = outgoing();
        o.attachments[0].mime = bad.into();
        refused(&o);
    }
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
