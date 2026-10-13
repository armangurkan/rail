//! A Microsoft 365 account through a real daemon over fake Graph: the first sync, a new
//! message one pass later, read on open reaching Graph as a PATCH, and every other action and
//! every draft refused with nothing queued.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::client::DaemonClient;
use super::{Connector, ServeConfig};
use crate::ops::op::{ActionApply, DraftSave, SyncNow, SyncStatus};
use crate::ops::{
    AccountSyncStatus, Action, ActionApplyRequest, DraftSaveRequest, ErrorCode, M365_NOT_YET,
    SyncNowRequest, SyncStatusRequest,
};
use crate::paths::Paths;
use crate::provider::Provider;
use crate::provider::graph::GraphProvider;
use crate::provider::graph::fixtures::{Graph, api_at, folder, message, recent};
use crate::store::Store;
use crate::store::drafts::DraftKind;
use crate::test_support::{daemon_context, start_daemon};

const CORP: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"60s\"\n\n[[accounts]]\nname = \"corp\"\nkind = \"m365\"\naddress = \"me@corp.example\"\nclient_id = \"cid\"\n";
const BOUND: Duration = Duration::from_secs(15);

fn graph_factory(base: String) -> crate::sync::ProviderFactory {
    Arc::new(move |_| Ok(Arc::new(GraphProvider::new(api_at(&base))) as Arc<dyn Provider>))
}

async fn until(
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

#[tokio::test]
async fn a_microsoft_365_account_syncs_marks_read_on_open_and_refuses_the_rest() {
    let g = Graph::start().await;
    g.mount_root(json!([folder("f-inbox", "Inbox", 0)])).await;
    g.mount_well_known(&[("inbox", "f-inbox")]).await;
    let at = recent(5);
    let first = g.page(vec![message("m1", "c1", "f-inbox", &at)], None, Some("d1"));
    g.mount_initial("f-inbox", first).await;
    let next = g.page(vec![message("m2", "c2", "f-inbox", &at)], None, Some("d2"));
    g.mount_link("d1", next).await;
    g.mount_link("d2", g.page(vec![], None, Some("d2"))).await;
    g.batch.answer(
        "PATCH",
        "/me/messages/m1",
        200,
        json!({"id": "m1", "isRead": true}),
    );
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

    let read = ActionApplyRequest {
        account: "corp".into(),
        threads: vec![thread(&paths, "c1")],
        action: Action::Read { on: true },
        grace: false,
        client_token: "read-1".into(),
    };
    client.call::<ActionApply>(&read).await.unwrap();
    tokio::time::timeout(BOUND, async {
        let patched = |r: &serde_json::Value| {
            r["method"] == "PATCH"
                && r["url"] == "/me/messages/m1"
                && r["body"] == json!({"isRead": true})
        };
        while !g.batch.requests().iter().any(patched) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the PATCH reached Graph");

    let refused = [
        Action::Archive {},
        Action::Trash {},
        Action::Star { on: true },
    ];
    for (n, action) in refused.into_iter().enumerate() {
        let request = ActionApplyRequest {
            account: "corp".into(),
            threads: vec![thread(&paths, "c1")],
            action,
            grace: true,
            client_token: format!("refused-{n}"),
        };
        let error = client.call::<ActionApply>(&request).await.unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (ErrorCode::Unsupported, M365_NOT_YET)
        );
    }
    let draft = DraftSaveRequest {
        account: "corp".into(),
        draft: None,
        kind: DraftKind::New,
        reply_to: None,
        to: vec!["ann@example.com".into()],
        cc: vec![],
        bcc: vec![],
        subject: "hello".into(),
        body: "hi".into(),
        attachments: vec![],
        client_token: "draft-1".into(),
    };
    let error = client.call::<DraftSave>(&draft).await.unwrap_err();
    assert_eq!(error.message, M365_NOT_YET);
    let queued: i64 = Store::open_reader(&paths.db_file())
        .unwrap()
        .conn()
        .query_row(
            "SELECT count(*) FROM outbox WHERE client_token != 'read-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(queued, 0, "nothing refused was queued");
}
