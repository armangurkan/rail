//! `l` and `m` against the seeded store (Inbox, Projects, Sent per account): the list, the
//! filter, toggling, applying, cancelling, the cross-account refusal, and snapshots.

use rail_core::ops::Action as MailAction;

use super::*;

fn projects(f: &Fixture) -> i64 {
    writer(f)
        .conn()
        .query_row(
            "SELECT id FROM folders WHERE account_id = 1 AND remote_id = 'Label_1'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn l_lists_the_accounts_labels_and_enter_adds_the_ticked_one() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('l')).await;
    assert!(
        matches!(f.screen.modal, Some(Modal::Picker(_))),
        "Projects is the only user label"
    );
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Enter).await;
    let top = thread_ids(&f)[0];
    assert_eq!(
        applies(&f),
        [FakeCall::Apply {
            account: "work".into(),
            threads: vec![top],
            action: MailAction::Labels {
                add: vec![projects(&f)],
                remove: vec![]
            },
            grace: true
        }]
    );
    assert!(f.screen.modal.is_none());
}

#[tokio::test]
async fn m_moves_and_esc_cancels_without_a_call() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('m')).await;
    press(&mut f, KeyCode::Esc).await;
    assert!(applies(&f).is_empty());
    press(&mut f, KeyCode::Char('m')).await;
    for c in "proj".chars() {
        press(&mut f, KeyCode::Char(c)).await;
    }
    press(&mut f, KeyCode::Enter).await;
    assert!(
        matches!(&applies(&f)[..], [FakeCall::Apply { action: MailAction::Move { to }, .. }] if *to == projects(&f))
    );
}

#[tokio::test]
async fn enter_with_nothing_changed_sends_nothing() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('l')).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(applies(&f).is_empty());
    assert!(
        f.screen.status_line().contains("No labels changed"),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn the_pickers_refuse_a_selection_across_accounts() {
    let mut f = fixture(2).await;
    for _ in 0..4 {
        press(&mut f, KeyCode::Char(' ')).await;
    }
    for picker in ['l', 'm'] {
        press(&mut f, KeyCode::Char(picker)).await;
        assert!(f.screen.modal.is_none());
        assert!(
            f.screen
                .status_line()
                .contains("select threads from one account"),
            "{}",
            f.screen.status_line()
        );
    }
}

#[tokio::test]
async fn the_pickers_at_90_and_150_columns() {
    for (key, name) in [('l', "labels"), ('m', "move")] {
        for width in [90, 150] {
            let mut f = fixture(3).await;
            press(&mut f, KeyCode::Char(key)).await;
            let shot = crate::tui::test_util::snapshot(width, 24, |frame| {
                f.screen.view(frame, &crate::tui::theme::Theme::default())
            });
            insta::assert_snapshot!(format!("picker_{name}_{width}"), shot);
        }
    }
}
