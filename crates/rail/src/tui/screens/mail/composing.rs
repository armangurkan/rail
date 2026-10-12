//! The mail screen's side of composing: what is pending between a key and the review, the
//! `draft.save` request, the editor's compose file, and the `Composing` a reply, a forward or
//! a resumed draft starts from.

use std::path::{Path, PathBuf};

use rail_core::ops::drafts::{DraftAttachment, ReplyTo};
use rail_core::ops::{
    DraftId, DraftSaveRequest, MAX_MESSAGE_BYTES, client_token, estimated_raw_size, too_large_text,
};
use rail_core::store::drafts::{Attachment, Draft, DraftKind, Recipient, ReplySource};

use super::{Focus, MailScreen};
use crate::tui::app::Effect;
use crate::tui::data::{ReplyTarget, SaveCall};
use crate::tui::keys::Action;
use crate::tui::screens::compose::model::{AttachmentRow, Composing, Headers};
use crate::tui::screens::compose::{Composer, file, prefill};
use crate::tui::text::escape;

/// What a `draft.save` answer leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum After {
    /// Open the editor on the saved draft.
    Edit,
    /// Show (or refresh) the review.
    Review,
    /// Save the draft to Gmail's Drafts once it is saved.
    Push,
}

/// The one composing request in flight; an answer that does not match it is stale. A send
/// or a push is kept apart, in `ComposeState::queuing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Pending {
    /// `r`, `R` or `f` asked the store for the message answered; `waiting` is the thread whose
    /// body fetch must answer first, `fetched` that one was asked for already.
    Source {
        seq: u64,
        kind: DraftKind,
        target: ReplyTarget,
        waiting: Option<i64>,
        fetched: bool,
    },
    /// `draft.save` is in flight; `before` is the attachment list to put back if it fails.
    Saving {
        seq: u64,
        after: After,
        before: Option<Vec<AttachmentRow>>,
    },
    /// The editor runs on the compose file.
    Editing,
    /// `draft.send` is in flight for `draft`.
    Sending { seq: u64, draft: DraftId },
    /// `draft.push` is in flight for `draft` under `client_token`.
    Pushing {
        seq: u64,
        draft: DraftId,
        client_token: String,
    },
    /// A draft is read to resume it.
    Resuming { seq: u64 },
    /// The drafts picker's list is read.
    Drafts { seq: u64 },
    /// `draft.import` is in flight for a Gmail draft; `then` is what its answer is for.
    Importing {
        seq: u64,
        then: super::drafts_flow::AfterImport,
    },
}

#[derive(Debug, Default)]
pub(crate) struct ComposeState {
    pub(super) seq: u64,
    pub(super) pending: Option<Pending>,
    /// The `draft.send` or `draft.push` in flight (`Sending` or `Pushing`). Kept apart from
    /// `pending`: closing its review and starting another message must not drop its answer,
    /// which makes the undo record.
    pub(super) queuing: Option<Pending>,
    /// The draft of the last send, kept so an undo can reopen its review.
    pub(super) last_sent: Option<Composing>,
    /// The compose file the editor works on, until it is parsed.
    pub(super) file: Option<PathBuf>,
    /// The text an editor left on a non-zero exit, while "Keep what you wrote?" asks.
    pub(super) kept: Option<String>,
    /// The error a failed send reopens its draft with.
    pub(super) reopen_error: Option<String>,
    /// The last `draft.save` failed: the store holds an older text than the review shows.
    pub(super) unsaved: bool,
}

/// Said when `y` or `s` is pressed while the last send or push is not answered yet.
pub(super) const QUEUING: &str = "Wait for the last message to be queued.";

/// Said when composing is asked for while a message is being written.
pub(super) const BUSY: &str = "Finish or close the message being written first.";

/// The prompts a new message opens with, before the editor.
pub(super) const OPENING_PROMPTS: [&str; 2] = ["To", "Subject"];

/// A recipient as the review and the compose file show it.
fn shown(recipients: &[Recipient]) -> Vec<String> {
    recipients
        .iter()
        .map(|r| prefill::display_parts(r.name.as_deref(), &r.address))
        .collect()
}

fn attachment_row(a: &Attachment) -> AttachmentRow {
    let (path, message, part) = match a {
        Attachment::File { path, .. } => (Some(path.clone()), None, None),
        Attachment::Part { message, part, .. } => (None, Some(*message), Some(part.clone())),
    };
    AttachmentRow {
        name: a.name().to_string(),
        mime: a.mime().to_string(),
        size: a.size(),
        path,
        message,
        part,
    }
}

/// An attachment as a save names it: a file by path, a stored part by message and part id.
/// A part not stored yet (a forward before its create) is named by nobody: the create takes
/// it from the store.
fn named(row: &AttachmentRow) -> Option<DraftAttachment> {
    match (&row.path, row.message, &row.part) {
        (Some(path), _, _) => Some(DraftAttachment::File { path: path.clone() }),
        (None, Some(message), Some(part)) => Some(DraftAttachment::Part {
            message,
            part: part.clone(),
        }),
        _ => None,
    }
}

