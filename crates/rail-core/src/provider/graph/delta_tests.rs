//! The delta against fake Graph: an initial sync over three folders resumed mid-way, read and
//! flag changes, a partial update, removals checked before they are deleted, moves in one call
//! and across calls, an expired link, folders added and gone, corrupt and oversized cursors,
//! every folder polled past the page budget, the history cutoff, the profile, and envelopes in
//! folders Rail does not sync.

use serde_json::{Map, Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::cursor::{GraphCursor, MAX_CURSOR_BYTES, State};
use super::dates::iso;
use super::fixtures::{Graph, ROOT, folder, message, removed};
use crate::provider::{ChangeSet, Provider, ProviderError, Relabel, SyncCursor, Window};

const AT: &str = "2026-10-12T08:30:15Z";

/// Inbox, Archive and Projects at the root; Inbox and Archive well-known.
async fn mailbox(g: &Graph) {
    g.mount_root(json!([
        folder("f-inbox", "Inbox", 0),
        folder("f-arch", "Archive", 0),
        folder("f-proj", "Projects", 0),
    ]))
    .await;
    g.mount_well_known(&[("inbox", "f-inbox"), ("archive", "f-arch")])
        .await;
}

/// A cursor whose folders are `(id, link name, state)`; no link: not started.
fn cursor(g: &Graph, folders: &[(&str, Option<&str>, &str)]) -> SyncCursor {
    let mut map = Map::new();
    for (id, link, state) in folders {
        let entry = match link {
            Some(name) => json!({"link": g.link_to(name), "state": state}),
            None => json!({"state": state}),
        };
        map.insert((*id).to_string(), entry);
    }
    SyncCursor(json!({"v": 1, "folders": map}).to_string())
}

/// Every folder of the usual mailbox caught up at its link `d-<id>`.
fn caught_up(g: &Graph) -> SyncCursor {
    cursor(
        g,
        &[
            ("f-inbox", Some("d-f-inbox"), "caught_up"),
            ("f-arch", Some("d-f-arch"), "caught_up"),
            ("f-proj", Some("d-f-proj"), "caught_up"),
        ],
    )
}

/// A caught-up folder's quiet answer: nothing new, the same link for the next round.
async fn quiet(g: &Graph, ids: &[&str]) {
    for id in ids {
        let link = format!("d-{id}");
        g.mount_link(&link, g.page(vec![], None, Some(&link))).await;
    }
}

fn added(set: &ChangeSet) -> Vec<(&str, &str)> {
    set.added
        .iter()
        .map(|e| {
            let folder = e.folders.first().map_or("", String::as_str);
            (e.remote_id.as_str(), folder)
        })
        .collect()
}

fn state_of(set: &ChangeSet, id: &str) -> State {
    GraphCursor::decode(&set.cursor).unwrap().folders[id].state
}

#[tokio::test]
async fn an_initial_sync_over_three_folders_resumes_where_it_stopped() {
    let g = Graph::start().await;
    mailbox(&g).await;
    // Inbox: its initial page and seven more; Archive and Projects: two each. Twelve pages,
    // ten a call.
    let first = g.page(vec![message("i0", "c", "f-inbox", AT)], Some("i1"), None);
    g.mount_initial("f-inbox", first).await;
    for n in 1..8 {
        let following = format!("i{}", n + 1);
        let (next, delta) = if n == 7 {
            (None, Some("d-f-inbox"))
        } else {
            (Some(following.as_str()), None)
        };
        let item = message(&format!("i{n}"), "c", "f-inbox", AT);
        g.mount_link(&format!("i{n}"), g.page(vec![item], next, delta))
            .await;
    }
    for id in ["f-arch", "f-proj"] {
        let (one, two, delta) = (format!("{id}-1"), format!("{id}-2"), format!("d-{id}"));
        let initial = g.page(vec![message(&one, "c", id, AT)], Some(&one), None);
        g.mount_initial(id, initial).await;
        let rest = g.page(vec![message(&two, "c", id, AT)], None, Some(&delta));
        g.mount_link(&one, rest).await;
    }
    let provider = g.provider();
    let start = provider.profile().await.unwrap().cursor;
    let one = provider.changes(&start).await.unwrap();
    assert!(one.more);
    assert_eq!(
        one.added.len(),
        10,
        "the inbox's eight pages, then archive's two"
    );
    assert_eq!(state_of(&one, "f-arch"), State::CaughtUp);
    let paused = GraphCursor::decode(&one.cursor).unwrap();
    assert_eq!(paused.folders["f-proj"].link, None, "not started yet");
    let two = provider.changes(&one.cursor).await.unwrap();
    assert!(!two.more);
    assert_eq!(
        added(&two),
        [("f-proj-1", "f-proj"), ("f-proj-2", "f-proj")]
    );
    for id in ["f-inbox", "f-arch", "f-proj"] {
        assert_eq!(state_of(&two, id), State::CaughtUp, "{id}");
    }
}

#[tokio::test]
async fn read_and_flag_changes_arrive_as_updates() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let mut changed = message("m1", "c1", "f-inbox", AT);
    changed["isRead"] = json!(true);
    changed["flag"] = json!({"flagStatus": "flagged"});
    g.mount_link("d-f-inbox", g.page(vec![changed], None, Some("d-f-inbox")))
        .await;
    quiet(&g, &["f-arch", "f-proj"]).await;
    let set = g.provider().changes(&caught_up(&g)).await.unwrap();
    assert_eq!(added(&set), [("m1", "f-inbox")]);
    assert!(set.added[0].is_read && set.added[0].is_starred);
    assert!(!set.more && set.deleted.is_empty());
}

