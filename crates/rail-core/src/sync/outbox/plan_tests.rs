use super::plan::{Plan, plan};
use crate::ops::Action;
use crate::provider::Change;
use crate::store::Store;
use crate::store::outbox::{NewAction, OutboxOutcome, OutboxWrite};

fn queued(action: Action) -> (Store, i64) {
    let mut store = Store::open_in_memory().unwrap();
    store.conn().execute_batch(
        "INSERT INTO accounts (id, name, kind, address) VALUES (1, 'work', 'gmail', 'me@example.com');
         INSERT INTO folders (id, account_id, remote_id, name, kind) VALUES
           (1, 1, 'INBOX', 'Inbox', 'inbox'), (2, 1, 'Label_1', 'Projects', 'user');
         INSERT INTO threads (id, account_id, remote_id) VALUES (1, 1, 't1');
         INSERT INTO messages (id, account_id, thread_id, remote_id) VALUES (1, 1, 1, 'm1');
         INSERT INTO message_folders (message_id, folder_id) VALUES (1, 1);"
    ).unwrap();
    let result = store
        .outbox_write(
            &OutboxWrite::Enqueue(NewAction {
                account_id: 1,
                action,
                threads: vec![1],
                not_before: 10,
                client_token: "test".into(),
            }),
            0,
        )
        .unwrap();
    let OutboxOutcome::Enqueued { rows, .. } = result else {
        panic!("{result:?}");
    };
    (store, rows[0])
}

#[test]
fn provider_changes_follow_the_original_folder_and_refuse_a_deleted_one() {
    let cases = [
        (
            Action::Move { to: 2 },
            Change::Move {
                to: "Label_1".into(),
            },
        ),
        (
            Action::Labels {
                add: vec![2],
                remove: vec![],
            },
            Change::Labels {
                add: vec!["Label_1".into()],
                remove: vec![],
            },
        ),
        (
            Action::Labels {
                add: vec![],
                remove: vec![2],
            },
            Change::Labels {
                add: vec![],
                remove: vec!["Label_1".into()],
            },
        ),
    ];
    for (action, expected) in cases {
        let (store, id) = queued(action);
        store.conn().execute_batch("UPDATE folders SET remote_id = 'Label_other' WHERE id = 2;
            INSERT INTO folders (id, account_id, remote_id, name, kind) VALUES (3, 1, 'Label_1', 'Projects', 'user');").unwrap();
        let row = store.outbox_row(id).unwrap().unwrap();
        let Plan::Send { change, .. } = plan(&store, &row).unwrap() else {
            panic!("expected send");
        };
        assert_eq!(change, expected);
        store
            .conn()
            .execute("DELETE FROM folders WHERE id = 3", [])
            .unwrap();
        assert_eq!(plan(&store, &row).unwrap(), Plan::LabelGone);
    }
}
