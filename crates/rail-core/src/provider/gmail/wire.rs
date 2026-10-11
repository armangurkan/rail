//! Gmail's JSON, as the REST reference describes it. Lenient: absent fields default, so one
//! odd message is skipped by the code that reads it rather than failing a whole answer.

use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct LabelList {
    #[serde(default)]
    pub labels: Vec<Label>,
}

#[derive(Deserialize)]
pub(crate) struct Label {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageList {
    #[serde(default)]
    pub messages: Vec<MessageRef>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageRef {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Message {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub internal_date: Option<String>,
    #[serde(default)]
    pub size_estimate: Option<i64>,
    #[serde(default)]
    pub payload: Option<Part>,
}

/// A MIME part. Task 5 adds the fields `format=full` needs; `format=metadata` has headers.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Part {
    #[serde(default)]
    pub headers: Vec<Header>,
}

#[derive(Deserialize)]
pub(crate) struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProfileWire {
    pub history_id: String,
    #[serde(default)]
    pub messages_total: u64,
}
