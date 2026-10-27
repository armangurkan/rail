//! A `* a` or `* f` answer that comes back after the user left the mailbox or the filter it
//! was asked for is dropped: the selection, the filter and the store are left alone. A child
//! module of `tests/bulk.rs`, whose helpers it uses.

use super::*;

/// Press `key`'s action without running the read it asks for, as a slow store would.
fn press_held(f: &mut Fixture, action: Action) {
    let held = f.screen.act(action);
    assert_eq!(held.len(), 1, "one read asked for");
}

/// What `Store::matching_targets` answers for every thread on screen now, newest first.
fn answer_for_screen(f: &Fixture) -> Vec<(ThreadKey, i64, bool, bool)> {
    f.screen
        .list
        .rows()
        .iter()
        .map(|row| (row.key, Picked::of(row).account_id, false, true))
        .collect()
}

#[tokio::test]
async fn a_select_all_answer_for_a_mailbox_left_is_dropped() {
    let mut f = fixture(3).await;
    let answer = answer_for_screen(&f);
    press_held(&mut f, Action::ListSelectAll);
    let seq = f.screen.bulk.latest_match;
    press(&mut f, KeyCode::Char('2')).await;
    feed(
        &mut f,
        MailEvent::Matched {
            seq,
            result: Ok(answer),
        },
    )
    .await;
    assert!(
        f.screen.actions.selection.is_empty(),
        "the unified inbox's threads are not selected in account 2"
    );
}

#[tokio::test]
async fn a_select_all_answer_for_a_filter_left_is_dropped() {
    let mut f = fixture(3).await;
    let answer = answer_for_screen(&f);
    press_held(&mut f, Action::ListSelectAll);
    let seq = f.screen.bulk.latest_match;
    filter_line(&mut f, "is:starred").await;
    feed(
        &mut f,
        MailEvent::Matched {
            seq,
            result: Ok(answer),
        },
    )
    .await;
    assert!(
        f.screen.actions.selection.is_empty(),
        "the unfiltered threads are not selected under a new filter"
    );
}

#[tokio::test]
async fn a_select_all_answer_for_the_same_view_is_still_applied() {
    let mut f = fixture(3).await;
    let answer = answer_for_screen(&f);
    press_held(&mut f, Action::ListSelectAll);
    let seq = f.screen.bulk.latest_match;
    // Away and back: the list shows what the read was asked for again.
    press(&mut f, KeyCode::Char('2')).await;
    press(&mut f, KeyCode::Char('0')).await;
    feed(
        &mut f,
        MailEvent::Matched {
            seq,
            result: Ok(answer),
        },
    )
    .await;
    assert_eq!(f.screen.actions.selection.len(), 5);
}

/// The cursor thread's sender, as `Store::newest_sender` answers.
fn dana() -> Mailbox {
    Mailbox {
        name: None,
        address: "dana@example.com".to_string(),
    }
}

#[tokio::test]
async fn a_sender_answer_for_a_mailbox_left_is_dropped() {
    let mut f = fixture(3).await;
    press_held(&mut f, Action::ListSelectSender);
    let seq = f.screen.bulk.latest_sender;
    press(&mut f, KeyCode::Char('2')).await;
    let slot = f.screen.slot_text();
    let queries = f.screen.ctx.store.queries();
    feed(
        &mut f,
        MailEvent::Sender {
            seq,
            result: Ok(Some(dana())),
        },
    )
    .await;
    assert_eq!(f.screen.slot_text(), slot, "the filter text is left alone");
    assert!(f.screen.actions.selection.is_empty());
    assert_eq!(f.screen.ctx.store.queries(), queries, "no read follows");
}

#[tokio::test]
async fn a_sender_answer_for_a_filter_left_is_dropped() {
    let mut f = fixture(3).await;
    press_held(&mut f, Action::ListSelectSender);
    let seq = f.screen.bulk.latest_sender;
    filter_line(&mut f, "is:starred").await;
    let slot = f.screen.slot_text();
    let queries = f.screen.ctx.store.queries();
    feed(
        &mut f,
        MailEvent::Sender {
            seq,
            result: Ok(Some(dana())),
        },
    )
    .await;
    assert_eq!(f.screen.slot_text(), slot, "the filter text is left alone");
    assert!(f.screen.actions.selection.is_empty());
    assert_eq!(f.screen.ctx.store.queries(), queries, "no read follows");
}
