//! Microsoft 365 in the mail screen: its thread actions go through like Gmail's; `l` says
//! Outlook has folders; `m` lists the Inbox and the account's own folders; the trash prompt
//! names Deleted Items; a selection across both kinds sends one call per account; every
//! compose start is refused; opening a thread still marks it read; its Drafts rows are
//! refused; and the bar says `saving to Microsoft 365…`.

use rail_core::config::AccountKind;
use rail_core::ops::{AccountSyncStatus, Action as MailAction, M365_LABELS, M365_NOT_YET};
use rail_core::store::drafts_folder::{DraftsRow, DraftsRowKind};

use super::*;

/// `work` with no threads and `home`, a Microsoft 365 account, with two: the cursor row is
/// home's newest.
async fn m365_fixture() -> Fixture {
    let mut f = fixture(0).await;
    f.screen.accounts[1].kind = AccountKind::M365;
    f
}

/// `m365_fixture`, with the store saying so too: the move picker reads the kind there.
async fn m365_store_fixture() -> Fixture {
    let f = m365_fixture().await;
    writer(&f)
        .conn()
        .execute("UPDATE accounts SET kind = 'm365' WHERE name = 'home'", [])
        .unwrap();
    f
}

/// `fixture(3)` with `home` a Microsoft 365 account: the two newest unified rows are one thread
/// of each account.
async fn mixed_fixture() -> Fixture {
    let mut f = fixture(3).await;
    f.screen.accounts[1].kind = AccountKind::M365;
    f
}

#[tokio::test]
async fn the_thread_actions_on_a_microsoft_365_thread_go_through() {
    let mut f = m365_fixture().await;
    for key in ['s', 'u', 'e'] {
        press(&mut f, KeyCode::Char(key)).await;
    }
    let sent: Vec<(String, MailAction)> = applies(&f)
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::Apply {
                account, action, ..
            } => Some((account, action)),
            _ => None,
        })
        .collect();
    assert!(
        sent.iter().all(|(account, _)| account == "home"),
        "{sent:?}"
    );
    let actions: Vec<&MailAction> = sent.iter().map(|(_, action)| action).collect();
    assert!(
        matches!(
            actions[..],
            [
                MailAction::Star { .. },
                MailAction::Read { .. },
                MailAction::Archive {}
            ]
        ),
        "{actions:?}"
    );
}

#[tokio::test]
async fn l_on_a_microsoft_365_thread_says_outlook_has_folders() {
    let mut f = m365_fixture().await;
    press(&mut f, KeyCode::Char('l')).await;
    let line = f.screen.status_line();
    assert!(line.contains(M365_LABELS), "{line}");
    assert!(f.screen.modal.is_none(), "no picker");
    assert!(applies(&f).is_empty());
}

#[tokio::test]
async fn m_lists_the_inbox_and_the_accounts_own_folders_on_microsoft_365() {
    let mut f = m365_store_fixture().await;
    press(&mut f, KeyCode::Char('m')).await;
    let Some(Modal::Picker(picker)) = &f.screen.modal else {
        panic!("no picker");
    };
    assert_eq!(picker.names(), ["Inbox", "Projects"]);
}

#[tokio::test]
async fn m_lists_the_labels_on_gmail_as_before() {
    let mut f = fixture(1).await;
    press(&mut f, KeyCode::Char('m')).await;
    let Some(Modal::Picker(picker)) = &f.screen.modal else {
        panic!("no picker");
    };
    assert_eq!(picker.names(), ["Projects"]);
}

