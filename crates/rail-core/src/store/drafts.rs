//! Drafts: messages being written, kept until sent or discarded.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::{Store, StoreError};
use crate::ops::DraftId;
use crate::provider::Mailbox;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    New,
    Reply,
    ReplyAll,
    Forward,
}

impl DraftKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reply => "reply",
            Self::ReplyAll => "reply_all",
            Self::Forward => "forward",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "reply" => Self::Reply,
            "reply_all" => Self::ReplyAll,
            "forward" => Self::Forward,
            _ => Self::New,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftState {
    Editing,
    Queued,
}

impl DraftState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editing => "editing",
            Self::Queued => "queued",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "queued" => Self::Queued,
            _ => Self::Editing,
        }
    }
}

/// One recipient. `name` and `address` are outside text.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub address: String,
}

impl std::fmt::Debug for Recipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recipient")
            .field("named", &self.name.is_some())
            .finish_non_exhaustive()
    }
}

impl From<Mailbox> for Recipient {
    fn from(m: Mailbox) -> Self {
        Self {
            name: m.name,
            address: m.address,
        }
    }
}

/// What goes with the message: a file on this machine, or a part of a stored message
/// (a forwarded or imported attachment) the daemon fetches when it sends.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    File {
        path: String,
        name: String,
        mime: String,
        size: i64,
    },
    Part {
        message: i64,
        part: String,
        name: String,
        mime: String,
        size: i64,
    },
}

impl Attachment {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Part { name, .. } => name,
        }
    }

    pub fn mime(&self) -> &str {
        match self {
            Self::File { mime, .. } | Self::Part { mime, .. } => mime,
        }
    }

    pub fn size(&self) -> i64 {
        match self {
            Self::File { size, .. } | Self::Part { size, .. } => *size,
        }
    }
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File { size, .. } => f
                .debug_struct("File")
                .field("size", size)
                .finish_non_exhaustive(),
            Self::Part { message, size, .. } => f
                .debug_struct("Part")
                .field("message", message)
                .field("size", size)
                .finish_non_exhaustive(),
        }
    }
}

/// A stored draft. Every string but the ids is outside text or mail content.
#[derive(Clone, PartialEq, Eq)]
pub struct Draft {
    pub id: DraftId,
    pub account_id: i64,
    pub kind: DraftKind,
    pub thread_id: Option<i64>,
    pub remote_thread_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<Attachment>,
    pub message_id: String,
    pub remote_draft_id: Option<String>,
    pub remote_message_id: Option<String>,
    pub state: DraftState,
    pub updated_at: i64,
    /// Set before the first send call: Gmail may hold the message from then on.
    pub send_attempted_at: Option<i64>,
}

impl std::fmt::Debug for Draft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Draft")
            .field("id", &self.id)
            .field("account_id", &self.account_id)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .field(
                "recipients",
                &(self.to.len() + self.cc.len() + self.bcc.len()),
            )
            .field("attachments", &self.attachments.len())
            .finish_non_exhaustive()
    }
}

/// What `draft_upsert` writes. `id: None` creates; `message_id` is used only then.
#[derive(Clone, PartialEq, Eq)]
pub struct DraftWrite {
    pub id: Option<DraftId>,
    pub account_id: i64,
    pub kind: DraftKind,
    pub thread_id: Option<i64>,
    pub remote_thread_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<Attachment>,
    pub message_id: String,
}

impl std::fmt::Debug for DraftWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftWrite")
            .field("id", &self.id)
            .field("account_id", &self.account_id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// A Drafts-folder row. `subject` and `to` are outside text.
#[derive(Clone, PartialEq, Eq)]
pub struct DraftSummary {
    pub id: DraftId,
    pub account_id: i64,
    pub kind: DraftKind,
    pub subject: String,
    pub to: Vec<Recipient>,
    pub state: DraftState,
    pub updated_at: i64,
}

impl std::fmt::Debug for DraftSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftSummary")
            .field("id", &self.id)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// A stored attachment of a message being replied to or forwarded. `filename` is outside text.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplyAttachment {
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
}

