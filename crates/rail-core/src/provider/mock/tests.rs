use super::*;

#[tokio::test]
async fn a_sent_message_is_found_afterwards_by_its_header_id() {
    let mock = MockProvider::new();
    let raw = b"From: a@example.com\r\nMessage-ID: <abc@example.com>\r\n\r\nHi";
    assert_eq!(mock.find_header("<abc@example.com>").await.unwrap(), None);
    let receipt = mock.send_raw(raw, None).await.unwrap();
    assert_eq!(receipt.message_id.as_deref(), Some("sent-1"));
    assert_eq!(receipt.thread_id.as_deref(), Some("t-sent-1"));
    assert_eq!(
        mock.find_header("<abc@example.com>").await.unwrap(),
        Some("found-<abc@example.com>".to_string())
    );
    let again = mock.send_raw(raw, Some("t9")).await.unwrap();
    assert_eq!(
        (
            again.message_id.as_deref().unwrap(),
            again.thread_id.as_deref().unwrap()
        ),
        ("sent-2", "t9")
    );
    let calls: Vec<Call> = mock.with(|m| m.calls.iter().map(|(c, _)| *c).collect());
    assert_eq!(
        calls,
        [
            Call::FindByHeader,
            Call::Send,
            Call::FindByHeader,
            Call::Send
        ]
    );
    assert_eq!(
        mock.with(|m| m.sent[1].thread_id.clone()),
        Some("t9".to_string())
    );
}

#[tokio::test]
async fn the_mock_serves_attachment_bytes_or_not_found() {
    let mock = MockProvider::new();
    mock.with(|m| {
        m.attachment_bytes
            .insert(("m1".into(), "p1".into()), vec![1, 2])
    });
    assert_eq!(
        mock.attachment(&"m1".to_string(), "p1").await.unwrap(),
        [1, 2]
    );
    assert!(matches!(
        mock.attachment(&"m1".to_string(), "p2").await,
        Err(ProviderError::NotFound)
    ));
}

#[tokio::test]
async fn the_mock_records_thread_changes_and_plays_them_as_history() {
    let mock = MockProvider::new();
    mock.seed(envelope("m1", "t1", 1_000));
    let cursor = SyncCursor(mock.with(|m| m.history_id).to_string());
    mock.modify(
        &["t1".into()],
        &["STARRED".into()],
        &["INBOX".into(), "UNREAD".into()],
    )
    .await
    .unwrap();
    mock.trash(&["t1".into()]).await.unwrap();
    let calls: Vec<Call> = mock.with(|m| m.calls.iter().map(|(c, _)| *c).collect());
    assert_eq!(calls, [Call::Modify, Call::Trash]);
    let env = mock.with(|m| m.messages["m1"].clone());
    assert!(env.is_starred && env.is_read);
    assert_eq!(env.folders, ["TRASH"]);
    let changes = mock.changes(&cursor).await.unwrap();
    assert_eq!(changes.relabeled.len(), 2);
    mock.fail_next(Call::Trash, ProviderError::NotFound);
    assert_eq!(
        mock.trash(&["t1".into()]).await,
        Err(ProviderError::NotFound)
    );
}

#[tokio::test]
async fn the_mock_pages_newest_first_and_plays_history() {
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 2);
    for (i, ms) in [1_000, 3_000, 2_000].into_iter().enumerate() {
        mock.seed(envelope(&format!("m{i}"), "t", ms));
    }
    let first = mock.list(&Window::default(), None).await.unwrap();
    assert_eq!(first.ids, ["m1", "m2"]);
    let second = mock
        .list(&Window::default(), first.next.as_ref())
        .await
        .unwrap();
    assert_eq!(second.ids, ["m0"]);
    assert!(second.next.is_none());
    let start = mock.profile().await.unwrap().cursor;
    mock.add(envelope("new", "t2", 9_000));
    mock.delete("m0");
    let read = Relabel {
        remote_id: "m1".into(),
        add_folders: vec!["STARRED-ish".into()],
        remove_folders: vec!["INBOX".into()],
        read: Some(true),
        starred: None,
    };
    mock.relabel(read.clone());
    let m1 = mock.with(|m| m.messages["m1"].clone());
    assert_eq!(m1.folders, ["STARRED-ish"]);
    assert!(m1.is_read && !m1.is_starred);
    let changes = mock.changes(&start).await.unwrap();
    assert_eq!(changes.added[0].remote_id, "new");
    assert_eq!(changes.deleted, ["m0"]);
    assert_eq!(changes.relabeled, [read]);
    assert!(!changes.more);
    assert_eq!(
        mock.calls(),
        [Call::Backfill, Call::Backfill, Call::Profile, Call::Changes]
    );
    assert_eq!(
        mock.backfill_windows(),
        [Window::default(), Window::default()]
    );
    mock.with(|m| m.expired_below = u64::MAX);
    assert_eq!(
        mock.changes(&start).await.unwrap_err(),
        ProviderError::CursorExpired
    );
}

