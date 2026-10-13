//! The engine with the Graph provider against fake Graph, over a real store and writer: the
//! first load shown as a backfill with its count and no total, a resync that keeps what Graph
//! still lists (with its body) and sweeps the rest only once caught up, and a grown window that
//! starts every folder over with the new filter.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::Notify;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::engine::{Engine, EngineParts};
use super::status::StatusBoard;
use super::writer::{WriterHandle, WriterTask, spawn_writer};
use crate::config::{AccountConfig, AccountKind, SyncSettings};
use crate::ops::{AccountSyncStatus, SyncPhase};
use crate::provider::graph::GraphProvider;
use crate::provider::graph::fixtures::{Graph, ROOT, folder, message, recent};
use crate::store::{Store, SyncStateRow};
use crate::test_support::sample_account;

const ADDRESS: &str = "me@corp.example";

struct Harness {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    writer: WriterHandle,
    _task: WriterTask,
    board: StatusBoard,
}

fn harness() -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("rail.db");
    Store::open(&db)
        .unwrap()
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('corp', 'm365', ?1)",
            [ADDRESS],
        )
        .unwrap();
    let (writer, task) = spawn_writer(&db).unwrap();
    Harness {
        _tmp: tmp,
        db,
        writer,
        _task: task,
        board: StatusBoard::default(),
    }
}

/// An engine over `provider` whose clock says `now` (each pass gets its own, later, clock).
fn engine(h: &Harness, provider: GraphProvider, history_days: u32, now: i64) -> Engine {
    let account = AccountConfig {
        name: "corp".into(),
        address: ADDRESS.into(),
        kind: AccountKind::M365,
        ..sample_account()
    };
    if h.board.get(ADDRESS).is_none() {
        h.board
            .insert(AccountSyncStatus::new("corp", ADDRESS, AccountKind::M365));
    }
    Engine::new(EngineParts {
        account,
        account_id: 1,
        settings: SyncSettings {
            history_days,
            body_cache_days: 30,
            poll_interval: Duration::from_secs(60),
        },
        provider: Arc::new(provider),
        writer: h.writer.clone(),
        reader: Store::open_reader(&h.db).unwrap(),
        board: h.board.clone(),
        wake: Arc::new(Notify::new()),
        clock: Arc::new(move || now),
    })
}