#[tokio::test]
async fn the_trash_prompt_names_each_providers_trash() {
    let mut f = m365_fixture().await;
    press(&mut f, KeyCode::Char('d')).await;
    let Some(Modal::Confirm(confirm)) = &f.screen.modal else {
        panic!("no prompt");
    };
    assert_eq!(confirm.lines()[1..], ["Moves to Deleted Items."]);
    let mut f = mixed_fixture().await;
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char('d')).await;
    let Some(Modal::Confirm(confirm)) = &f.screen.modal else {
        panic!("no prompt");
    };
    assert_eq!(
        confirm.lines()[1..],
        [
            "Gmail keeps trashed mail in its Trash for 30 days.",
            "Moves to Deleted Items."
        ]
    );
}

#[tokio::test]
async fn a_selection_across_gmail_and_microsoft_365_sends_one_call_per_account() {
    let mut f = mixed_fixture().await;
    let ids = thread_ids(&f);
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char('e')).await;
    assert_eq!(
        applies(&f),
        [
            FakeCall::Apply {
                account: "work".into(),
                threads: vec![ids[0]],
                action: MailAction::Archive {},
                grace: true
            },
            FakeCall::Apply {
                account: "home".into(),
                threads: vec![ids[1]],
                action: MailAction::Archive {},
                grace: true
            },
        ]
    );
}

#[tokio::test]
async fn l_on_a_selection_holding_a_microsoft_365_thread_is_refused_whole() {
    let mut f = mixed_fixture().await;
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char('l')).await;
    assert!(f.screen.status_line().contains(M365_LABELS));
    assert!(f.screen.modal.is_none());
    assert!(applies(&f).is_empty(), "not even the Gmail thread");
}

#[tokio::test]
async fn opening_a_microsoft_365_thread_still_marks_it_read() {
    let mut f = m365_fixture().await;
    let top = f.screen.list.selected().unwrap().key.thread_id;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        applies(&f),
        [FakeCall::Apply {
            account: "home".into(),
            threads: vec![top],
            action: MailAction::Read { on: true },
            grace: false
        }]
    );
}

#[tokio::test]
async fn every_compose_start_on_a_microsoft_365_account_is_refused() {
    let mut f = m365_fixture().await;
    for key in ['r', 'R', 'f'] {
        press(&mut f, KeyCode::Char(key)).await;
        let line = f.screen.status_line();
        assert!(line.contains(M365_NOT_YET), "{key}: {line}");
        assert!(
            f.screen.compose.is_none() && f.screen.composing.pending.is_none(),
            "{key}"
        );
    }
    // `c` and the `Write from` picker both arrive here with the account's index.
    let effects = f.screen.start_new(1);
    assert!(effects.is_empty() && f.screen.compose.is_none());
    let _ = f.screen.start_new(0);
    assert!(f.screen.compose.is_some(), "the Gmail account still writes");
}

#[tokio::test]
async fn a_microsoft_365_drafts_row_is_refused_to_open_and_to_discard() {
    let mut f = m365_fixture().await;
    let account_id = f.screen.account_ids[1].unwrap();
    let row = DraftsRow {
        account_id,
        kind: DraftsRowKind::Gmail { message: 1 },
        to: Vec::new(),
        subject: "s".into(),
        date: 0,
    };
    let effects = f.screen.open_draft_row(row.clone());
    assert!(effects.is_empty() && f.screen.composing.pending.is_none());
    assert!(f.screen.status_line().contains(M365_NOT_YET));
    f.screen.drafts.pane.apply(vec![row]);
    let effects = f.screen.confirm_discard_row();
    assert!(effects.is_empty() && f.screen.modal.is_none());
    assert!(f.screen.status_line().contains(M365_NOT_YET));
}

#[tokio::test]
async fn the_bar_says_saving_to_microsoft_365_for_that_account() {
    let mut f = m365_fixture().await;
    let mut home = AccountSyncStatus::new("home", "h@example.com", AccountKind::M365);
    home.phase = SyncPhase::Idle;
    home.outbox.saving = 1;
    let event = MailEvent::Daemon(DaemonEvent::Progress(Box::new(home)));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
    let line = f.screen.status_line();
    assert!(line.contains("saving to Microsoft 365…"), "{line}");
}
