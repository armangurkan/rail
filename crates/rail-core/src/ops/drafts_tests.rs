use super::drafts::*;
use crate::store::drafts::DraftKind;

fn save() -> DraftSaveRequest {
    DraftSaveRequest {
        account: "work".into(),
        draft: None,
        kind: DraftKind::New,
        reply_to: None,
        to: vec!["Sam <sam@example.com>".into()],
        cc: vec![],
        bcc: vec!["bcc@example.com".into()],
        subject: "Hello".into(),
        body: "hi\n".into(),
        attachments: vec![],
        client_token: "tok_1".into(),
    }
}

#[test]
fn recipients_parse_bare_and_named_forms_and_refuse_the_rest() {
    let named = parse_recipient("  Dana Reyes <dana@example.com> ").unwrap();
    assert_eq!(named.name.as_deref(), Some("Dana Reyes"));
    assert_eq!(named.address, "dana@example.com");
    let quoted = parse_recipient("\"Reyes, Dana\" <dana@example.com>").unwrap();
    assert_eq!(quoted.name.as_deref(), Some("Reyes, Dana"));
    let bare = parse_recipient("dana@example.com").unwrap();
    assert_eq!(bare.name, None);
    let idn = parse_recipient("dana@xn--bcher-kva.example").unwrap();
    assert_eq!(idn.address, "dana@xn--bcher-kva.example");
    for bad in [
        "",
        "dana",
        "@example.com",
        "dana@",
        "dana@@example.com",
        "dana@exa mple.com",
        "a\u{202E}@example.com",
        "Dana <dana@example.com",
        "<dana@example.com> extra",
        &format!("{}@example.com", "a".repeat(250)),
        "dana@-bad.example",
        "dana@example",
    ] {
        let error = parse_recipient(bad).unwrap_err();
        assert!(!error.contains('\u{202E}'), "{error}");
    }
}

#[test]
fn save_checks_every_bound_without_echoing_an_address() {
    assert!(save().checked().is_ok());
    let mut many = save();
    many.to = (0..101).map(|i| format!("r{i}@example.com")).collect();
    let error = many.checked().unwrap_err();
    assert!(
        error.message.contains("100 recipients"),
        "{}",
        error.message
    );
    let mut bad = save();
    bad.to = vec!["nope".into()];
    let error = bad.checked().unwrap_err();
    assert!(error.message.contains("to"), "{}", error.message);
    assert!(
        !error.message.contains("nope") || error.message.contains("\"nope\""),
        "quoted only"
    );
    let mut subject = save();
    subject.subject = "a\nb".into();
    assert!(subject.checked().is_err());
    subject.subject = "x".repeat(999);
    assert!(subject.checked().is_err());
    let mut body = save();
    body.body = "x".repeat(MAX_BODY_BYTES + 1);
    assert!(body.checked().is_err());
    let mut relative = save();
    relative.attachments = vec![DraftAttachment::File {
        path: "relative.pdf".into(),
    }];
    assert!(relative.checked().unwrap_err().message.contains("absolute"));
    let mut token = save();
    token.client_token = "a b".into();
    assert!(token.checked().is_err());
    let mut reply = save();
    reply.kind = DraftKind::Reply;
    assert!(
        reply.checked().is_err(),
        "a reply names the message it answers"
    );
    reply.draft = Some(7);
    assert!(
        reply.checked().is_ok(),
        "an update keeps the message it was created for"
    );
}

#[test]
fn a_message_id_has_the_shape_and_the_domain() {
    let id = new_message_id("Dana@Example.com");
    assert!(id.starts_with('<') && id.ends_with("@example.com>"), "{id}");
    assert_eq!(id.len(), 1 + 36 + 1 + "example.com".len() + 1);
    assert_ne!(new_message_id("a@b.c"), new_message_id("a@b.c"));
    assert!(new_message_id("noat").ends_with("@rail.invalid>"));
}

#[test]
fn requests_refuse_unknown_fields() {
    assert!(
        serde_json::from_str::<DraftSendRequest>(r#"{"draft":1,"client_token":"t","x":1}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<DraftDiscardRequest>(r#"{"draft":1,"x":1}"#).is_err());
}

/// What `mime::build` refuses must not save: no RFC 5322 special in the address part.
#[test]
fn an_address_holds_no_rfc_5322_special_and_a_name_keeps_its_quotes_closed() {
    for special in ['(', ')', '<', '>', '[', ']', ':', ';', '\\', ',', '"'] {
        let text = format!("da{special}na@example.com");
        assert!(parse_recipient(&text).is_err(), "{text:?}");
        let named = format!("Dana <da{special}na@example.com>");
        assert!(parse_recipient(&named).is_err(), "{named:?}");
    }
    for bad in [
        ".dana@example.com",
        "dana.@example.com",
        "da..na@example.com",
    ] {
        assert!(parse_recipient(bad).is_err(), "{bad:?}");
    }
    let quoted = parse_recipient("\"Dana (Ops); <x>\" <dana@example.com>").unwrap();
    assert_eq!(quoted.name.as_deref(), Some("Dana (Ops); <x>"));
    for bad in [
        "Da\"na <dana@example.com>",
        "\"Da\"na\" <dana@example.com>",
        "Da>na <dana@example.com>",
    ] {
        assert!(parse_recipient(bad).is_err(), "{bad:?}");
    }
    let long = format!("{} <dana@example.com>", "n".repeat(MAX_NAME_CHARS + 1));
    assert!(parse_recipient(&long).is_err());
    assert_eq!(
        parse_recipient("\"\" <dana@example.com>").unwrap().name,
        None
    );
}

#[test]
fn a_refusal_names_its_field_and_quotes_only_a_short_address() {
    let mut cc = save();
    cc.cc = vec!["a\u{202E}b".into()];
    let error = cc.checked().unwrap_err();
    assert!(error.message.starts_with("cc: "), "{}", error.message);
    assert!(!error.message.contains('\u{202E}'), "{}", error.message);
    let mut long = save();
    long.bcc = vec!["x".repeat(10_000)];
    let error = long.checked().unwrap_err();
    assert!(
        error.message.starts_with("bcc: ") && error.message.len() < 200,
        "{}",
        error.message
    );
    let mut token = save();
    token.client_token = String::new();
    assert!(
        token
            .checked()
            .unwrap_err()
            .message
            .starts_with("client_token: ")
    );
    let mut draft = save();
    draft.draft = Some(0);
    assert!(draft.checked().unwrap_err().message.starts_with("draft: "));
    let mut twice = save();
    twice.attachments = vec![
        DraftAttachment::File {
            path: "/a.pdf".into()
        };
        2
    ];
    assert!(twice.checked().unwrap_err().message.contains("twice"));
}
