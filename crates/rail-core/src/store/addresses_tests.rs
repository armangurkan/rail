//! The addresses a sync batch teaches the store, and the suggestions `To:` completion reads.

use super::Store;
use crate::provider::{Envelope, Mailbox};
use crate::sync::batch::{Batch, Origin};
use crate::test_support::store_with_account;

fn envelope(id: &str, from: (&str, &str), to: &[(&str, &str)], ms: i64) -> Envelope {
    let mailbox = |(name, address): (&str, &str)| Mailbox {
        name: (!name.is_empty()).then(|| name.to_string()),
        address: address.to_string(),
    };
    Envelope {
        remote_id: id.into(),
        thread_id: format!("t-{id}"),
        from: vec![mailbox(from)],
        to: to.iter().copied().map(mailbox).collect(),
        date: ms / 1000,
        internal_date: ms,
        folders: vec!["INBOX".into()],
        ..Envelope::default()
    }
}

fn apply(store: &mut Store, account_id: i64, upserts: Vec<Envelope>, now: i64) {
    let batch = Batch {
        upserts,
        origin: Origin::Sync,
        ..Batch::new(account_id)
    };
    store.apply(&batch, now).unwrap();
}

#[test]
fn a_sync_batch_remembers_every_address_it_sees() {
    let (mut store, account_id) = store_with_account();
    apply(
        &mut store,
        account_id,
        vec![
            envelope(
                "1",
                ("Dana Reyes", "Dana@Example.com"),
                &[("", "me@example.com")],
                1_000,
            ),
            envelope(
                "2",
                ("", "dana@example.com"),
                &[("Sam", "sam@example.com")],
                2_000,
            ),
        ],
        10,
    );
    let all = store.address_suggestions("", 20).unwrap();
    let addresses: Vec<&str> = all.iter().map(|a| a.address.as_str()).collect();
    assert_eq!(
        addresses,
        ["dana@example.com", "me@example.com", "sam@example.com"],
        "by hits then recency"
    );
    assert_eq!(
        all[0].name.as_deref(),
        Some("Dana Reyes"),
        "the last non-empty name is kept, lower-cased key"
    );
}

#[test]
fn suggestions_match_the_address_or_a_word_of_the_name_case_insensitively() {
    let (mut store, account_id) = store_with_account();
    apply(
        &mut store,
        account_id,
        vec![envelope(
            "1",
            ("Dana Reyes", "dana@example.com"),
            &[("Sam Ortiz", "sam@work.example")],
            1_000,
        )],
        10,
    );
    let hits = |p: &str| -> Vec<String> {
        store
            .address_suggestions(p, 20)
            .unwrap()
            .into_iter()
            .map(|a| a.address)
            .collect()
    };
    assert_eq!(hits("rey"), ["dana@example.com"]);
    assert_eq!(hits("SAM"), ["sam@work.example"]);
    assert_eq!(
        hits("work"),
        ["sam@work.example"],
        "a prefix of the domain counts too"
    );
    assert!(hits("zzz").is_empty());
    assert_eq!(store.address_suggestions("", 1).unwrap().len(), 1, "limit");
    assert!(
        store.address_suggestions("", 500).unwrap().len() <= 20,
        "capped at MAX_SUGGESTIONS"
    );
}