fn stored(h: &Harness) -> Vec<String> {
    let store = Store::open_reader(&h.db).unwrap();
    let mut stmt = store
        .conn()
        .prepare("SELECT remote_id FROM messages ORDER BY remote_id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn state(h: &Harness) -> SyncStateRow {
    Store::open_reader(&h.db).unwrap().sync_state(1).unwrap()
}

fn body_of(h: &Harness, id: &str) -> Option<String> {
    Store::open_reader(&h.db)
        .unwrap()
        .conn()
        .query_row(
            "SELECT body_text FROM messages WHERE remote_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

async fn inbox(g: &Graph) {
    g.mount_root(json!([folder("f-inbox", "Inbox", 0)])).await;
    g.mount_well_known(&[("inbox", "f-inbox")]).await;
}

#[tokio::test]
async fn the_first_load_shows_as_a_backfill_with_its_count_and_no_total() {
    let g = Graph::start().await;
    inbox(&g).await;
    let at = recent(60);
    let first = g.page(vec![message("m1", "c1", "f-inbox", &at)], Some("p2"), None);
    g.mount_initial("f-inbox", first).await;
    let rest = g.page(vec![message("m2", "c2", "f-inbox", &at)], None, Some("d1"));
    g.mount_link("p2", rest).await;
    let h = harness();
    let phases = Arc::new(Mutex::new(Vec::new()));
    let seen = phases.clone();
    h.board.subscribe(Box::new(move |s| {
        seen.lock().unwrap().push(s.phase);
        true
    }));
    let now = crate::auth::unix_now();
    engine(&h, g.provider(), 0, now).pass().await.unwrap();
    assert_eq!(stored(&h), ["m1", "m2"]);
    let status = h.board.get(ADDRESS).unwrap();
    assert_eq!(
        (status.backfill_fetched, status.backfill_estimate),
        (2, None)
    );
    let phases = phases.lock().unwrap();
    assert!(phases.contains(&SyncPhase::Backfill));
    assert!(
        !phases.contains(&SyncPhase::Delta),
        "the first load never shows as a delta"
    );
    let state = state(&h);
    assert!(state.cursor.is_some());
    assert_eq!(
        state.backfill_started_at, None,
        "the sweep ran once caught up"
    );
}

#[tokio::test]
async fn a_resync_keeps_what_graph_still_lists_with_its_body_and_drops_the_rest() {
    let g = Graph::start().await;
    inbox(&g).await;
    let at = recent(60);
    let both = vec![
        message("m1", "c1", "f-inbox", &at),
        message("m2", "c2", "f-inbox", &at),
    ];
    g.mount_initial("f-inbox", g.page(both, None, Some("d1")))
        .await;
    let body = json!({"id": "m1", "hasAttachments": false,
                      "body": {"contentType": "text", "content": "kept body"}});
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/m1")))
        .and(query_param("$select", "id,body,hasAttachments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&g.server)
        .await;
    let h = harness();
    let now = crate::auth::unix_now();
    engine(&h, g.provider(), 0, now).pass().await.unwrap();
    assert_eq!(stored(&h), ["m1", "m2"]);
    assert_eq!(body_of(&h, "m1").as_deref(), Some("kept body"));
    // Graph forgot the link, and m2 is gone meanwhile.
    g.reset().await;
    inbox(&g).await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/links/d1")))
        .respond_with(ResponseTemplate::new(410))
        .mount(&g.server)
        .await;
    let again = g.page(vec![message("m1", "c1", "f-inbox", &at)], None, Some("d2"));
    g.mount_initial("f-inbox", again).await;
    engine(&h, g.provider(), 0, now + 60).pass().await.unwrap();
    assert_eq!(stored(&h), ["m1"]);
    assert_eq!(
        body_of(&h, "m1").as_deref(),
        Some("kept body"),
        "never deleted, never fetched again"
    );
    assert_eq!(state(&h).backfill_started_at, None);
}

#[tokio::test]
async fn a_grown_window_starts_every_folder_over_with_the_new_filter() {
    let g = Graph::start().await;
    inbox(&g).await;
    let (fresh, old) = (recent(60), recent(20 * 24 * 60));
    let first = g.page(
        vec![message("m1", "c1", "f-inbox", &fresh)],
        None,
        Some("d1"),
    );
    g.mount_initial("f-inbox", first).await;
    let h = harness();
    let now = crate::auth::unix_now();
    engine(&h, g.provider_with_days(7), 7, now)
        .pass()
        .await
        .unwrap();
    assert_eq!(stored(&h), ["m1"]);
    g.reset().await;
    inbox(&g).await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/links/d1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(g.page(vec![], None, Some("d1"))))
        .expect(0)
        .mount(&g.server)
        .await;
    let all = vec![
        message("m1", "c1", "f-inbox", &fresh),
        message("m-old", "c2", "f-inbox", &old),
    ];
    g.mount_initial("f-inbox", g.page(all, None, Some("d2")))
        .await;
    engine(&h, g.provider_with_days(30), 30, now + 60)
        .pass()
        .await
        .unwrap();
    assert_eq!(stored(&h), ["m-old", "m1"]);
}

/// Page `n` of a twelve-page inbox walk of one message each (`m01` to `m12`): page 1 is the
/// initial delta, the last gives the delta link `d2`.
fn walk_page(g: &Graph, n: usize, at: &str) -> serde_json::Value {
    let item = message(&format!("m{n:02}"), &format!("c{n}"), "f-inbox", at);
    match n {
        12 => g.page(vec![item], None, Some("d2")),
        _ => g.page(vec![item], Some(&format!("p{}", n + 1)), None),
    }
}

#[tokio::test]
async fn a_first_load_over_two_passes_and_a_restart_sweeps_once_at_the_end_from_its_first_start() {
    let g = Graph::start().await;
    inbox(&g).await;
    let at = recent(60);
    let before = g.page(
        vec![message("m-gone", "c0", "f-inbox", &at)],
        None,
        Some("d1"),
    );
    g.mount_initial("f-inbox", before).await;
    let h = harness();
    let now = crate::auth::unix_now();
    engine(&h, g.provider(), 0, now).pass().await.unwrap();
    assert_eq!(stored(&h), ["m-gone"]);
    // Graph forgets the link; the reload walks twelve pages, ten to a call, and the pass
    // that began it fails on page 11 (a stand-in for the daemon stopping).
    g.reset().await;
    inbox(&g).await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/links/d1")))
        .respond_with(ResponseTemplate::new(410))
        .mount(&g.server)
        .await;
    g.mount_initial("f-inbox", walk_page(&g, 1, &at)).await;
    for n in 2..=10 {
        g.mount_link(&format!("p{n}"), walk_page(&g, n, &at)).await;
    }
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/links/p11")))
        .respond_with(ResponseTemplate::new(400))
        .mount(&g.server)
        .await;
    let first_start = now + 60;
    assert!(
        engine(&h, g.provider(), 0, first_start)
            .pass()
            .await
            .is_err()
    );
    let midway = stored(&h);
    assert!(
        midway.contains(&"m-gone".to_string()),
        "no sweep yet: {midway:?}"
    );
    assert!(midway.contains(&"m10".to_string()), "{midway:?}");
    let mid = state(&h);
    assert_eq!(mid.backfill_started_at, Some(first_start));
    assert!(mid.cursor.is_some());
    // A fresh engine, later, finishes the load.
    g.reset().await;
    inbox(&g).await;
    for n in 11..=12 {
        g.mount_link(&format!("p{n}"), walk_page(&g, n, &at)).await;
    }
    engine(&h, g.provider(), 0, now + 120).pass().await.unwrap();
    let expected: Vec<String> = (1..=12).map(|n| format!("m{n:02}")).collect();
    assert_eq!(
        stored(&h),
        expected,
        "m-gone swept; pages 1 to 10, seen only in the first pass, kept: the first start rules"
    );
    assert_eq!(state(&h).backfill_started_at, None);
}
