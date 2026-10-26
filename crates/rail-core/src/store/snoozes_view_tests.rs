//! What the views show of snoozes: the Snoozed mailbox lists the threads a snooze holds (waiting
//! for its archive, or archived until its time), the sidebar counts them, and the reader's thread
//! says until when; a failed or woken snooze holds nothing.

use super::Store;
use super::apply_local_tests::{seeded, thread};
use super::filter::parse;
use super::outbox::{Failure, OutboxOutcome, OutboxWrite};
use super::snoozes_tests::{HOUR, NOW, in_folders, send_next, snooze, sync, wake};
use super::view::{AccountFilter, ListQuery, PageAt, Scope};

pub(super) fn snoozed(store: &Store) -> Vec<i64> {
    let query = ListQuery {
        scope: Scope::Snoozed { accounts: vec![1] },
        filter: parse(""),
        account: AccountFilter::Any,
        utc_offset: 0,
    };
    let page = store.thread_page(&query, PageAt::First).unwrap();
    let mut ids: Vec<i64> = page.rows.iter().map(|r| r.key.thread_id).collect();
    ids.sort_unstable();
    ids
}

fn counted(store: &Store) -> u64 {
    store.sidebar_counts(&[1]).unwrap().unified.snoozed
}

pub(super) fn until(store: &Store, thread: i64) -> Option<i64> {
    store.thread_detail(thread).unwrap().unwrap().snoozed_until
}

#[test]
fn a_snooze_holds_its_thread_only_while_some_of_its_mail_is_still_archived() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    // Trashed on another device, m1 first, then m2.
    sync(
        &mut store,
        vec![in_folders("m1", "t1", 1_000, &["TRASH"])],
        NOW + 20,
    );
    assert_eq!(snoozed(&store), [t1], "m2 is still archived");
    sync(
        &mut store,
        vec![in_folders("m2", "t1", 2_000, &["TRASH"])],
        NOW + 30,
    );
    assert_eq!(
        snoozed(&store),
        Vec::<i64>::new(),
        "nothing left to bring back"
    );
    assert_eq!(counted(&store), 0);
    assert_eq!(until(&store, t1), None);
}

#[test]
fn the_snoozed_mailbox_lists_what_a_snooze_holds_and_the_reader_says_until_when() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    snooze(&mut store, vec![t2], NOW + 2 * HOUR, "b");
    let mut both = vec![t1, t2];
    both.sort_unstable();
    assert_eq!(snoozed(&store), both, "an active one and a pending one");
    assert_eq!(counted(&store), 2);
    assert_eq!(
        (until(&store, t1), until(&store, t2)),
        (Some(NOW + HOUR), Some(NOW + 2 * HOUR))
    );
}

#[test]
fn a_failed_or_woken_snooze_holds_nothing() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    snooze(&mut store, vec![t2], NOW + 2 * HOUR, "b");
    let row = match store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 20)
        .unwrap()
    {
        OutboxOutcome::Claimed(Some(row)) => row.id,
        other => panic!("{other:?}"),
    };
    let refused = Failure::Permanent("Gmail refused the change".into());
    store
        .outbox_write(
            &OutboxWrite::Fail {
                id: row,
                failure: refused,
            },
            NOW + 20,
        )
        .unwrap();
    assert_eq!(snoozed(&store), [t1], "the failed one is back in the inbox");
    assert_eq!((counted(&store), until(&store, t2)), (1, None));
    wake(&mut store, NOW + HOUR);
    assert_eq!(snoozed(&store), Vec::<i64>::new(), "the woken one is back");
    assert_eq!((counted(&store), until(&store, t1)), (0, None));
}
