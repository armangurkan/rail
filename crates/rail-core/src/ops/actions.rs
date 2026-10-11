//! What a user can do to threads (`action.apply`, `action.undo`, `action.retry`), and the outbox
//! row ids those operations speak. Terminal-only: no MCP tool reaches them, so agents cannot
//! change mail. Every value here is an id, a flag or a count: never mail content.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::OpError;

/// An outbox row's id.
pub type OutboxId = i64;

/// How long an action waits in the outbox before it is sent, so `z` can take it back.
pub const UNDO_GRACE: Duration = Duration::from_secs(10);

/// An account's outbox: rows waiting (inflight ones too) and rows that failed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutboxDepth {
    pub pending: u64,
    pub failed: u64,
}

/// The `action.failed` notification: an action Gmail refused for good, already reverted in the
/// store. `error` is Rail's words with Gmail's as untrusted detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionFailed {
    /// The account's name.
    pub account: String,
    pub row: OutboxId,
    pub threads: Vec<i64>,
    pub action: Action,
    pub error: OpError,
}

/// One change to whole threads (every message in them, as Gmail does). Label ids are the
/// store's `folders.id`. `Archive {}` and `Trash {}` are empty structs, not unit variants, so
/// `deny_unknown_fields` refuses extra fields there too; the JSON is still `{"kind":"archive"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Remove the inbox from every message.
    Archive {},
    /// Gmail's Trash: add the trash folder and remove the inbox; user labels stay.
    Trash {},
    Star {
        on: bool,
    },
    Read {
        on: bool,
    },
    /// Add and remove these labels.
    Labels {
        add: Vec<i64>,
        remove: Vec<i64>,
    },
    /// Add this label and remove the inbox.
    Move {
        to: i64,
    },
}

impl Action {
    /// The v1 `outbox.kind` value: its `CHECK` predates these actions (plan decision 2).
    pub fn kind_column(&self) -> &'static str {
        match self {
            Self::Archive {} | Self::Labels { .. } | Self::Move { .. } => "move",
            Self::Star { .. } | Self::Read { .. } => "set_flags",
            Self::Trash {} => "trash",
        }
    }

    /// Every label id the action names.
    pub fn label_ids(&self) -> Vec<i64> {
        match self {
            Self::Labels { add, remove } => add.iter().chain(remove).copied().collect(),
            Self::Move { to } => vec![*to],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_have_the_spec_json_and_refuse_unknown_fields() {
        let cases = [
            (Action::Archive {}, r#"{"kind":"archive"}"#),
            (Action::Trash {}, r#"{"kind":"trash"}"#),
            (Action::Star { on: true }, r#"{"kind":"star","on":true}"#),
            (Action::Read { on: false }, r#"{"kind":"read","on":false}"#),
            (
                Action::Labels {
                    add: vec![4],
                    remove: vec![5],
                },
                r#"{"kind":"labels","add":[4],"remove":[5]}"#,
            ),
            (Action::Move { to: 4 }, r#"{"kind":"move","to":4}"#),
        ];
        for (action, json) in cases {
            assert_eq!(serde_json::to_string(&action).unwrap(), json);
            assert_eq!(serde_json::from_str::<Action>(json).unwrap(), action);
        }
        for bad in [
            r#"{"kind":"archive","x":1}"#,
            r#"{"kind":"star","on":true,"x":1}"#,
            r#"{"kind":"delete"}"#,
        ] {
            assert!(serde_json::from_str::<Action>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn kinds_fit_the_v1_check_and_labels_are_listed() {
        assert_eq!(Action::Archive {}.kind_column(), "move");
        assert_eq!(Action::Read { on: true }.kind_column(), "set_flags");
        assert_eq!(Action::Trash {}.kind_column(), "trash");
        let labels = Action::Labels {
            add: vec![1, 2],
            remove: vec![3],
        };
        assert_eq!(labels.label_ids(), [1, 2, 3]);
        assert_eq!(Action::Move { to: 9 }.label_ids(), [9]);
    }
}
