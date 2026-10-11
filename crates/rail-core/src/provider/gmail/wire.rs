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
    /// Empty when absent: an id Rail cannot use is dropped by whoever reads it.
    #[serde(default)]
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

/// A MIME part of a `format=full` message (only `headers` for `format=metadata`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Part {
    #[serde(default)]
    pub part_id: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: PartBody,
    #[serde(default)]
    pub parts: Vec<Part>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PartBody {
    #[serde(default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub data: Option<String>,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryList {
    #[serde(default)]
    pub history: Vec<History>,
    #[serde(default)]
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub history_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct History {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub messages_added: Vec<HistoryMessage>,
    #[serde(default)]
    pub messages_deleted: Vec<HistoryMessage>,
    #[serde(default)]
    pub labels_added: Vec<HistoryLabels>,
    #[serde(default)]
    pub labels_removed: Vec<HistoryLabels>,
}

#[derive(Deserialize)]
pub(crate) struct HistoryMessage {
    pub message: MessageRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryLabels {
    pub message: MessageRef,
    #[serde(default)]
    pub label_ids: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct AttachmentBody {
    #[serde(default)]
    pub data: Option<String>,
}