#[tokio::test]
async fn the_mock_windows_in_whole_seconds_like_gmail() {
    let mock = MockProvider::new();
    mock.seed(envelope("a", "t", 5_000));
    mock.seed(envelope("b", "t", 5_900));
    let window = Window {
        after_ms: None,
        before_ms: Some(5_500),
    };
    let page = mock.list(&window, None).await.unwrap();
    assert!(page.ids.is_empty(), "before:5 excludes all of second 5");
    mock.fail_next(Call::Folders, ProviderError::transient("down"));
    assert!(mock.folders().await.is_err());
    assert!(mock.folders().await.is_ok());
}

/// The mock's tokens behave as the engine must assume Gmail's do: valid only for their
/// own listing, refused once stale; and a shuffled listing is out of order.
#[tokio::test]
async fn the_mock_refuses_stale_or_foreign_tokens_and_can_shuffle() {
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 1);
    for n in 0..10 {
        mock.seed(envelope(&format!("m{n}"), "t", 10_000 * (n + 1)));
    }
    let first = mock.list(&Window::default(), None).await.unwrap();
    let token = first.next.unwrap();
    let other = Window {
        after_ms: Some(1),
        before_ms: None,
    };
    assert_eq!(
        mock.list(&other, Some(&token)).await.unwrap_err(),
        ProviderError::PageTokenRejected
    );
    assert!(mock.list(&Window::default(), Some(&token)).await.is_ok());
    mock.expire_page_tokens();
    assert_eq!(
        mock.list(&Window::default(), Some(&token))
            .await
            .unwrap_err(),
        ProviderError::PageTokenRejected
    );
    mock.with(|m| {
        m.page_size = 10;
        m.shuffle_within_ms = 1_000_000;
    });
    let all = mock.list(&Window::default(), None).await.unwrap();
    let dates: Vec<i64> = all
        .ids
        .iter()
        .map(|id| mock.with(|m| m.messages[id].internal_date))
        .collect();
    assert!(dates.windows(2).any(|w| w[0] < w[1]), "{dates:?}");
    let page = mock.envelopes(&all.ids[..2]).await.unwrap();
    assert_eq!(page.envelopes.len(), 2);
    assert_eq!(mock.with(|m| m.metadata_gets.len()), 2);
}

#[tokio::test]
async fn a_draft_update_keeps_the_draft_id_and_changes_the_message_id() {
    let mock = MockProvider::new();
    let created = mock.create_raw(b"one", Some("t1")).await.unwrap();
    let updated = mock
        .update_raw(&created.draft_id, b"two", None)
        .await
        .unwrap();
    assert_eq!(updated.draft_id, created.draft_id);
    assert_ne!(updated.message_id, created.message_id);
    mock.with(|m| {
        let draft = &m.drafts[&created.draft_id];
        assert_eq!(draft.raw, b"two");
        assert_eq!(draft.thread_id, None);
    });
    assert_eq!(
        mock.calls(),
        [Call::DraftCreate, Call::DraftUpdate],
        "each draft call is recorded"
    );
}

#[tokio::test]
async fn updating_or_deleting_a_missing_draft_is_not_found() {
    let mock = MockProvider::new();
    assert_eq!(
        mock.update_raw("r9", b"x", None).await.unwrap_err(),
        ProviderError::NotFound
    );
    assert_eq!(
        mock.draft_delete("r9").await.unwrap_err(),
        ProviderError::NotFound
    );
    let created = mock.create_raw(b"x", None).await.unwrap();
    mock.with(|m| {
        m.missing_drafts.insert(created.draft_id.clone());
    });
    assert_eq!(
        mock.update_raw(&created.draft_id, b"y", None)
            .await
            .unwrap_err(),
        ProviderError::NotFound
    );
    assert_eq!(
        mock.draft_delete(&created.draft_id).await.unwrap_err(),
        ProviderError::NotFound
    );
}