#[tokio::test]
async fn an_update_with_only_is_read_changes_the_flag_and_nothing_else() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let update = json!({"@odata.type": "#microsoft.graph.message", "id": "m1", "isRead": true});
    g.mount_link("d-f-inbox", g.page(vec![update], None, Some("d-f-inbox")))
        .await;
    quiet(&g, &["f-arch", "f-proj"]).await;
    let set = g.provider().changes(&caught_up(&g)).await.unwrap();
    assert!(
        set.added.is_empty(),
        "no envelope from an update without one"
    );
    let read = Relabel {
        remote_id: "m1".into(),
        read: Some(true),
        ..Relabel::default()
    };
    assert_eq!(set.relabeled, [read]);
}

#[tokio::test]
async fn a_removal_with_no_add_is_checked_then_deleted() {
    let g = Graph::start().await;
    mailbox(&g).await;
    g.mount_link(
        "d-f-inbox",
        g.page(vec![removed("m1")], None, Some("d-f-inbox")),
    )
    .await;
    quiet(&g, &["f-arch", "f-proj"]).await;
    let set = g.provider().changes(&caught_up(&g)).await.unwrap();
    assert_eq!(set.deleted, ["m1"]);
    let asked: Vec<String> = g
        .batch
        .requests()
        .iter()
        .map(|r| {
            r["url"]
                .as_str()
                .unwrap()
                .split('?')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(asked, ["/me/messages/m1"], "checked first");
}

#[tokio::test]
async fn a_move_within_one_call_is_one_upsert_in_the_new_folder_either_way_round() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let inbox = vec![removed("m1"), message("m2", "c2", "f-inbox", AT)];
    g.mount_link("d-f-inbox", g.page(inbox, None, Some("d-f-inbox")))
        .await;
    let archive = vec![message("m1", "c1", "f-arch", AT), removed("m2")];
    g.mount_link("d-f-arch", g.page(archive, None, Some("d-f-arch")))
        .await;
    quiet(&g, &["f-proj"]).await;
    let set = g.provider().changes(&caught_up(&g)).await.unwrap();
    assert_eq!(added(&set), [("m1", "f-arch"), ("m2", "f-inbox")]);
    assert!(set.deleted.is_empty());
    assert_eq!(g.batch.posts(), 0, "nothing left to check");
}

#[tokio::test]
async fn a_removal_after_its_add_in_an_earlier_call_keeps_the_message_where_it_went() {
    let g = Graph::start().await;
    mailbox(&g).await;
    // An earlier call brought m1 into the inbox; now only Archive is due, and says it left.
    let start = cursor(
        &g,
        &[
            ("f-inbox", Some("d-f-inbox"), "caught_up"),
            ("f-arch", Some("d-f-arch"), "due"),
            ("f-proj", Some("d-f-proj"), "caught_up"),
        ],
    );
    g.mount_link(
        "d-f-arch",
        g.page(vec![removed("m1")], None, Some("d-f-arch")),
    )
    .await;
    g.batch.answer(
        "GET",
        "/me/messages/m1",
        200,
        message("m1", "c1", "f-inbox", AT),
    );
    let set = g.provider().changes(&start).await.unwrap();
    assert_eq!(added(&set), [("m1", "f-inbox")]);
    assert!(set.deleted.is_empty(), "never deleted");
}

#[tokio::test]
async fn a_removal_before_its_add_in_a_later_call_is_an_upsert_where_it_is_now() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let start = cursor(
        &g,
        &[
            ("f-inbox", Some("d-f-inbox"), "due"),
            ("f-arch", Some("d-f-arch"), "caught_up"),
            ("f-proj", Some("d-f-proj"), "caught_up"),
        ],
    );
    g.mount_link(
        "d-f-inbox",
        g.page(vec![removed("m1")], None, Some("d-f-inbox")),
    )
    .await;
    g.batch.answer(
        "GET",
        "/me/messages/m1",
        200,
        message("m1", "c1", "f-proj", AT),
    );
    let set = g.provider().changes(&start).await.unwrap();
    assert_eq!(added(&set), [("m1", "f-proj")]);
    assert!(set.deleted.is_empty());
}