/// The file at `path` can be attached to `c`: its size, or the refusal. The message is
/// counted as the daemon and the worker count it, attachments as encoded.
pub(super) fn attachable(path: &str, c: &Composing) -> Result<i64, String> {
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("attachments: {name:?} cannot be read: {e}"))?;
    if !meta.is_file() {
        return Err(format!("attachments: {name:?} is not a regular file"));
    }
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let sizes: Vec<usize> = c
        .attachments
        .iter()
        .map(|a| a.size)
        .chain([size])
        .map(|s| usize::try_from(s).unwrap_or(0))
        .collect();
    let estimated = estimated_raw_size(c.body.len(), &sizes);
    if estimated > MAX_MESSAGE_BYTES {
        return Err(format!("attachments: {}", too_large_text(estimated)));
    }
    Ok(size)
}

/// A compose file already on disk when the editor round for a saved draft starts.
enum Leftover {
    /// It holds text the store does not.
    Text(PathBuf),
    /// It could not be read: the editor gets it unchanged, with no claim about its text.
    Unreadable(PathBuf),
}

impl MailScreen {
    pub(super) fn next_compose_seq(&mut self) -> u64 {
        self.composing.seq += 1;
        self.composing.seq
    }

    /// Back to where the composer was opened from.
    pub(super) fn focus_back(&mut self) {
        self.focus = if self.reader.is_some() {
            Focus::Reader
        } else {
            Focus::List
        };
    }

    /// Show `composer` over the screen.
    pub(super) fn show_composer(&mut self, composer: Composer) {
        let keymap = &self.ctx.keymap;
        let labels = [
            Action::ComposeSend,
            Action::ComposePush,
            Action::ComposeEdit,
            Action::ComposeHeaders,
            Action::ComposeAttach,
            Action::ComposeRemove,
            Action::ComposeClose,
            Action::ComposeDiscard,
        ]
        .map(|a| keymap.label(a));
        self.compose = Some(composer.with_hint(&labels));
        self.composing.unsaved = false;
        self.focus = Focus::Compose;
    }

    /// Close the composer: its answers are dropped (except a send's or a push's, which still
    /// make the undo record) and the compose file goes.
    pub(super) fn hide_composer(&mut self) {
        self.compose = None;
        self.composing.unsaved = false;
        self.composing.pending = None;
        self.remove_compose_file();
        self.focus_back();
    }

    /// The position of the account with this store id.
    pub(super) fn account_index_of(&self, account_id: i64) -> Option<usize> {
        self.account_ids
            .iter()
            .position(|id| *id == Some(account_id))
            .filter(|&i| i < self.accounts.len())
    }

    pub(super) fn save_request(&self, c: &Composing) -> DraftSaveRequest {
        DraftSaveRequest {
            account: self.accounts[c.account_index].name.clone(),
            draft: c.draft,
            kind: c.kind,
            // Only the create names the message answered; an update keeps the threading the
            // draft was created with (that message may have left the store since).
            reply_to: c
                .reply_to
                .filter(|_| c.draft.is_none())
                .map(|message| ReplyTo { message }),
            to: c.headers.to.clone(),
            cc: c.headers.cc.clone(),
            bcc: c.headers.bcc.clone(),
            subject: c.headers.subject.clone(),
            body: c.body.clone(),
            // A create names files only; every save after it names each attachment kept.
            attachments: c
                .attachments
                .iter()
                .filter_map(named)
                .filter(|a| c.draft.is_some() || matches!(a, DraftAttachment::File { .. }))
                .collect(),
            client_token: client_token(),
        }
    }

    /// Save the composer's draft, then do `after` when the daemon answers; `before` is put
    /// back as the attachment list if the save fails.
    pub(super) fn save_draft(
        &mut self,
        after: After,
        before: Option<Vec<AttachmentRow>>,
    ) -> Vec<Effect> {
        let Some(c) = self.compose.as_ref().map(|c| c.composing().clone()) else {
            return Vec::new();
        };
        let request = self.save_request(&c);
        let seq = self.next_compose_seq();
        self.composing.pending = Some(Pending::Saving { seq, after, before });
        vec![self.ctx.link.draft_save(SaveCall { seq, request })]
    }

    /// The compose file a crashed session left for the saved draft in `c`, when it holds text
    /// the store does not: the editor opens it as it is. A file this session wrote itself
    /// (a reopen after a refused header or a failed save) is not a leftover. A file that
    /// cannot be read is kept too, rather than overwritten.
    fn leftover_file(&self, c: &Composing) -> Option<Leftover> {
        let path = file::path_of(&self.ctx.paths.compose_dir(), &c.draft?.to_string());
        if self.composing.file.as_ref() == Some(&path) {
            return None;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => (file::plain_line_ends(&text) != file::render(&c.headers, &c.body))
                .then_some(Leftover::Text(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "could not read the compose file");
                Some(Leftover::Unreadable(path))
            }
        }
    }

