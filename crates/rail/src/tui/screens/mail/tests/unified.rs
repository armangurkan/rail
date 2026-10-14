//! The merged mailboxes through the mail screen: Starred, Sent, Trash and Drafts across both
//! accounts, their header, the filter, the palette and compose's account question.

use crossterm::event::KeyCode;
use rail_core::sync::Batch;

use super::{Fixture, fixture, press, store_changed, writer};
use crate::tui::data::ScopeChoice;
use crate::tui::modals::Modal;
use crate::tui::test_util::{BASE_MS, mail_envelope, settle};

/// `fixture(3)` plus: a starred thread of `work` only under Projects, a starred one in `home`'s
/// inbox, and a read sent message of each account.
async fn merged_fixture() -> Fixture {
    let mut f = fixture(3).await;
    let mut store = writer(&f);
    let mut starred = mail_envelope("ws", "tws", BASE_MS + 600_000);
    starred.folders = vec!["Label_1".into()];
    starred.is_starred = true;
    let mut sent = mail_envelope("wsent", "twsent", BASE_MS + 300_000);
    sent.folders = vec!["SENT".into()];
    sent.is_read = true;
    let work = Batch {
        upserts: vec![starred, sent],
        ..Batch::new(1)
    };
    store.apply(&work, BASE_MS / 1000).unwrap();
    let mut home_starred = mail_envelope("hs", "ths", BASE_MS + 500_000);
    home_starred.is_starred = true;
    let mut home_sent = mail_envelope("hsent", "thsent", BASE_MS + 200_000);
    home_sent.folders = vec!["SENT".into()];
    home_sent.is_read = true;
    let home = Batch {
        upserts: vec![home_starred, home_sent],
        ..Batch::new(2)
    };
    store.apply(&home, BASE_MS / 1000).unwrap();
    store_changed(&mut f).await;
    f
}

async fn show(f: &mut Fixture, scope: ScopeChoice) {
    let effects = f.screen.set_scope(scope);
    settle(&mut f.screen, effects).await;
}

fn subjects(f: &Fixture) -> Vec<String> {
    f.screen
        .list
        .rows()
        .iter()
        .map(|r| r.subject.clone())
        .collect()
}

#[tokio::test]
async fn starred_lists_both_accounts_newest_first_and_counts_its_unread() {
    let mut f = merged_fixture().await;
    show(&mut f, ScopeChoice::Starred).await;
    assert_eq!(subjects(&f), ["subject ws", "subject hs"]);
    assert_eq!(
        f.screen.header_left(),
        "▍Rail │ Starred · 2 accounts · 2 unread"
    );
    f.screen.filter_text = "account:home".into();
    let effects = f.screen.apply_filter();
    settle(&mut f.screen, effects).await;
    assert_eq!(
        subjects(&f),
        ["subject hs"],
        "the filter works as in the unified inbox"
    );
}

#[tokio::test]
async fn sent_and_trash_merge_every_account_and_count_nothing() {
    let mut f = merged_fixture().await;
    show(&mut f, ScopeChoice::Sent).await;
    assert_eq!(subjects(&f), ["subject wsent", "subject hsent"]);
    assert_eq!(f.screen.header_left(), "▍Rail │ Sent · 2 accounts");
    show(&mut f, ScopeChoice::Trash).await;
    assert!(
        subjects(&f).is_empty(),
        "no account has a Trash folder here"
    );
    assert_eq!(f.screen.header_left(), "▍Rail │ Trash · 2 accounts");
}

#[tokio::test]
async fn merged_drafts_open_the_drafts_pane_for_every_account() {
    let mut f = merged_fixture().await;
    show(&mut f, ScopeChoice::Drafts).await;
    assert!(f.screen.in_drafts());
    assert_eq!(f.screen.scope_account_ids(), [1, 2]);
    assert_eq!(f.screen.header_left(), "▍Rail │ Drafts · 2 accounts");
    assert_eq!(f.screen.slot_text(), "", "no filter in the Drafts pane");
}

#[tokio::test]
async fn the_palette_offers_each_merged_mailbox_and_compose_asks_for_an_account() {
    let mut f = merged_fixture().await;
    let entries = f.screen.palette_entries();
    for name in ["Starred", "Drafts", "Sent", "Trash"] {
        let entry = entries
            .iter()
            .find(|e| e.text == format!("Mailbox: {name}"))
            .unwrap_or_else(|| panic!("no `Mailbox: {name}`"));
        assert!(
            entry.search.contains(&name.to_lowercase()),
            "{}",
            entry.search
        );
    }
    show(&mut f, ScopeChoice::Starred).await;
    press(&mut f, KeyCode::Char('c')).await;
    assert!(
        matches!(f.screen.modal, Some(Modal::Picker(_))),
        "a merged mailbox asks which account writes"
    );
}
