//! The two tools the user must confirm: `rail_account_remove` and `rail_secrets_configure`,
//! with hosts that accept, decline, cancel, answer oddly or cannot ask, and with a plan that
//! changes between the question and the answer.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    FILE_BACKEND, Host, KEYRING_BACKEND, Reply, WORK, config_text, host, one_call,
    refused_by_schema, structured, text,
};

/// Every answer that is not a plain yes, and how the summary explains it.
const NOT_YES: [(Reply, &str); 4] = [
    (Reply::Decline, "the user declined"),
    (Reply::Unticked, "the user declined"),
    (Reply::Cancel, "the user declined"),
    (Reply::Empty, "could not be shown"),
];

async fn remove_with(
    host: Host,
    args: Value,
) -> (tempfile::TempDir, rmcp::model::CallToolResult, Vec<String>) {
    let (dir, result, asked) = one_call(WORK, host, "rail_account_remove", args).await;
    (dir, result.unwrap(), asked)
}

#[tokio::test]
async fn remove_confirmed_through_elicitation_removes_the_account() {
    let (dir, result, asked) =
        remove_with(host(Some(Reply::Yes)), json!({"account": "WORK"})).await;
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].contains("Remove account \"work\" (\"me@example.com\", gmail)?"),
        "{asked:?}"
    );
    assert!(asked[0].contains("Rail never deletes mail"), "{asked:?}");
    assert_eq!(structured(&result)["name"], "work", "{}", text(&result));
    assert!(!config_text(&dir).contains("me@example.com"));
}

#[tokio::test]
async fn remove_answered_with_anything_but_yes_removes_nothing() {
    for (reply, why) in NOT_YES {
        let (dir, result, asked) = remove_with(host(Some(reply)), json!({"account": "work"})).await;
        assert_eq!(asked.len(), 1, "{reply:?}");
        assert_eq!(structured(&result)["removed"], false, "{reply:?}");
        assert!(text(&result).contains(why), "{reply:?}: {}", text(&result));
        assert_eq!(config_text(&dir), WORK, "{reply:?}");
    }
}

/// Review focus 4: without elicitation nothing is removed.
#[tokio::test]
async fn remove_without_elicitation_removes_nothing_and_names_the_cli_command() {
    let (dir, result, asked) = remove_with(host(None), json!({"account": "work"})).await;
    assert!(asked.is_empty());
    assert_eq!(structured(&result)["removed"], false);
    assert!(
        text(&result).contains("rail account remove work"),
        "{}",
        text(&result)
    );
    assert_eq!(config_text(&dir), WORK);
}

/// Ruling A1: the agent cannot even send a `confirm` field.
#[tokio::test]
async fn a_confirm_field_from_the_agent_is_refused() {
    let args = json!({"account": "work", "confirm": true});
    let (dir, result, asked) =
        one_call(WORK, host(Some(Reply::Yes)), "rail_account_remove", args).await;
    let result = result.unwrap();
    assert!(refused_by_schema(&result), "{}", text(&result));
    assert!(asked.is_empty());
    assert_eq!(config_text(&dir), WORK);
    let args = json!({"backend": "file", "confirm": true});
    let (dir, result, asked) = one_call(
        KEYRING_BACKEND,
        host(Some(Reply::Yes)),
        "rail_secrets_configure",
        args,
    )
    .await;
    let result = result.unwrap();
    assert!(refused_by_schema(&result), "{}", text(&result));
    assert!(asked.is_empty());
    assert_eq!(config_text(&dir), KEYRING_BACKEND);
}

/// Ruling S3: the account changes while the user reads the question; their yes was for the
/// old plan, so nothing is removed.
#[tokio::test]
async fn a_removal_whose_plan_changed_before_the_answer_removes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    super::write_config(&dir, WORK);
    let file = dir.path().join("config").join("config.toml");
    let moved = WORK.replace("me@example.com", "other@example.com");
    let edit = moved.clone();
    let mut host = host(Some(Reply::Yes));
    host.meanwhile = Some(Arc::new(move || std::fs::write(&file, &edit).unwrap()));
    let client = super::start(&dir, host, None).await;
    let result = super::call(&client, "rail_account_remove", json!({"account": "work"})).await;
    client.close().await;
    assert_eq!(structured(&result)["removed"], false, "{}", text(&result));
    assert!(
        text(&result).contains("no longer matches"),
        "{}",
        text(&result)
    );
    assert_eq!(config_text(&dir), moved);
}

async fn switch_to_file(
    host: Host,
) -> (tempfile::TempDir, rmcp::model::CallToolResult, Vec<String>) {
    let args = json!({"backend": "file"});
    let (dir, result, asked) =
        one_call(KEYRING_BACKEND, host, "rail_secrets_configure", args).await;
    (dir, result.unwrap(), asked)
}

/// No account has tokens, so the keyring is never asked: only the config changes.
#[tokio::test]
async fn a_secret_store_change_confirmed_through_elicitation_happens() {
    let (dir, result, asked) = switch_to_file(host(Some(Reply::Yes))).await;
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        asked[0].contains("from keyring to file") && asked[0].contains("plain 0600 file"),
        "{asked:?}"
    );
    assert_eq!(structured(&result)["backend"], "file", "{}", text(&result));
    assert_eq!(structured(&result)["changed"], true);
    assert!(text(&result).contains("Tokens now go to the file backend"));
    assert!(config_text(&dir).contains("secrets = \"file\""));
}

#[tokio::test]
async fn a_secret_store_change_answered_with_anything_but_yes_changes_nothing() {
    for (reply, why) in NOT_YES {
        let (dir, result, asked) = switch_to_file(host(Some(reply))).await;
        assert_eq!(asked.len(), 1, "{reply:?}");
        assert_eq!(structured(&result)["changed"], false, "{reply:?}");
        assert!(text(&result).contains(why), "{reply:?}: {}", text(&result));
        assert_eq!(config_text(&dir), KEYRING_BACKEND, "{reply:?}");
    }
}

#[tokio::test]
async fn a_secret_store_change_without_elicitation_changes_nothing_and_says_how() {
    let (dir, result, asked) = switch_to_file(host(None)).await;
    assert!(asked.is_empty());
    assert_eq!(structured(&result)["changed"], false, "{}", text(&result));
    let said = text(&result);
    assert!(
        said.contains("secrets = \\\"file\\\"") && said.contains("config.toml"),
        "{said}"
    );
    assert!(said.contains("do not edit config.toml yourself"), "{said}");
    assert_eq!(config_text(&dir), KEYRING_BACKEND);
}

#[tokio::test]
async fn a_secret_store_setting_already_in_place_needs_no_confirmation() {
    let args = json!({"backend": "file"});
    let (dir, result, asked) = one_call(
        FILE_BACKEND,
        host(Some(Reply::Decline)),
        "rail_secrets_configure",
        args,
    )
    .await;
    let result = result.unwrap();
    assert!(asked.is_empty(), "{asked:?}");
    assert_eq!(structured(&result)["changed"], false, "{}", text(&result));
    assert!(
        text(&result).contains("already in place"),
        "{}",
        text(&result)
    );
    assert_eq!(config_text(&dir), FILE_BACKEND);
}