#[tokio::test]
async fn find_answers_the_draft_of_a_message_and_a_delete_removes_it() {
    let mock = MockProvider::new();
    let created = mock.create_raw(b"x", None).await.unwrap();
    assert_eq!(
        mock.draft_find(&created.message_id).await.unwrap(),
        Some(created.draft_id.clone())
    );
    assert_eq!(mock.draft_find("nope").await.unwrap(), None);
    mock.draft_delete(&created.draft_id).await.unwrap();
    assert_eq!(mock.draft_find(&created.message_id).await.unwrap(), None);
}

#[tokio::test]
async fn a_draft_create_in_a_thread_gone_at_gmail_is_not_found_and_saves_nothing() {
    let mock = MockProvider::new();
    mock.with(|m| m.missing_threads.insert("t-gone".into()));
    assert_eq!(
        mock.create_raw(b"x", Some("t-gone")).await.unwrap_err(),
        ProviderError::NotFound
    );
    assert!(mock.with(|m| m.drafts.is_empty()));
    let created = mock.create_raw(b"x", None).await.unwrap();
    assert_eq!(created.draft_id, "r1", "the refused create took no id");
}

#[tokio::test]
async fn the_mock_applies_a_change_as_gmail_would_and_records_it() {
    let mock = MockProvider::new();
    mock.seed(envelope("m1", "t1", 1_000));
    mock.apply(&["t1".into()], &crate::provider::Change::Archive)
        .await
        .unwrap();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        mock.with(|m| m.modifies.clone()),
        [(s(&["t1"]), s(&[]), s(&["INBOX"]))]
    );
    assert_eq!(mock.with(|m| m.applied.len()), 1);
}

#[tokio::test]
async fn the_mocks_writing_methods_play_gmails_adapter_and_graph_mode_consumes_the_draft() {
    use crate::provider::{DraftIds, Outgoing, OutgoingMail};
    use crate::store::drafts::DraftKind;
    let mail = |remote: Option<DraftIds>, thread: Option<&str>| OutgoingMail {
        outgoing: Outgoing {
            from_name: None,
            from: "me@example.com".into(),
            to: vec![crate::provider::Mailbox {
                name: None,
                address: "sam@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            message_id: "<w@example.com>".into(),
            in_reply_to: None,
            references: vec![],
            body: "b\n".into(),
            attachments: vec![],
        },
        kind: DraftKind::New,
        source: None,
        remote,
        thread: thread.map(str::to_string),
    };
    let mock = MockProvider::new();
    mock.with(|m| {
        m.missing_threads.insert("t-gone".into());
        m.missing_drafts.insert("r9".into());
    });
    let gone = DraftIds {
        draft_id: "r9".into(),
        message_id: "x".into(),
    };
    let saved = mock
        .draft_save(&mail(Some(gone), Some("t-gone")))
        .await
        .unwrap()
        .ids;
    assert_eq!(saved.draft_id, "r1");
    assert_eq!(
        mock.calls(),
        [Call::DraftUpdate, Call::DraftCreate, Call::DraftCreate],
        "update 404, create in the gone thread 404, create on its own"
    );
    assert!(!mock.sent_already(&mail(None, None)).await.unwrap());
    mock.send(&mail(None, Some("t-gone"))).await.unwrap();
    assert_eq!(mock.with(|m| m.sent[0].thread_id.clone()), None);
    assert!(mock.sent_already(&mail(None, None)).await.unwrap());
    assert!(!mock.send_consumes_draft() && !mock.forward_carries_originals());
    mock.with(|m| m.graph_model = true);
    mock.send(&mail(Some(saved), None)).await.unwrap();
    assert!(
        mock.with(|m| m.drafts.is_empty()),
        "Graph's send consumes the draft"
    );
    assert!(mock.send_consumes_draft() && mock.forward_carries_originals());
    assert_eq!(mock.with(|m| m.sent_mail.len()), 2);
}
