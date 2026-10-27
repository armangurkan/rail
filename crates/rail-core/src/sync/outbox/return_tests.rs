//! A snooze's return through the worker and the mock (#106): refused, kept through a sync,
//! retried from `:errors`; the retry sends only the recorded messages not in Trash or Spam now.

use rusqlite::params;

use super::tests::{Rig, rig};
use crate::auth::unix_now;
use crate::ops::Action;
use crate::provider::mock::{Call, envelope};
use crate::provider::{Change, Envelope, ProviderError};
use crate::store::Store;
use crate::store::outbox::{OutboxOutcome, OutboxWrite};
use crate::sync::batch::Batch;

fn in_folders(id: &str, folders: &[&str]) -> Envelope {
    Envelope {
        folders: folders.iter().map(|f| (*f).to_string()).collect(),
        ..envelope(id, "t1", 1_000)
    }
}

/// t1 holds m1 and m3, both archived by an active snooze that is due.
async fn due_snooze(rig: &Rig) {
    let batch = Batch {
        upserts: vec![in_folders("m1", &[]), in_folders("m3", &[])],
        ..Batch::new(1)
    };
    rig.writer.write(batch).await.unwrap();
    let store = Store::open(&rig.db).unwrap();
    let conn = store.conn();
    conn.execute(
        "INSERT INTO snoozes (account_id, thread_remote, until, created_at) VALUES (1, 't1', ?1, ?1)",
        params![unix_now() - 1],
    )
    .unwrap();
    let snooze = conn.last_insert_rowid();
    for m in ["m1", "m3"] {
        conn.execute(
            "INSERT INTO snooze_messages (snooze_id, message_remote) VALUES (?1, ?2)",
            params![snooze, m],
        )
        .unwrap();
    }
}

/// The messages of every return the mock was asked for, in order.
fn returns(rig: &Rig) -> Vec<Vec<String>> {
    rig.mock.with(|m| {
        m.applied
            .iter()
            .filter_map(|(_, c)| match c {
                Change::Unsnooze { messages } => Some(messages.clone()),
                _ => None,
            })
            .collect()
    })
}

#[tokio::test]
async fn a_retried_return_leaves_a_message_trashed_since_in_the_trash() {
    let rig = rig().await;
    due_snooze(&rig).await;
    let worker = rig.worker();
    rig.mock
        .fail_next(Call::Modify, ProviderError::permanent("refused"));
    worker.step().await;
    let failed = rig.store().failed_rows(1).unwrap();
    assert!(matches!(failed[0].action, Action::Unsnooze { .. }));
    // The user trashes m1 on their phone; m3 stays archived.
    let batch = Batch {
        upserts: vec![in_folders("m1", &["TRASH"]), in_folders("m3", &[])],
        ..Batch::new(1)
    };
    rig.writer.write(batch).await.unwrap();
    let kept = rig.store().failed_rows(1).unwrap();
    assert_eq!(kept.len(), 1, "m3 is still owed its return");
    let retry = OutboxWrite::Retry {
        id: kept[0].id,
        client_token: kept[0].client_token.clone(),
    };
    let retried = rig.writer.outbox(retry).await.unwrap();
    assert_eq!(retried, OutboxOutcome::Retried(true));
    for _ in 0..3 {
        worker.step().await;
    }
    assert_eq!(
        returns(&rig),
        [
            vec!["m1".to_string(), "m3".to_string()],
            vec!["m3".to_string()]
        ],
        "the retry leaves m1 in the trash"
    );
}
