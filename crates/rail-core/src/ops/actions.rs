//! What a user can do to threads (`action.apply`, `action.undo`, `action.retry`), and the outbox
//! row ids those operations speak. Terminal-only: no MCP tool reaches them, so agents cannot
//! change mail. Every value here is an id, a flag or a count: never mail content.

use std::collections::HashSet;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::OpError;

/// An outbox row's id.
pub type OutboxId = i64;

/// A local draft's id (the store's `drafts.id`).
pub type DraftId = i64;

/// A row id with the client token the caller used when it was queued: undo and retry act only
/// when both match, so a reused row id can never be undone or resent by mistake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowToken {
    pub row: OutboxId,
    pub client_token: String,
}

/// How long an action waits in the outbox before it is sent, so `z` can take it back.
pub const UNDO_GRACE: Duration = Duration::from_secs(10);

/// An account's outbox: rows waiting (inflight ones too), rows that failed, and `send` rows
/// being sent now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutboxDepth {
    pub pending: u64,
    pub failed: u64,
    #[serde(default)]
    pub sending: u64,
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
    /// The draft of a draft action.
    #[serde(default)]
    pub draft: Option<DraftId>,
    /// The token the row was queued with, for `action.retry`.
    #[serde(default)]
    pub client_token: String,
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
    /// Send this draft (the daemon builds the message when it sends).
    Send {
        draft: DraftId,
    },
    /// Create or update this draft in Gmail's Drafts.
    PushDraft {
        draft: DraftId,
    },
    /// Delete this draft from Gmail's Drafts.
    DeleteDraft {
        draft: DraftId,
        remote_draft_id: String,
    },
}

