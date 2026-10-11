//! The sync tools, and the rule that MCP never returns mail content.

use serde_json::json;

use super::{WORK, call, host, refused_by_schema, start, structured, text, try_call, write_config};

const CANARY_SUBJECT: &str = "CANARY-SUBJECT-7f3a";
const CANARY_SENDER: &str = "canary-sender@evil.example";
const CANARY_SNIPPET: &str = "CANARY-SNIPPET-9b1c";
const CANARY_BODY: &str = "CANARY-BODY-2d4e";

/// A store that already holds a message full of canaries for the configured account.
fn seed_mail(dir: &tempfile::TempDir) {
    let store = rail_core::store::Store::open(&dir.path().join("data").join("rail.db")).unwrap();
    store
        .conn()
        .execute_batch(&format!(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'me@example.com');
             INSERT INTO threads (account_id, remote_id, subject) VALUES (1, 't1', '{CANARY_SUBJECT}');
             INSERT INTO messages (account_id, thread_id, remote_id, subject, from_json, snippet, body_text, internal_date)
               VALUES (1, 1, 'm1', '{CANARY_SUBJECT}', '[{{\"name\":\"Evil\",\"address\":\"{CANARY_SENDER}\"}}]',
                       '{CANARY_SNIPPET}', '{CANARY_BODY}', 1);"
        ))
        .unwrap();
}

fn assert_no_mail(what: &str, value: &str) {
    for canary in [CANARY_SUBJECT, CANARY_SENDER, CANARY_SNIPPET, CANARY_BODY] {
        assert!(
            !value.contains(canary),
            "{what} carries mail content ({canary}): {value}"
        );
    }
}

/// Spec §8: MCP results contain no subject or sender.
#[tokio::test]
async fn no_tool_result_or_log_line_carries_mail_content() {
    let dir = tempfile::tempdir().unwrap();
    write_config(&dir, WORK);
    seed_mail(&dir);
    let client = start(&dir, host(None), None).await;
    for (tool, args) in [
        ("rail_sync_status", json!({})),
        ("rail_sync_status", json!({"account": "work"})),
        ("rail_sync_now", json!({})),
        ("rail_sync_now", json!({"account": "me@example.com"})),
        ("rail_status", json!({})),
        ("rail_accounts_list", json!({})),
    ] {
        let result = call(&client, tool, args).await;
        assert_no_mail(tool, &serde_json::to_string(&result).unwrap());
    }
    let status = call(&client, "rail_sync_status", json!({"account": "work"})).await;
    let account = &structured(&status)["accounts"][0];
    assert_eq!(account["name"], "work");
    assert!(account["messages"].as_u64().is_some(), "{account}");
    client.close().await;
    let log =
        std::fs::read_to_string(dir.path().join("state").join("rail.log")).unwrap_or_default();
    assert_no_mail("rail.log", &log);
}

#[tokio::test]
async fn the_sync_tools_refuse_unknown_arguments() {
    let dir = tempfile::tempdir().unwrap();
    write_config(&dir, WORK);
    let client = start(&dir, host(None), None).await;
    let watch = try_call(&client, "rail_sync_now", json!({"watch": true})).await;
    assert!(
        watch.is_err() || refused_by_schema(watch.as_ref().unwrap()),
        "{watch:?}"
    );
    let bogus = try_call(&client, "rail_sync_status", json!({"bogus": 1})).await;
    assert!(
        bogus.is_err() || refused_by_schema(bogus.as_ref().unwrap()),
        "{bogus:?}"
    );
    client.close().await;
}

#[tokio::test]
async fn sync_now_names_what_it_woke_and_an_unknown_account_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    write_config(&dir, WORK);
    let client = start(&dir, host(None), None).await;
    let woke = call(&client, "rail_sync_now", json!({"account": "work"})).await;
    assert_eq!(structured(&woke)["targets"][0]["name"], "work");
    assert!(text(&woke).contains("rail_sync_status"), "{}", text(&woke));
    let unknown = call(&client, "rail_sync_now", json!({"account": "nobody"})).await;
    assert_eq!(unknown.is_error, Some(true));
    assert_eq!(structured(&unknown)["code"], "not_found");
    client.close().await;
}
