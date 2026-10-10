//! Tool results: structured content (the operation's response, outside text inside
//! `{"untrusted": ...}`) with the summary for the model as the text content.

use rail_core::ops::{OpError, Untrusted};
use rmcp::model::{CallToolResult, ContentBlock};
use serde::Serialize;

use super::summary;

/// Said when a result cannot be encoded; nothing of the result goes into it.
const UNENCODABLE: &str = "Rail could not encode its answer as JSON. The operation itself ran; rail_status shows where things stand.";

/// What a guarded tool returns when it did nothing: `{"<done>": false, "reason": ..}`, plus
/// the outside text that explains it (an MCP client's error), marked untrusted.
#[derive(Serialize)]
struct NothingDone<'a> {
    #[serde(flatten)]
    done: std::collections::BTreeMap<&'a str, bool>,
    reason: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<Untrusted<&'a str>>,
}

fn encode<T: Serialize>(value: &T) -> Option<serde_json::Value> {
    serde_json::to_value(value)
        .inspect_err(|e| tracing::warn!(category = ?e.classify(), "could not encode a tool result"))
        .ok()
}

/// A finished operation: its response and the summary.
pub fn ok<T: Serialize>(value: &T, text: String) -> CallToolResult {
    let Some(value) = encode(value) else {
        return CallToolResult::error(vec![ContentBlock::text(UNENCODABLE)]);
    };
    let mut result = CallToolResult::structured(value);
    result.content = vec![ContentBlock::text(text)];
    result
}

/// A failed operation: the error (detail marked untrusted) and its summary.
pub fn failed(error: &OpError) -> CallToolResult {
    let Some(value) = encode(error) else {
        return CallToolResult::error(vec![ContentBlock::text(UNENCODABLE)]);
    };
    let mut result = CallToolResult::structured_error(value);
    result.content = vec![ContentBlock::text(summary::failure(error))];
    result
}

/// A guarded change that did not happen. `done` names the flag (`removed`, `changed`);
/// `reason` is Rail's text; `outside` is text from the client, quoted in the summary.
pub fn nothing_done(done: &str, what: &str, reason: &str, outside: Option<&str>) -> CallToolResult {
    let body = NothingDone {
        done: [(done, false)].into_iter().collect(),
        reason,
        detail: outside.map(Untrusted::new),
    };
    ok(&body, summary::nothing_done(what, reason, outside))
}
