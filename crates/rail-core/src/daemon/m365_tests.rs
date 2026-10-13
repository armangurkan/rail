//! A Microsoft 365 account through a real daemon over fake Graph: the first sync, a new
//! message one pass later, read on open reaching Graph as a PATCH and archive as a move to
//! Archive; labels and a crafted move into Sent Items refused with nothing queued (writing:
//! `m365_draft_tests`).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::client::DaemonClient;
use super::{Connector, ServeConfig};
use crate::ops::op::{ActionApply, SyncNow, SyncStatus};
use crate::ops::{
    AccountSyncStatus, Action, ActionApplyRequest, ErrorCode, M365_LABELS, SyncNowRequest,
    SyncStatusRequest,
};
use crate::paths::Paths;
use crate::provider::Provider;
use crate::provider::graph::GraphProvider;
use crate::provider::graph::fixtures::{Graph, api_at, folder, message, recent};
use crate::store::Store;
use crate::test_support::{daemon_context, start_daemon};

pub(super) const CORP: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"60s\"\n\n[[accounts]]\nname = \"corp\"\nkind = \"m365\"\naddress = \"me@corp.example\"\nclient_id = \"cid\"\n";
pub(super) const BOUND: Duration = Duration::from_secs(15);

pub(super) fn graph_factory(base: String) -> crate::sync::ProviderFactory {
    Arc::new(move |_| Ok(Arc::new(GraphProvider::new(api_at(&base))) as Arc<dyn Provider>))
}

pub(super) async fn until(
    client: &DaemonClient,
    done: impl Fn(&AccountSyncStatus) -> bool,
) -> AccountSyncStatus {
    tokio::time::timeout(BOUND, async {
        loop {
            let request = SyncStatusRequest {
                account: Some("corp".into()),
            };
            if let Ok(found) = client.call::<SyncStatus>(&request).await
                && done(&found.accounts[0])
            {
                return found.accounts[0].clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the status was reached in time")
}

fn stored(paths: &Paths) -> Vec<String> {
    let store = Store::open_reader(&paths.db_file()).unwrap();
    let mut stmt = store
        .conn()
        .prepare("SELECT remote_id FROM messages ORDER BY remote_id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn thread(paths: &Paths, remote: &str) -> i64 {
    Store::open_reader(&paths.db_file())
        .unwrap()
        .conn()
        .query_row(
            "SELECT id FROM threads WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

fn folder_id(paths: &Paths, remote: &str) -> i64 {
    Store::open_reader(&paths.db_file())
        .unwrap()
        .conn()
        .query_row(
            "SELECT id FROM folders WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

fn apply(thread: i64, action: Action, token: &str) -> ActionApplyRequest {
    ActionApplyRequest {
        account: "corp".into(),
        threads: vec![thread],
        action,
        grace: false,
        client_token: token.into(),
    }
}

/// Wait until Graph was sent a batch item `sent` accepts.
async fn until_sent(g: &Graph, sent: impl Fn(&serde_json::Value) -> bool) {
    tokio::time::timeout(BOUND, async {
        while !g.batch.requests().iter().any(&sent) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the request reached Graph");
}

/// Inbox, Sent Items, Archive and Deleted Items, all well-known: c1 in the Inbox, and c2
/// one pass later.
async fn mailbox(g: &Graph) {
    g.mount_root(json!([
        folder("f-inbox", "Inbox", 0),
        folder("f-sent", "Sent Items", 0),
        folder("f-arch", "Archive", 0),
        folder("f-del", "Deleted Items", 0),
    ]))
    .await;
    g.mount_well_known(&[
        ("inbox", "f-inbox"),
        ("sentitems", "f-sent"),
        ("archive", "f-arch"),
        ("deleteditems", "f-del"),
    ])
    .await;
    let at = recent(5);
    let first = g.page(vec![message("m1", "c1", "f-inbox", &at)], None, Some("d1"));
    g.mount_initial("f-inbox", first).await;
    let next = g.page(vec![message("m2", "c2", "f-inbox", &at)], None, Some("d2"));
    g.mount_link("d1", next).await;
    g.mount_link("d2", g.page(vec![], None, Some("d2"))).await;
    for id in ["f-sent", "f-arch", "f-del"] {
        let link = format!("d-{id}");
        g.mount_initial(id, g.page(vec![], None, Some(&link))).await;
        g.mount_link(&link, g.page(vec![], None, Some(&link))).await;
    }
}

#[tokio::test]
async fn a_microsoft_365_account_acts_through_graph_and_refuses_labels() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let read = json!({"id": "m1", "isRead": true});
    g.batch.answer("PATCH", "/me/messages/m1", 200, read);
    g.batch
        .answer("POST", "/me/messages/m1/move", 201, json!({"id": "m1"}));
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), CORP).unwrap();
    let (ctx, _) = daemon_context(&paths);
    let ctx = ctx.with_provider_factory(graph_factory(g.base()));
    start_daemon(ctx, ServeConfig::default()).await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();

    let first = until(&client, |s| s.passes_completed >= 1).await;
    assert!(first.last_error.is_none(), "{:?}", first.last_error);
    assert_eq!(stored(&paths), ["m1"]);
    let wake = SyncNowRequest {
        account: Some("corp".into()),
        watch: false,
    };
    client.call::<SyncNow>(&wake).await.unwrap();
    until(&client, |s| s.passes_completed >= 2).await;
    assert_eq!(
        stored(&paths),
        ["m1", "m2"],
        "the new message, one pass later"
    );

    let c1 = thread(&paths, "c1");
    let read = apply(c1, Action::Read { on: true }, "read-1");
    client.call::<ActionApply>(&read).await.unwrap();
    until_sent(&g, |r| {
        r["method"] == "PATCH"
            && r["url"] == "/me/messages/m1"
            && r["body"] == json!({"isRead": true})
    })
    .await;
    let archive = apply(c1, Action::Archive {}, "archive-1");
    client.call::<ActionApply>(&archive).await.unwrap();
    until_sent(&g, |r| {
        r["method"] == "POST"
            && r["url"] == "/me/messages/m1/move"
            && r["body"] == json!({"destinationId": "f-arch"})
    })
    .await;

    let labels = Action::Labels {
        add: vec![folder_id(&paths, "f-arch")],
        remove: vec![],
    };
    let error = client
        .call::<ActionApply>(&apply(c1, labels, "labels-1"))
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (ErrorCode::Unsupported, M365_LABELS)
    );
    let into_sent = Action::Move {
        to: folder_id(&paths, "f-sent"),
    };
    let error = client
        .call::<ActionApply>(&apply(c1, into_sent, "sent-1"))
        .await
        .unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            ErrorCode::InvalidInput,
            "a folder is not one of this account's move targets"
        )
    );
    let reader = Store::open_reader(&paths.db_file()).unwrap();
    let queued: i64 = reader
        .conn()
        .query_row(
            "SELECT count(*) FROM outbox WHERE client_token NOT IN ('read-1', 'archive-1')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(queued, 0, "nothing refused was queued");
}