impl std::fmt::Debug for ReplyAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplyAttachment")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// What a reply or forward is built from. Every string but the ids is mail content.
#[derive(Clone, PartialEq, Eq)]
pub struct ReplySource {
    /// The store's message id.
    pub message: i64,
    pub account_id: i64,
    pub thread_id: i64,
    pub remote_thread_id: String,
    pub remote_id: String,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Vec<Mailbox>,
    pub reply_to: Vec<Mailbox>,
    pub to: Vec<Mailbox>,
    pub cc: Vec<Mailbox>,
    /// The `Date:` header, Unix seconds.
    pub date: i64,
    /// `None` until the body is fetched.
    pub body_text: Option<String>,
    pub attachments: Vec<ReplyAttachment>,
}

impl std::fmt::Debug for ReplySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplySource")
            .field("message", &self.message)
            .field("thread_id", &self.thread_id)
            .field("has_body", &self.body_text.is_some())
            .field("attachments", &self.attachments.len())
            .finish_non_exhaustive()
    }
}

const DRAFT_COLUMNS: &str = "id, account_id, kind, thread_id, remote_thread_id, in_reply_to, \
    references_ids, to_json, cc_json, bcc_json, subject, body, attachments_json, message_id, \
    remote_draft_id, remote_message_id, state, updated_at, send_attempted_at";

/// Rail's own types as JSON; serialising them cannot fail.
fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".into())
}

/// Rail's own JSON; an unreadable value (a hand-edited store) reads as empty.
fn parsed<T: serde::de::DeserializeOwned + Default>(text: &str) -> T {
    serde_json::from_str(text).unwrap_or_default()
}

fn read_draft(r: &Row<'_>) -> rusqlite::Result<Draft> {
    let text = |i: usize| -> rusqlite::Result<String> { r.get(i) };
    Ok(Draft {
        id: r.get(0)?,
        account_id: r.get(1)?,
        kind: DraftKind::parse(&text(2)?),
        thread_id: r.get(3)?,
        remote_thread_id: r.get(4)?,
        in_reply_to: r.get(5)?,
        references: r.get(6)?,
        to: parsed(&text(7)?),
        cc: parsed(&text(8)?),
        bcc: parsed(&text(9)?),
        subject: text(10)?,
        body: text(11)?,
        attachments: parsed(&text(12)?),
        message_id: text(13)?,
        remote_draft_id: r.get(14)?,
        remote_message_id: r.get(15)?,
        state: DraftState::parse(&text(16)?),
        updated_at: r.get(17)?,
        send_attempted_at: r.get(18)?,
    })
}

fn read_summary(r: &Row<'_>) -> rusqlite::Result<DraftSummary> {
    Ok(DraftSummary {
        id: r.get(0)?,
        account_id: r.get(1)?,
        kind: DraftKind::parse(&r.get::<_, String>(2)?),
        subject: r.get(3)?,
        to: parsed(&r.get::<_, String>(4)?),
        state: DraftState::parse(&r.get::<_, String>(5)?),
        updated_at: r.get(6)?,
    })
}

/// A message's headers and addresses, without its attachments. `references_ids` holds the
/// JSON array the sync writer stores.
fn read_reply_head(r: &Row<'_>) -> rusqlite::Result<ReplySource> {
    let text = |i: usize| -> rusqlite::Result<String> { r.get(i) };
    let references: Option<String> = r.get(6)?;
    Ok(ReplySource {
        message: r.get(0)?,
        account_id: r.get(1)?,
        thread_id: r.get(2)?,
        remote_thread_id: text(3)?,
        remote_id: text(4)?,
        message_id: r.get(5)?,
        references: references.as_deref().map(parsed).unwrap_or_default(),
        subject: text(7)?,
        from: parsed(&text(8)?),
        reply_to: parsed(&text(9)?),
        to: parsed(&text(10)?),
        cc: parsed(&text(11)?),
        date: r.get(12)?,
        body_text: r.get(13)?,
        attachments: Vec::new(),
    })
}

