//! The Drafts folder: the store's own drafts and the Gmail drafts no local draft owns, merged
//! newest first; a local draft found by its Gmail message; and what an import of a Gmail draft
//! is built from.

use rusqlite::{OptionalExtension, Row, params};

use super::drafts::{Attachment, Recipient, ReplyAttachment, attachments_of, parsed};
use super::{Store, StoreError};
use crate::ops::DraftId;
use crate::provider::Mailbox;

/// Rows `drafts_folder` answers at most.
pub const MAX_DRAFTS_ROWS: usize = 500;

/// What a Drafts-folder row stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftsRowKind {
    /// One of the store's drafts; `on_remote` once it was pushed or imported.
    Local { draft: DraftId, on_remote: bool },
    /// A synced provider draft (Gmail's or Outlook's) no local draft owns (written elsewhere):
    /// the store's message id.
    Remote { message: i64 },
}

/// One row of the Drafts folder. `to` and `subject` are outside text.
#[derive(Clone, PartialEq, Eq)]
pub struct DraftsRow {
    pub account_id: i64,
    pub kind: DraftsRowKind,
    pub to: Vec<Recipient>,
    pub subject: String,
    /// Unix seconds: a local draft's last save, a Gmail draft's `Date:` header.
    pub date: i64,
}

impl std::fmt::Debug for DraftsRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftsRow")
            .field("account_id", &self.account_id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// What an import of a stored message (a Gmail draft) is built from. Every string but the
/// ids is mail content.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportSource {
    /// The store's message id.
    pub message: i64,
    pub account_id: i64,
    pub thread_id: i64,
    pub remote_thread_id: String,
    pub remote_id: String,
    pub is_draft: bool,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    pub bcc: Vec<Mailbox>,
    /// `None` until the body is fetched.
    pub body_text: Option<String>,
    pub attachments: Vec<ReplyAttachment>,
}

impl std::fmt::Debug for ImportSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportSource")
            .field("message", &self.message)
            .field("account_id", &self.account_id)
            .field("thread_id", &self.thread_id)
            .field("is_draft", &self.is_draft)
            .finish_non_exhaustive()
    }
}

/// `drafts.updated_at` is the writer's clock in Unix seconds, as `messages.date` (the
/// `Date:` header) is: the two compare as they are.
const LOCAL: &str =
    "SELECT id, account_id, remote_draft_id IS NOT NULL, to_json, subject, updated_at
     FROM drafts WHERE account_id IN (SELECT value FROM json_each(?1))
     ORDER BY updated_at DESC, id DESC LIMIT ?2";
/// Driven from each account's Drafts folder (`message_folders_by_folder`), not a walk over
/// every message: Gmail's `DRAFT` label is both the folder and `is_draft`. `CROSS JOIN` keeps
/// SQLite to that order.
pub(super) const GMAIL: &str = "SELECT m.id, f.account_id, m.to_json, m.subject, m.date
     FROM folders f
     CROSS JOIN message_folders mf ON mf.folder_id = f.id
     CROSS JOIN messages m ON m.id = mf.message_id
     WHERE f.kind = 'drafts' AND f.account_id IN (SELECT value FROM json_each(?1))
       AND m.is_draft = 1
       AND NOT EXISTS (SELECT 1 FROM drafts d
                       WHERE d.account_id = f.account_id AND d.remote_message_id = m.remote_id)
     ORDER BY m.date DESC, m.id DESC LIMIT ?2";

fn read_local(r: &Row<'_>) -> rusqlite::Result<DraftsRow> {
    Ok(DraftsRow {
        kind: DraftsRowKind::Local {
            draft: r.get(0)?,
            on_remote: r.get(2)?,
        },
        account_id: r.get(1)?,
        to: parsed(&r.get::<_, String>(3)?),
        subject: r.get(4)?,
        date: r.get(5)?,
    })
}

fn read_gmail(r: &Row<'_>) -> rusqlite::Result<DraftsRow> {
    let to: Vec<Mailbox> = parsed(&r.get::<_, String>(2)?);
    Ok(DraftsRow {
        kind: DraftsRowKind::Remote { message: r.get(0)? },
        account_id: r.get(1)?,
        to: to.into_iter().map(Recipient::from).collect(),
        subject: r.get(3)?,
        date: r.get(4)?,
    })
}

/// Newest first; on one date local drafts before Gmail's, then the higher id first.
fn order_key(row: &DraftsRow) -> (i64, bool, i64) {
    match row.kind {
        DraftsRowKind::Local { draft, .. } => (row.date, true, draft),
        DraftsRowKind::Remote { message } => (row.date, false, message),
    }
}

