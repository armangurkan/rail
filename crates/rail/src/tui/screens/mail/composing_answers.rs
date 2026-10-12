//! The answers composing waits for: `draft.save`, `draft.send` (and its undo reopening the
//! review), `draft.push`, `draft.discard`, the address suggestions, a send the daemon gave up on, and the
//! drafts picker with the draft it resumes.

use rail_core::ops::{
    DraftId, DraftPushResponse, DraftSaveResponse, DraftSendResponse, OpError, RowToken,
};
use rail_core::store::drafts::{Draft, DraftSummary};

use super::composing::{After, BUSY, Pending};
use super::{MailScreen, error_line};
use crate::tui::app::Effect;
use crate::tui::data::{MailEvent, ReadError, ScopeChoice};
use crate::tui::modals::{Modal, Picker, PickerItem, PickerKind};
use crate::tui::screens::compose::Composer;
use crate::tui::screens::compose::model::AttachmentRow;
use crate::tui::screens::compose::prefill;

impl MailScreen {
    /// The composing answers; every other event is ignored.
    pub(super) fn compose_event(&mut self, event: MailEvent) -> Vec<Effect> {
        match event {
            MailEvent::DraftSaved { seq, result } => self.draft_saved(seq, result),
            MailEvent::DraftSent {
                seq,
                draft,
                client_token,
                result,
            } => self.draft_sent(seq, draft, client_token, result),
            MailEvent::DraftPushed {
                seq,
                draft,
                client_token,
                result,
            } => self.draft_pushed(seq, draft, client_token, result),
            MailEvent::DraftDiscarded { draft, result } => self.draft_discarded(draft, result),
            MailEvent::Suggestions { seq, result } => {
                match (result, self.compose.as_mut()) {
                    (Ok(entries), Some(composer)) => composer.prompt_suggestions(seq, entries),
                    (Ok(_), None) => {}
                    // Asked on every key: a failure is logged, not shown each time.
                    (Err(_), _) => tracing::warn!("an address suggestion read failed"),
                }
                Vec::new()
            }
            MailEvent::ReplySource { seq, result } => self.reply_source_read(seq, result),
            MailEvent::Drafts { seq, result } => self.drafts_read(seq, result),
            MailEvent::Draft { seq, result } => self.draft_read(seq, result),
            MailEvent::EditorDone(result) => self.editor_done(result),
            _ => Vec::new(),
        }
    }

    fn draft_saved(&mut self, seq: u64, result: Result<DraftSaveResponse, OpError>) -> Vec<Effect> {
        let Some(Pending::Saving {
            seq: wanted,
            after,
            before,
        }) = self.composing.pending.clone()
        else {
            return Vec::new();
        };
        if wanted != seq || self.compose.is_none() {
            return Vec::new();
        }
        self.composing.pending = None;
        let saved = match result {
            Ok(answer) => Some(self.save_answered(answer)),
            Err(error) => {
                self.save_failed(&error, before);
                None
            }
        };
        match after {
            // Nothing is lost when the save failed: the editor works on the text in memory.
            After::Edit => self.open_editor(),
            After::Review => Vec::new(),
            // A failed save pushes nothing: its error is on the review.
            After::Push => saved.map_or_else(Vec::new, |draft| self.start_push(draft)),
        }
    }

    /// The saved draft's id.
    fn save_answered(&mut self, answer: DraftSaveResponse) -> DraftId {
        self.status.answered();
        self.composing.unsaved = false;
        self.remove_compose_file();
        let draft = answer.draft;
        let Some(composer) = self.compose.as_mut() else {
            return draft;
        };
        let composing = composer.composing_mut();
        composing.draft = Some(answer.draft);
        composing.message_id = Some(answer.message_id);
        composing.errors.clear();
        composing.attachments = answer
            .attachments
            .into_iter()
            .map(|a| AttachmentRow {
                name: a.name,
                mime: a.mime,
                size: a.size,
                path: a.path,
                message: a.message,
                part: a.part,
            })
            .collect();
        draft
    }