    /// Write the compose file (or keep the text a crash left in it) and hand the terminal to
    /// the editor.
    pub(super) fn open_editor(&mut self) -> Vec<Effect> {
        let Some(c) = self.compose.as_ref().map(Composer::composing) else {
            return Vec::new();
        };
        let key = c.draft.map_or_else(|| "new".to_string(), |d| d.to_string());
        let written = match self.leftover_file(c) {
            Some(Leftover::Text(path)) => {
                let text = "opened the text from your last editor session";
                self.status.notify(text, self.tick);
                Ok(path)
            }
            Some(Leftover::Unreadable(path)) => Ok(path),
            None => file::write(&self.ctx.paths.compose_dir(), &key, &c.headers, &c.body),
        };
        match written {
            Ok(path) => {
                self.composing.file = Some(path.clone());
                self.composing.pending = Some(Pending::Editing);
                vec![Effect::RunEditor {
                    command: self.ctx.editor.clone(),
                    path,
                }]
            }
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "could not write the compose file");
                let text = format!(
                    "Could not write the draft file: {}",
                    escape(&error.to_string())
                );
                self.status.error(text, self.ctx.clock.now(), self.tick);
                self.composing.pending = None;
                Vec::new()
            }
        }
    }

    /// The compose file is done with: remove it.
    pub(super) fn remove_compose_file(&mut self) {
        let Some(path) = self.composing.file.take() else {
            return;
        };
        if let Err(error) = std::fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(kind = ?error.kind(), "could not remove the compose file");
        }
    }

    /// The signature of account `index`, saying once why an unreadable one is left out.
    pub(super) fn signature(&mut self, index: usize) -> Option<String> {
        match self.ctx.signatures.get(index).cloned().flatten() {
            Some(Ok(text)) => Some(text),
            Some(Err(why)) => {
                let text = format!("The signature file could not be read: {}", escape(&why));
                self.status.notify(text, self.tick);
                None
            }
            None => None,
        }
    }

    /// A new message from account `index`: the signature only.
    pub(super) fn new_composing(&mut self, index: usize) -> Composing {
        let signature = self.signature(index);
        Composing {
            draft: None,
            account_index: index,
            account_address: self.accounts[index].address.clone(),
            kind: DraftKind::New,
            reply_to: None,
            headers: Headers::default(),
            body: prefill::with_signature("", signature.as_deref()),
            attachments: Vec::new(),
            message_id: None,
            errors: Vec::new(),
        }
    }

    /// A reply or forward of `source` from account `index`, with the signature.
    pub(super) fn composing_from(
        &mut self,
        kind: DraftKind,
        source: &ReplySource,
        index: usize,
    ) -> Composing {
        let me = self.accounts[index].address.clone();
        let zone = self.ctx.clock.zone().clone();
        let (headers, body) = match kind {
            DraftKind::Forward => prefill::forward(source),
            DraftKind::ReplyAll => prefill::reply(source, true, &me, &zone),
            DraftKind::Reply | DraftKind::New => prefill::reply(source, false, &me, &zone),
        };
        let signature = self.signature(index);
        let attachments = match kind {
            DraftKind::Forward => source
                .attachments
                .iter()
                .map(|a| AttachmentRow {
                    name: a.filename.clone(),
                    mime: a.mime_type.clone(),
                    size: a.size,
                    path: None,
                    message: Some(source.message),
                    part: Some(a.part_id.clone()),
                })
                .collect(),
            _ => Vec::new(),
        };
        Composing {
            draft: None,
            account_index: index,
            account_address: me,
            kind,
            reply_to: Some(source.message),
            headers,
            body: prefill::with_signature(&body, signature.as_deref()),
            attachments,
            message_id: None,
            errors: Vec::new(),
        }
    }

    /// A stored draft as the composer holds it; `None` when its account is not configured.
    /// It names no message answered: its saves are updates, which keep the stored threading.
    pub(super) fn composing_of(&self, d: Draft) -> Option<Composing> {
        let index = self.account_index_of(d.account_id)?;
        Some(Composing {
            draft: Some(d.id),
            account_index: index,
            account_address: self.accounts[index].address.clone(),
            kind: d.kind,
            reply_to: None,
            headers: Headers {
                to: shown(&d.to),
                cc: shown(&d.cc),
                bcc: shown(&d.bcc),
                subject: d.subject,
            },
            body: d.body,
            attachments: d.attachments.iter().map(attachment_row).collect(),
            message_id: Some(d.message_id),
            errors: Vec::new(),
        })
    }
}