fn read_import(r: &Row<'_>) -> rusqlite::Result<ImportSource> {
    let text = |i: usize| -> rusqlite::Result<String> { r.get(i) };
    let references: Option<String> = r.get(8)?;
    Ok(ImportSource {
        message: r.get(0)?,
        account_id: r.get(1)?,
        thread_id: r.get(2)?,
        remote_thread_id: text(3)?,
        remote_id: text(4)?,
        is_draft: r.get(5)?,
        message_id: r.get(6)?,
        in_reply_to: r.get(7)?,
        references: references.as_deref().map(parsed).unwrap_or_default(),
        subject: text(9)?,
        to: parsed(&text(10)?),
        cc: parsed(&text(11)?),
        bcc: parsed(&text(12)?),
        body_text: r.get(13)?,
        attachments: Vec::new(),
    })
}

impl Store {
    /// The Drafts folder of `accounts`: their local drafts and the Gmail drafts no local draft
    /// owns (a pushed draft's Gmail copy is its local row), newest first, at most
    /// `MAX_DRAFTS_ROWS`.
    pub fn drafts_folder(&self, accounts: &[i64]) -> Result<Vec<DraftsRow>, StoreError> {
        let ids = serde_json::to_string(accounts).unwrap_or_else(|_| "[]".into());
        let limit = i64::try_from(MAX_DRAFTS_ROWS).unwrap_or(i64::MAX);
        let mut rows: Vec<DraftsRow> = {
            let mut stmt = self.conn.prepare(LOCAL)?;
            let local = stmt.query_map(params![ids, limit], read_local)?;
            local.collect::<Result<_, _>>()?
        };
        let mut stmt = self.conn.prepare(GMAIL)?;
        for row in stmt.query_map(params![ids, limit], read_gmail)? {
            rows.push(row?);
        }
        rows.sort_by_key(|row| std::cmp::Reverse(order_key(row)));
        rows.truncate(MAX_DRAFTS_ROWS);
        Ok(rows)
    }

    /// The account's local draft whose Gmail copy is `remote_message_id`.
    pub fn draft_by_remote_message(
        &self,
        account_id: i64,
        remote_message_id: &str,
    ) -> Result<Option<DraftId>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM drafts WHERE account_id = ?1 AND remote_message_id = ?2
                 ORDER BY id LIMIT 1",
                params![account_id, remote_message_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The account's local draft bound to the Gmail draft `remote_draft_id` (pushed or
    /// imported), whatever message Gmail holds under it now.
    pub fn draft_by_remote_draft(
        &self,
        account_id: i64,
        remote_draft_id: &str,
    ) -> Result<Option<DraftId>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM drafts WHERE account_id = ?1 AND remote_draft_id = ?2
                 ORDER BY id LIMIT 1",
                params![account_id, remote_draft_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The draft `token` made, in any account (tokens are unique across accounts), and the
    /// Gmail message it holds, if any.
    pub fn draft_token_owner(
        &self,
        token: &str,
    ) -> Result<Option<(DraftId, Option<String>)>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, remote_message_id FROM drafts WHERE client_token = ?1",
                [token],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// The client token a draft was created with (`None` for a draft without one, or none).
    pub fn draft_client_token(&self, id: DraftId) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT client_token FROM drafts WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten())
    }

    /// The path of every file attachment of every draft; `None` when a draft's list does not
    /// parse, so a caller removing files nothing names stops instead of guessing.
    pub fn draft_file_paths(&self) -> Result<Option<Vec<String>>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT attachments_json FROM drafts")?;
        let lists = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for list in lists {
            let Ok(parsed) = serde_json::from_str::<Vec<Attachment>>(&list?) else {
                return Ok(None);
            };
            paths.extend(parsed.into_iter().filter_map(|a| match a {
                Attachment::File { path, .. } => Some(path),
                Attachment::Part { .. } => None,
            }));
        }
        Ok(Some(paths))
    }

    /// The headers, addresses, body and attachments an import of `message` needs; `None`
    /// when the store holds no such message in a thread.
    pub fn import_source(&self, message: i64) -> Result<Option<ImportSource>, StoreError> {
        let head = self
            .conn
            .query_row(
                "SELECT m.id, t.account_id, m.thread_id, t.remote_id, m.remote_id, m.is_draft,
                        m.message_id, m.in_reply_to, m.references_ids, m.subject, m.to_json,
                        m.cc_json, m.bcc_json, m.body_text
                 FROM messages m JOIN threads t ON t.id = m.thread_id WHERE m.id = ?1",
                [message],
                read_import,
            )
            .optional()?;
        let Some(mut source) = head else {
            return Ok(None);
        };
        source.attachments = attachments_of(&self.conn, message)?;
        Ok(Some(source))
    }
}
