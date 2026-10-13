//! Microsoft 365 in Part A: the action keys and every compose start are refused at once with
//! nothing sent, a selection holding one of its threads is refused whole, opening a thread
//! still marks it read, its Drafts rows are refused, and the bar says `saving to Microsoft
//! 365…`.

use rail_core::config::AccountKind;
use rail_core::ops::{AccountSyncStatus, Action as MailAction, M365_NOT_YET};
use rail_core::store::drafts_folder::{DraftsRow, DraftsRowKind};

use super::*;

/// `work` with no threads and `home`, a Microsoft 365 account, with two: the cursor row is
/// home's newest.
async fn m365_fixture() -> Fixture {
    let mut f = fixture(0).await;
    f.screen.accounts[1].kind = AccountKind::M365;
    f
}

#[tokio::test]
async fn the_action_keys_on_a_microsoft_365_thread_are_refused_with_nothing_sent() {
    let mut f = m365_fixture().await;
    for key in ['e', 'd', 's', 'u', 'l', 'm'] {
        press(&mut f, KeyCode::Char(key)).await;
        let line = f.screen.status_line();
        assert!(line.contains(M365_NOT_YET), "{key}: {line}");
        assert!(f.screen.modal.is_none(), "{key}: no prompt and no picker");
    }
    assert!(applies(&f).is_empty());
}

#[tokio::test]
async fn a_selection_holding_a_microsoft_365_thread_is_refused_whole() {
    let mut f = fixture(1).await;
    f.screen.accounts[1].kind = AccountKind::M365;
    // The unified inbox: wm0, hm1, hm0 newest first; select work's and home's newest.
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char('e')).await;
    assert!(f.screen.status_line().contains(M365_NOT_YET));
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