    fn save_failed(&mut self, error: &OpError, before: Option<Vec<AttachmentRow>>) {
        tracing::warn!(code = ?error.code, "draft.save failed");
        self.status.failed(error);
        let line = error_line(error);
        if let Some(composer) = self.compose.as_mut() {
            if let Some(before) = before {
                composer.composing_mut().attachments = before;
            } else {
                self.composing.unsaved = true;
            }
            composer.set_error(line.clone());
        }
        let text = format!("Could not save the draft: {line}");
        self.status.error(text, self.ctx.clock.now(), self.tick);
    }

    fn draft_sent(
        &mut self,
        seq: u64,
        draft: DraftId,
        client_token: String,
        result: Result<DraftSendResponse, OpError>,
    ) -> Vec<Effect> {
        if self.composing.pending != Some(Pending::Sending { seq }) {
            return Vec::new();
        }
        self.composing.pending = None;
        match result {
            Ok(answer) => {
                self.status.answered();
                let ours = self
                    .compose
                    .as_ref()
                    .is_some_and(|c| c.composing().draft == Some(draft));
                self.composing.last_sent = None;
                if ours {
                    self.composing.last_sent = self.compose.as_ref().map(|c| {
                        let mut composing = c.composing().clone();
                        composing.errors.clear();
                        composing
                    });
                    self.hide_composer();
                }
                self.sent_record(RowToken {
                    row: answer.row,
                    client_token,
                });
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, "draft.send failed");
                self.status.failed(&error);
                let line = error_line(&error);
                if let Some(composer) = self.compose.as_mut() {
                    composer.set_error(line.clone());
                }
                let text = format!("Could not send: {line}");
                self.status.error(text, self.ctx.clock.now(), self.tick);
            }
        }
        Vec::new()
    }

    fn draft_pushed(
        &mut self,
        seq: u64,
        draft: DraftId,
        client_token: String,
        result: Result<DraftPushResponse, OpError>,
    ) -> Vec<Effect> {
        let wanted = Pending::Pushing {
            seq,
            draft,
            client_token: client_token.clone(),
        };
        if self.composing.pending.as_ref() != Some(&wanted) {
            return Vec::new();
        }
        self.composing.pending = None;
        let ours = self
            .compose
            .as_ref()
            .is_some_and(|c| c.composing().draft == Some(draft));
        match result {
            Ok(answer) => {
                self.status.answered();
                if ours {
                    self.hide_composer();
                }
                self.pushed_record(RowToken {
                    row: answer.row,
                    client_token,
                });
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, "draft.push failed");
                self.status.failed(&error);
                let line = error_line(&error);
                if let Some(composer) = self.compose.as_mut().filter(|_| ours) {
                    composer.set_error(line.clone());
                }
                let text = format!("Could not save to Gmail: {line}");
                self.status.error(text, self.ctx.clock.now(), self.tick);
            }
        }
        Vec::new()
    }

    /// A send was undone: its review opens again, unless another message is being written.
    pub(super) fn reopen_sent(&mut self) {
        if self.compose.is_some() {
            return;
        }
        if let Some(composing) = self.composing.last_sent.take() {
            self.show_composer(Composer::new(composing));
        }
    }

    fn draft_discarded(&mut self, draft: DraftId, result: Result<(), OpError>) -> Vec<Effect> {
        match result {
            Ok(()) => {
                let ours = self
                    .compose
                    .as_ref()
                    .is_some_and(|c| c.composing().draft == Some(draft));
                if ours {
                    self.hide_composer();
                }
                self.status.notify("draft discarded", self.tick);
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, "draft.discard failed");
                self.status.failed(&error);
                let text = format!("Could not discard the draft: {}", error_line(&error));
                self.status.error(text, self.ctx.clock.now(), self.tick);
            }
        }
        Vec::new()
    }

    /// The daemon gave up sending `draft`: its review opens with the error, unless another
    /// message is being written.
    pub(super) fn send_failed(&mut self, draft: DraftId, error: &OpError) -> Vec<Effect> {
        if self.compose.is_some() {
            return Vec::new();
        }
        self.composing.reopen_error = Some(error_line(error));
        self.resume_draft(draft)
    }

    /// Open a stored draft in the composer.
    pub(super) fn resume_draft(&mut self, draft: DraftId) -> Vec<Effect> {
        if self.compose.is_some() {
            self.status.notify(BUSY, self.tick);
            return Vec::new();
        }
        let seq = self.next_compose_seq();
        self.composing.pending = Some(Pending::Resuming { seq });
        vec![self.ctx.store.draft(seq, draft)]
    }

    fn draft_read(
        &mut self,
        seq: u64,
        result: Result<Option<Box<Draft>>, ReadError>,
    ) -> Vec<Effect> {
        if self.composing.pending != Some(Pending::Resuming { seq }) || self.compose.is_some() {
            return Vec::new();
        }
        self.composing.pending = None;
        let error = self.composing.reopen_error.take();
        match result {
            Ok(Some(read)) => match self.composing_of(*read) {
                Some(mut composing) => {
                    composing.errors = error.into_iter().collect();
                    self.show_composer(Composer::new(composing));
                }
                None => {
                    let text = "This draft's account is no longer configured.";
                    self.status.notify(text, self.tick);
                }
            },
            Ok(None) => {
                let text = "That draft is no longer in the store.";
                self.status.notify(text, self.tick);
            }
            Err(error) => {
                self.side_read_failed(error, "draft");
            }
        }
        Vec::new()
    }

    /// `:drafts`: the scope's account's drafts (every account's in the unified inbox).
    pub(super) fn drafts_picker(&mut self) -> Vec<Effect> {
        if self.compose.is_some() {
            self.status.notify(BUSY, self.tick);
            return Vec::new();
        }
        let ids: Vec<i64> = match &self.scope {
            ScopeChoice::Unified => self.account_ids.iter().flatten().copied().collect(),
            ScopeChoice::Account(index) | ScopeChoice::Folder { account: index, .. } => self
                .account_ids
                .get(*index)
                .copied()
                .flatten()
                .into_iter()
                .collect(),
        };
        let seq = self.next_compose_seq();
        self.composing.pending = Some(Pending::Drafts { seq });
        vec![self.ctx.store.drafts(seq, ids)]
    }

    fn drafts_read(
        &mut self,
        seq: u64,
        result: Result<Vec<DraftSummary>, ReadError>,
    ) -> Vec<Effect> {
        if self.composing.pending != Some(Pending::Drafts { seq }) {
            return Vec::new();
        }
        self.composing.pending = None;
        match result {
            Ok(drafts) if drafts.is_empty() => self.status.notify("No drafts.", self.tick),
            Ok(drafts) => {
                let items = drafts.iter().map(|d| self.draft_item(d)).collect();
                self.modal = Some(Modal::Picker(Picker::new(PickerKind::Draft, items)));
            }
            Err(error) => {
                self.side_read_failed(error, "drafts");
            }
        }
        Vec::new()
    }

    /// `subject · to · when`, raw: the picker escapes names when it draws them.
    fn draft_item(&self, d: &DraftSummary) -> PickerItem {
        let subject = if d.subject.trim().is_empty() {
            "(no subject)"
        } else {
            d.subject.as_str()
        };
        let to =
            d.to.iter()
                .map(|r| prefill::display_parts(r.name.as_deref(), &r.address))
                .collect::<Vec<_>>()
                .join(", ");
        let when = self.ctx.clock.short(d.updated_at);
        PickerItem {
            id: d.id,
            name: format!("{subject} · {to} · {when}"),
            checked: false,
            swatch: Vec::new(),
        }
    }
}