impl Action {
    /// The v1 `outbox.kind` value: its `CHECK` predates these actions (plan decision 2).
    pub fn kind_column(&self) -> &'static str {
        match self {
            Self::Archive {} | Self::Labels { .. } | Self::Move { .. } => "move",
            Self::Star { .. } | Self::Read { .. } => "set_flags",
            Self::Trash {} => "trash",
            Self::Send { .. } => "send",
            Self::PushDraft { .. } | Self::DeleteDraft { .. } => "save_draft",
        }
    }

    /// The draft a draft action acts on; `None` for an action on threads.
    pub fn draft_id(&self) -> Option<DraftId> {
        match self {
            Self::Send { draft } | Self::PushDraft { draft } | Self::DeleteDraft { draft, .. } => {
                Some(*draft)
            }
            Self::Archive {}
            | Self::Trash {}
            | Self::Star { .. }
            | Self::Read { .. }
            | Self::Labels { .. }
            | Self::Move { .. } => None,
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

/// Threads one `action.apply` takes at most.
pub const MAX_ACTION_THREADS: usize = 200;
/// Label ids one `labels` action adds, or removes, at most (Gmail's limit per change).
pub const MAX_ACTION_LABELS: usize = 100;
/// Rows one `action.undo` takes at most.
pub const MAX_UNDO_ROWS: usize = 64;
/// Characters a client token has at most.
pub const MAX_CLIENT_TOKEN_CHARS: usize = 64;

/// `action.apply`: one account's threads (the TUI sends one call per account).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionApplyRequest {
    /// An account's name or address.
    pub account: String,
    /// Local thread ids (the store's), 1 to 200.
    pub threads: Vec<i64>,
    pub action: Action,
    /// Wait `UNDO_GRACE` before sending (false: mark-read on open).
    #[serde(default)]
    pub grace: bool,
    /// Made by the client; a replay finds the row it made.
    pub client_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionApplyResponse {
    pub rows: Vec<OutboxId>,
    /// How many of the threads the change applied to.
    pub applied: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionUndoRequest {
    /// 1 to `MAX_UNDO_ROWS` rows, each with the token it was queued with.
    pub rows: Vec<RowToken>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionUndoResponse {
    pub undone: Vec<OutboxId>,
    /// Already sent, being sent, failed, or unknown.
    pub too_late: Vec<OutboxId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionRetryRequest {
    pub row: OutboxId,
    /// The token the row was queued with.
    pub client_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionRetryResponse {
    /// False when the row is not a failed one.
    pub retried: bool,
}

/// A fresh client token: 24 random letters and digits.
pub fn client_token() -> String {
    (0..24).map(|_| fastrand::alphanumeric()).collect()
}

fn safe_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_CLIENT_TOKEN_CHARS
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Refused (never quoting it) unless `token` is 1 to `MAX_CLIENT_TOKEN_CHARS` letters, digits,
/// '-' or '_'.
pub fn checked_token(token: &str) -> Result<(), OpError> {
    if safe_token(token) {
        Ok(())
    } else {
        Err(OpError::invalid_input(format!(
            "a client token is 1 to {MAX_CLIENT_TOKEN_CHARS} letters, digits, '-' or '_'"
        )))
    }
}

/// True when no id appears twice in `ids`.
fn all_distinct(ids: &[i64]) -> bool {
    let mut seen = HashSet::new();
    ids.iter().all(|id| seen.insert(*id))
}

fn check_labels(action: &Action) -> Result<(), OpError> {
    if action.label_ids().iter().any(|id| *id <= 0) {
        return Err(OpError::invalid_input("a label id is a positive number"));
    }
    let Action::Labels { add, remove } = action else {
        return Ok(());
    };
    if add.is_empty() && remove.is_empty() {
        return Err(OpError::invalid_input(
            "a labels action adds or removes at least one label",
        ));
    }
    if add.len() > MAX_ACTION_LABELS || remove.len() > MAX_ACTION_LABELS {
        return Err(OpError::invalid_input(format!(
            "a labels action adds or removes at most {MAX_ACTION_LABELS} labels"
        )));
    }
    // Refused, not deduplicated: a repeated id is a client bug worth surfacing.
    if !all_distinct(add) || !all_distinct(remove) {
        return Err(OpError::invalid_input("a label is listed twice"));
    }
    if add.iter().any(|id| remove.contains(id)) {
        return Err(OpError::invalid_input(
            "a label cannot be both added and removed",
        ));
    }
    Ok(())
}

impl ActionApplyRequest {
    /// The thread ids, each once, in order; refused (never quoting an id) unless 1 to
    /// `MAX_ACTION_THREADS` positive ids, a safe client token and sound label ids.
    pub fn checked(&self) -> Result<Vec<i64>, OpError> {
        if self.threads.is_empty() || self.threads.len() > MAX_ACTION_THREADS {
            return Err(OpError::invalid_input(format!(
                "an action takes 1 to {MAX_ACTION_THREADS} threads"
            )));
        }
        if self.threads.iter().any(|id| *id <= 0) {
            return Err(OpError::invalid_input("a thread id is a positive number"));
        }
        checked_token(&self.client_token)?;
        if self.action.draft_id().is_some() {
            return Err(OpError::invalid_input(
                "a draft action goes through draft.send",
            ));
        }
        check_labels(&self.action)?;
        let mut seen = HashSet::new();
        Ok(self
            .threads
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect())
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
            (Action::Send { draft: 5 }, r#"{"kind":"send","draft":5}"#),
            (
                Action::PushDraft { draft: 5 },
                r#"{"kind":"push_draft","draft":5}"#,
            ),
            (
                Action::DeleteDraft {
                    draft: 5,
                    remote_draft_id: "r1".into(),
                },
                r#"{"kind":"delete_draft","draft":5,"remote_draft_id":"r1"}"#,
            ),
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
        assert_eq!(Action::Send { draft: 1 }.kind_column(), "send");
        assert_eq!(Action::PushDraft { draft: 1 }.kind_column(), "save_draft");
        let delete = Action::DeleteDraft {
            draft: 2,
            remote_draft_id: "r".into(),
        };
        assert_eq!(delete.kind_column(), "save_draft");
        assert_eq!(delete.draft_id(), Some(2));
        assert_eq!(Action::Send { draft: 1 }.draft_id(), Some(1));
        assert_eq!(Action::Archive {}.draft_id(), None);
    }

    fn request(threads: Vec<i64>, action: Action) -> ActionApplyRequest {
        ActionApplyRequest {
            account: "work".into(),
            threads,
            action,
            grace: true,
            client_token: "tok_1".into(),
        }
    }

    #[test]
    fn apply_checks_bounds_dedupes_and_never_quotes_input() {
        assert_eq!(
            request(vec![3, 1, 3], Action::Archive {})
                .checked()
                .unwrap(),
            [3, 1]
        );
        for bad in [vec![], vec![0], vec![-4], (1..=201).collect::<Vec<i64>>()] {
            let error = request(bad.clone(), Action::Archive {})
                .checked()
                .unwrap_err();
            assert!(!error.message.contains("-4"), "{}", error.message);
        }
        let mut token = request(vec![1], Action::Archive {});
        for bad in ["", "a b", &"x".repeat(65), "tok\n"] {
            token.client_token = bad.to_string();
            assert!(token.checked().is_err(), "{bad:?}");
        }
        let both = Action::Labels {
            add: vec![2],
            remove: vec![2],
        };
        assert!(request(vec![1], both).checked().is_err());
        let none = Action::Labels {
            add: vec![],
            remove: vec![],
        };
        assert!(request(vec![1], none).checked().is_err());
        let many = Action::Labels {
            add: (1..=101).collect(),
            remove: vec![],
        };
        assert!(request(vec![1], many).checked().is_err());
        assert!(request(vec![1], Action::Move { to: 0 }).checked().is_err());
        for draft in [
            Action::Send { draft: 1 },
            Action::PushDraft { draft: 1 },
            Action::DeleteDraft {
                draft: 1,
                remote_draft_id: "r".into(),
            },
        ] {
            let error = request(vec![1], draft).checked().unwrap_err();
            assert_eq!(error.message, "a draft action goes through draft.send");
        }
    }

    /// A repeated label id inside one list is refused (not silently deduplicated).
    #[test]
    fn a_label_listed_twice_is_refused() {
        for action in [
            Action::Labels {
                add: vec![5, 5],
                remove: vec![],
            },
            Action::Labels {
                add: vec![],
                remove: vec![6, 7, 6],
            },
        ] {
            let error = request(vec![1], action).checked().unwrap_err();
            assert!(!error.message.contains('5'), "{}", error.message);
        }
        let fine = Action::Labels {
            add: vec![5],
            remove: vec![6],
        };
        assert!(request(vec![1], fine).checked().is_ok());
    }

    #[test]
    fn requests_refuse_unknown_fields_and_a_token_looks_right() {
        let json =
            r#"{"account":"w","threads":[1],"action":{"kind":"archive"},"client_token":"t","x":1}"#;
        assert!(serde_json::from_str::<ActionApplyRequest>(json).is_err());
        let undo = r#"{"rows":[{"row":1,"client_token":"t"}]}"#;
        assert!(serde_json::from_str::<ActionUndoRequest>(undo).is_ok());
        assert!(serde_json::from_str::<ActionUndoRequest>(r#"{"rows":[1]}"#).is_err());
        let extra = r#"{"rows":[{"row":1,"client_token":"t","x":1}]}"#;
        assert!(serde_json::from_str::<ActionUndoRequest>(extra).is_err());
        let retry = r#"{"row":1,"client_token":"t"}"#;
        assert!(serde_json::from_str::<ActionRetryRequest>(retry).is_ok());
        assert!(
            serde_json::from_str::<ActionRetryRequest>(r#"{"row":1,"client_token":"t","x":1}"#)
                .is_err()
        );
        assert!(checked_token("tok_1-A").is_ok());
        for bad in ["", "a b", "tok\n"] {
            assert!(checked_token(bad).is_err(), "{bad:?}");
        }
        // An old depth without `sending` still reads.
        let depth: OutboxDepth = serde_json::from_str(r#"{"pending":1,"failed":2}"#).unwrap();
        assert_eq!(depth.sending, 0);
        let token = client_token();
        assert_eq!(token.len(), 24);
        assert!(token.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(client_token(), token);
    }
}