/// Insert a draft; `token` is the create's client token (`None` outside `draft.save`).
fn insert_draft(
    conn: &Connection,
    w: &DraftWrite,
    token: Option<&str>,
    now: i64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO drafts (account_id, kind, thread_id, remote_thread_id, in_reply_to,
           references_ids, to_json, cc_json, bcc_json, subject, body, attachments_json,
           message_id, created_at, updated_at, client_token)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)",
        params![
            w.account_id,
            w.kind.as_str(),
            w.thread_id,
            w.remote_thread_id,
            w.in_reply_to,
            w.references,
            json(&w.to),
            json(&w.cc),
            json(&w.bcc),
            w.subject,
            w.body,
            json(&w.attachments),
            w.message_id,
            now,
            token
        ],
    )?;
    Ok(())
}

/// Rewrite one of the account's drafts; how many rows changed (0: no such draft there).
fn update_draft(
    conn: &Connection,
    id: DraftId,
    w: &DraftWrite,
    now: i64,
) -> Result<usize, StoreError> {
    Ok(conn.execute(
        "UPDATE drafts SET kind = ?2, thread_id = ?3, remote_thread_id = ?4, in_reply_to = ?5,
           references_ids = ?6, to_json = ?7, cc_json = ?8, bcc_json = ?9, subject = ?10,
           body = ?11, attachments_json = ?12, updated_at = ?13
         WHERE id = ?1 AND account_id = ?14",
        params![
            id,
            w.kind.as_str(),
            w.thread_id,
            w.remote_thread_id,
            w.in_reply_to,
            w.references,
            json(&w.to),
            json(&w.cc),
            json(&w.bcc),
            w.subject,
            w.body,
            json(&w.attachments),
            now,
            w.account_id
        ],
    )?)
}

/// What a save through the writer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftSaved {
    /// Created, updated, or (a create whose token was seen before) the draft that token made.
    Saved(DraftId),
    /// The id names no draft of the account.
    Missing,
    /// The draft is being sent: it changes only after an undo.
    Queued,
}

/// True while the draft is being sent: `queued`, or a live `send` row names it.
fn being_sent(conn: &Connection, id: DraftId) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM drafts WHERE id = ?1 AND state = 'queued')
             OR EXISTS (SELECT 1 FROM outbox WHERE draft_id = ?1 AND kind = 'send'
                          AND state IN ('pending', 'inflight'))",
        [id],
        |r| r.get(0),
    )?)
}

