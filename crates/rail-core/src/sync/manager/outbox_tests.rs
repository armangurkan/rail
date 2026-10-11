//! The outbox worker beside the engine: a waiting row is sent, and shutdown keeps its deadline.

use std::time::Duration;

use super::tests::{context, until};
use crate::provider::mock::{Call, MockProvider, envelope};

#[test]
fn the_outbox_shares_the_start_backoff() {
    let mins = |m: u64| Duration::from_secs(60 * m);
    assert_eq!(
        [1, 2, 3, 4, 20].map(super::parts::retry_delay),
        [mins(1), mins(5), mins(15), mins(15), mins(15)]
    );
}

#[tokio::test]
async fn a_waiting_row_is_sent_by_the_accounts_worker_and_shutdown_keeps_its_deadline() {
    use crate::ops::Action;
    use crate::store::Store;
    use crate::store::outbox::{NewAction, OutboxWrite};
    let tmp = tempfile::tempdir().unwrap();
    let mock = MockProvider::new();
    mock.seed(envelope("m1", "t1", crate::auth::unix_now() * 1000 - 1_000));
    let ctx = context(&tmp, &mock);
    ctx.sync.reconcile(&ctx).await;
    until(&ctx, |s| s.passes_completed > 0).await;
    let db = ctx.paths.db_file();
    let thread: i64 = Store::open_reader(&db)
        .unwrap()
        .conn()
        .query_row("SELECT id FROM threads LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let writer = ctx.sync.writer(&ctx).await.unwrap();
    let new = NewAction {
        account_id: 1,
        action: Action::Star { on: true },
        threads: vec![thread],
        not_before: 0,
        client_token: "t".into(),
    };
    writer.outbox(OutboxWrite::Enqueue(new)).await.unwrap();
    drop(writer);
    ctx.sync.logged_in(&ctx, "me@example.com").await; // also wakes the worker
    tokio::time::timeout(Duration::from_secs(10), async {
        while mock.with(|m| m.modifies.is_empty()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the worker sent the row");
    // A second row whose call hangs: shutdown must not wait for it.
    mock.with(|m| m.action_delay = Duration::from_secs(60));
    let writer = ctx.sync.writer(&ctx).await.unwrap();
    let again = NewAction {
        account_id: 1,
        action: Action::Star { on: false },
        threads: vec![thread],
        not_before: 0,
        client_token: "u".into(),
    };
    writer.outbox(OutboxWrite::Enqueue(again)).await.unwrap();
    drop(writer);
    ctx.sync.logged_in(&ctx, "me@example.com").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while mock.calls().iter().filter(|c| **c == Call::Modify).count() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the second call started");
    let started = std::time::Instant::now();
    ctx.sync.shutdown(Duration::from_secs(2)).await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "within the grace"
    );
}