#[tokio::test]
async fn a_message_moved_to_a_folder_rail_does_not_sync_is_deleted() {
    let g = Graph::start().await;
    mailbox(&g).await;
    g.mount_link(
        "d-f-inbox",
        g.page(vec![removed("m1")], None, Some("d-f-inbox")),
    )
    .await;
    quiet(&g, &["f-arch", "f-proj"]).await;
    g.batch.answer(
        "GET",
        "/me/messages/m1",
        200,
        message("m1", "c1", "f-conv", AT),
    );
    let set = g.provider().changes(&caught_up(&g)).await.unwrap();
    assert_eq!(set.deleted, ["m1"]);
    assert!(set.added.is_empty());
}

#[tokio::test]
async fn an_expired_link_for_one_folder_expires_the_whole_cursor() {
    let g = Graph::start().await;
    mailbox(&g).await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/links/d-f-inbox")))
        .respond_with(ResponseTemplate::new(410))
        .mount(&g.server)
        .await;
    let error = g.provider().changes(&caught_up(&g)).await.unwrap_err();
    assert_eq!(error, ProviderError::CursorExpired);
}

#[tokio::test]
async fn a_new_folder_joins_unstarted_and_a_folder_gone_resyncs() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let initial = g.page(
        vec![message("p1", "c1", "f-proj", AT)],
        None,
        Some("d-f-proj"),
    );
    g.mount_initial("f-proj", initial).await;
    let provider = g.provider();
    let older = cursor(
        &g,
        &[
            ("f-inbox", Some("d-f-inbox"), "caught_up"),
            ("f-arch", Some("d-f-arch"), "caught_up"),
        ],
    );
    let set = provider.changes(&older).await.unwrap();
    assert_eq!(added(&set), [("p1", "f-proj")]);
    assert_eq!(state_of(&set, "f-proj"), State::CaughtUp);
    let with_gone = cursor(
        &g,
        &[
            ("f-inbox", Some("d-f-inbox"), "caught_up"),
            ("f-arch", Some("d-f-arch"), "caught_up"),
            ("f-proj", Some("d-f-proj"), "caught_up"),
            ("f-old", Some("d-f-old"), "caught_up"),
        ],
    );
    assert_eq!(
        provider.changes(&with_gone).await.unwrap_err(),
        ProviderError::CursorExpired
    );
}