/// `draft_upsert` in the writer's outbox transaction. A create keeps its client token, so a
/// create sent again with that token answers the draft it made and changes nothing; an
/// update of a draft being sent is refused.
pub(super) fn save_in(
    conn: &Connection,
    w: &DraftWrite,
    token: &str,
    now: i64,
) -> Result<DraftSaved, StoreError> {
    if let Some(id) = w.id {
        let held: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM drafts WHERE id = ?1 AND account_id = ?2)",
            params![id, w.account_id],
            |r| r.get(0),
        )?;
        if !held {
            return Ok(DraftSaved::Missing);
        }
        if being_sent(conn, id)? {
            return Ok(DraftSaved::Queued);
        }
        update_draft(conn, id, w, now)?;
        return Ok(DraftSaved::Saved(id));
    }
    let known: Option<DraftId> = conn
        .query_row(
            "SELECT id FROM drafts WHERE client_token = ?1 AND account_id = ?2",
            params![token, w.account_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = known {
        return Ok(DraftSaved::Saved(id));
    }
    insert_draft(conn, w, Some(token), now)?;
    Ok(DraftSaved::Saved(conn.last_insert_rowid()))
}

/// `draft_delete` in the writer's outbox transaction, unless the draft is being sent (its
/// send row pending or inflight): false then, and nothing changes. A missing draft is gone
/// already: true.
pub(super) fn delete_in(conn: &Connection, id: DraftId) -> Result<bool, StoreError> {
    if being_sent(conn, id)? {
        return Ok(false);
    }
    conn.execute("DELETE FROM outbox WHERE draft_id = ?1", [id])?;
    conn.execute("DELETE FROM drafts WHERE id = ?1", [id])?;
    Ok(true)
}

impl Store {
    /// Create (`id: None`) or update one of the account's drafts; the draft's id, or `None` when
    /// `id` names no draft of that account. An update leaves `message_id`, the remote ids and
    /// the state as they are.
    pub fn draft_upsert(
        &mut self,
        w: &DraftWrite,
        now: i64,
    ) -> Result<Option<DraftId>, StoreError> {
        match w.id {
            Some(id) => Ok((update_draft(&self.conn, id, w, now)? > 0).then_some(id)),
            None => {
                insert_draft(&self.conn, w, None, now)?;
                Ok(Some(self.conn.last_insert_rowid()))
            }
        }
    }

    pub fn draft(&self, id: DraftId) -> Result<Option<Draft>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {DRAFT_COLUMNS} FROM drafts WHERE id = ?1"),
                [id],
                read_draft,
            )
            .optional()?)
    }

    /// The account's drafts, newest first.
    pub fn drafts_for_account(&self, account_id: i64) -> Result<Vec<DraftSummary>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, kind, subject, to_json, state, updated_at FROM drafts
             WHERE account_id = ?1 ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([account_id], read_summary)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Delete the draft and its outbox rows.
    pub fn draft_delete(&mut self, id: DraftId) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM outbox WHERE draft_id = ?1", [id])?;
        tx.execute("DELETE FROM drafts WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn draft_set_state(&mut self, id: DraftId, state: DraftState) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE drafts SET state = ?2 WHERE id = ?1",
            params![id, state.as_str()],
        )?;
        Ok(())
    }

    /// The Gmail draft id and the draft's message id at Gmail, once pushed or imported.
    pub fn draft_set_remote(
        &mut self,
        id: DraftId,
        draft: &str,
        message: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE drafts SET remote_draft_id = ?2, remote_message_id = ?3 WHERE id = ?1",
            params![id, draft, message],
        )?;
        Ok(())
    }

    /// The provider id of one of the account's stored messages: where a forwarded part is
    /// fetched from.
    pub fn message_remote_id(
        &self,
        account_id: i64,
        message: i64,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT m.remote_id FROM messages m JOIN threads t ON t.id = m.thread_id
                 WHERE m.id = ?1 AND t.account_id = ?2",
                params![message, account_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// False when the draft's stored attachment list does not parse (`draft` reads such a
    /// list as empty): a send must not go out without what the user attached.
    pub fn draft_attachments_readable(&self, id: DraftId) -> Result<bool, StoreError> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT attachments_json FROM drafts WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(text.is_none_or(|t| serde_json::from_str::<Vec<Attachment>>(&t).is_ok()))
    }

    /// The headers, addresses, body and attachments a reply or forward of `message` needs.
    pub fn reply_source(&self, message: i64) -> Result<Option<ReplySource>, StoreError> {
        let head = self
            .conn
            .query_row(
                "SELECT m.id, t.account_id, m.thread_id, t.remote_id, m.remote_id, m.message_id,
                        m.references_ids, m.subject, m.from_json, m.reply_to_json, m.to_json,
                        m.cc_json, m.date, m.body_text
                 FROM messages m JOIN threads t ON t.id = m.thread_id WHERE m.id = ?1",
                [message],
                read_reply_head,
            )
            .optional()?;
        let Some(mut source) = head else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT remote_part_id, filename, mime_type, size FROM attachments
             WHERE message_id = ?1 ORDER BY id",
        )?;
        source.attachments = stmt
            .query_map([message], |r| {
                Ok(ReplyAttachment {
                    part_id: r.get(0)?,
                    filename: r.get(1)?,
                    mime_type: r.get(2)?,
                    size: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(Some(source))
    }

    /// The newest message of `thread` (by date, then id): the one a reply or forward of the
    /// thread answers. `None` when the thread holds no message.
    pub fn latest_message_of(&self, thread: i64) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM messages WHERE thread_id = ?1 ORDER BY date DESC, id DESC LIMIT 1",
                [thread],
                |r| r.get(0),
            )
            .optional()?)
    }
}