#[tokio::test]
async fn a_corrupt_oversized_or_foreign_cursor_expires() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let provider = g.provider();
    let pad = "x".repeat(MAX_CURSOR_BYTES);
    let oversized = SyncCursor(format!("{{\"v\":1,\"folders\":{{}},\"pad\":\"{pad}\"}}"));
    for bad in [
        SyncCursor("{not json".into()),
        oversized,
        SyncCursor(json!({"v": 2, "folders": {}}).to_string()),
        SyncCursor("12345".into()),
    ] {
        assert_eq!(
            provider.changes(&bad).await.unwrap_err(),
            ProviderError::CursorExpired
        );
    }
}

#[tokio::test]
async fn every_folder_is_polled_each_pass_even_past_the_page_budget() {
    let g = Graph::start().await;
    let ids: Vec<String> = (0..12).map(|n| format!("f-{n:02}")).collect();
    let listed: Vec<Value> = ids.iter().map(|id| folder(id, id, 0)).collect();
    g.mount_root(Value::Array(listed)).await;
    g.mount_well_known(&[]).await;
    let links: Vec<String> = ids.iter().map(|id| format!("d-{id}")).collect();
    for link in &links {
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}/links/{link}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(g.page(
                vec![],
                None,
                Some(link),
            )))
            .expect(1)
            .mount(&g.server)
            .await;
    }
    let folders: Vec<(&str, Option<&str>, &str)> = ids
        .iter()
        .zip(&links)
        .map(|(id, link)| (id.as_str(), Some(link.as_str()), "caught_up"))
        .collect();
    let provider = g.provider();
    let one = provider.changes(&cursor(&g, &folders)).await.unwrap();
    assert!(one.more, "ten pages a call: two folders still due");
    let two = provider.changes(&one.cursor).await.unwrap();
    assert!(!two.more, "the round is over");
    // Each link's `expect(1)` is checked when the server drops: every folder, once.
}

#[tokio::test]
async fn history_days_puts_the_cutoff_on_every_initial_delta() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let provider = g.provider_with_days(30);
    let start = provider.profile().await.unwrap().cursor;
    let after = GraphCursor::decode(&start)
        .unwrap()
        .after
        .expect("a cutoff");
    let filter = format!("receivedDateTime ge {}", iso(after));
    for id in ["f-inbox", "f-arch", "f-proj"] {
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}/me/mailFolders/{id}/messages/delta")))
            .and(query_param("$filter", filter.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(g.page(
                vec![],
                None,
                Some(&format!("d-{id}")),
            )))
            .expect(1)
            .mount(&g.server)
            .await;
    }
    let set = provider.changes(&start).await.unwrap();
    assert!(!set.more);
}

#[tokio::test]
async fn the_profile_lists_every_folder_unstarted_and_the_listing_is_empty() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let provider = g.provider();
    let profile = provider.profile().await.unwrap();
    assert_eq!(profile.messages_total, None);
    let start = GraphCursor::decode(&profile.cursor).unwrap();
    let ids: Vec<&String> = start.folders.keys().collect();
    assert_eq!(ids, ["f-arch", "f-inbox", "f-proj"]);
    assert!(
        start
            .folders
            .values()
            .all(|f| f.link.is_none() && f.state == State::Running)
    );
    assert_eq!(start.after, None);
    let page = provider.list(&Window::default(), None).await.unwrap();
    assert!(page.ids.is_empty() && page.next.is_none());
}

#[tokio::test]
async fn an_envelope_in_a_folder_rail_does_not_sync_gets_no_folder() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let provider = g.provider();
    provider.folders().await.unwrap();
    g.batch.answer(
        "GET",
        "/me/messages/m1",
        200,
        message("m1", "c1", "f-conv", AT),
    );
    g.batch.answer(
        "GET",
        "/me/messages/m2",
        200,
        message("m2", "c2", "f-inbox", AT),
    );
    let page = provider
        .envelopes(&["m1".into(), "m2".into()])
        .await
        .unwrap();
    assert!(
        page.envelopes[0].folders.is_empty(),
        "no placeholder folder"
    );
    assert_eq!(page.envelopes[1].folders, ["f-inbox"]);
}
