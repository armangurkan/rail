//! Composing from the mail screen's keys: `c` (asking for the account in the unified inbox),
//! `r`, `R` and `f` (fetching a missing body first), the composer's own keys and outcomes,
//! and the editor round trip with its failure paths.

use crossterm::event::{KeyCode, KeyEvent};
use rail_core::ops::client_token;
use rail_core::store::drafts::{DraftKind, ReplySource};

use super::composing::{After, BUSY, OPENING_PROMPTS, Pending, attachable};
use super::{Focus, MailScreen};
use crate::tui::app::Effect;
use crate::tui::data::{ReadError, ReplyTarget, ScopeChoice};
use crate::tui::keys::{Action, Resolved, View};
use crate::tui::modals::{Command, Confirm, Modal, Picker, PickerItem, PickerKind};
use crate::tui::screens::compose::model::AttachmentRow;
use crate::tui::screens::compose::{ComposeOutcome, Composer, file};
use crate::tui::text::escape;

impl MailScreen {
    /// `c`, `r`, `R`, `f` in the list or the reader; `None` for every other action.
    pub(super) fn compose_action(&mut self, action: Action) -> Option<Vec<Effect>> {
        if !matches!(self.focus, Focus::List | Focus::Reader) {
            return None;
        }
        let kind = match action {
            Action::ListCompose => return Some(self.start_compose()),
            Action::ListReply => DraftKind::Reply,
            Action::ListReplyAll => DraftKind::ReplyAll,
            Action::ListForward => DraftKind::Forward,
            _ => return None,
        };
        Some(self.start_reply(kind))
    }

    /// `c`: the scope's account, or the account picker in the unified inbox.
    pub(super) fn start_compose(&mut self) -> Vec<Effect> {
        if self.compose.is_some() {
            self.status.notify(BUSY, self.tick);
            return Vec::new();
        }
        if self.accounts.is_empty() {
            self.status.notify("No account is configured.", self.tick);
            return Vec::new();
        }
        let index = match &self.scope {
            ScopeChoice::Unified if self.accounts.len() > 1 => {
                let items = self
                    .accounts
                    .iter()
                    .enumerate()
                    .map(|(i, a)| PickerItem {
                        id: i64::try_from(i).unwrap_or(i64::MAX),
                        name: format!("{} · {}", a.name, a.address),
                        checked: false,
                        swatch: Vec::new(),
                    })
                    .collect();
                self.modal = Some(Modal::Picker(Picker::new(PickerKind::Account, items)));
                return Vec::new();
            }
            ScopeChoice::Unified => 0,
            ScopeChoice::Account(index) | ScopeChoice::Folder { account: index, .. } => *index,
        };
        self.start_new(index)
    }

    /// A new message from account `index`: the `To` and `Subject` prompts first.
    pub(super) fn start_new(&mut self, index: usize) -> Vec<Effect> {
        if self.compose.is_some() || index >= self.accounts.len() {
            return Vec::new();
        }
        // A reply or draft read still in flight must not replace this message.
        self.composing.pending = None;
        let composing = self.new_composing(index);
        self.show_composer(Composer::new_with_prompts(composing, &OPENING_PROMPTS));
        Vec::new()
    }

    /// `r`, `R`, `f`: the open thread's latest message, else the cursor thread's.
    fn start_reply(&mut self, kind: DraftKind) -> Vec<Effect> {
        if self.compose.is_some() {
            self.status.notify(BUSY, self.tick);
            return Vec::new();
        }
        let target = match (self.focus, self.reader.as_ref()) {
            (Focus::Reader, Some(reader)) => reader
                .detail()
                .messages
                .last()
                .map(|m| ReplyTarget::Message(m.id)),
            _ => self
                .list
                .selected()
                .map(|r| ReplyTarget::Thread(r.key.thread_id)),
        };
        let Some(target) = target else {
            self.status.notify("No message to answer.", self.tick);
            return Vec::new();
        };
        self.read_source(kind, target, false)
    }

    fn read_source(&mut self, kind: DraftKind, target: ReplyTarget, fetched: bool) -> Vec<Effect> {
        let seq = self.next_compose_seq();
        self.composing.pending = Some(Pending::Source {
            seq,
            kind,
            target,
            waiting: None,
            fetched,
        });
        vec![self.ctx.store.reply_source(seq, target)]
    }

    pub(super) fn reply_source_read(
        &mut self,
        seq: u64,
        result: Result<Option<Box<ReplySource>>, ReadError>,
    ) -> Vec<Effect> {
        let Some(Pending::Source {
            seq: wanted,
            kind,
            fetched,
            ..
        }) = self.composing.pending.clone()
        else {
            return Vec::new();
        };
        if wanted != seq || self.compose.is_some() {
            return Vec::new();
        }
        let source = match result {
            Ok(Some(source)) => source,
            Ok(None) => {
                self.composing.pending = None;
                let text = "That message is no longer in the store.";
                self.status.notify(text, self.tick);
                return Vec::new();
            }
            Err(error) => {
                self.composing.pending = None;
                self.side_read_failed(error, "message");
                return Vec::new();
            }
        };
        let Some(index) = self.account_index_of(source.account_id) else {
            self.composing.pending = None;
            let text = "This account is no longer configured.";
            self.status.notify(text, self.tick);
            return Vec::new();
        };
        if source.body_text.is_none() {
            if !fetched {
                return self.fetch_for_reply(&source, index);
            }
            let text = "The message's text is not available: nothing is quoted.";
            self.status.notify(text, self.tick);
        }
        self.composing.pending = None;
        let composing = self.composing_from(kind, &source, index);
        self.show_composer(Composer::new(composing));
        self.save_draft(After::Edit, None)
    }

    /// The answered message's body is not stored yet: fetch it, then read it again.
    fn fetch_for_reply(&mut self, source: &ReplySource, index: usize) -> Vec<Effect> {
        if let Some(Pending::Source {
            waiting, fetched, ..
        }) = self.composing.pending.as_mut()
        {
            *waiting = Some(source.thread_id);
            *fetched = true;
        }
        self.status.notify("fetching the message…", self.tick);
        let account = self.accounts[index].name.clone();
        let ids = vec![source.remote_id.clone()];
        vec![self.ctx.link.fetch_bodies(source.thread_id, account, ids)]
    }

    /// A body fetch answered: the reply waiting on that thread reads its message again.
    pub(super) fn body_for_reply(&mut self, thread_id: i64) -> Vec<Effect> {
        match self.composing.pending.clone() {
            Some(Pending::Source {
                kind,
                target,
                waiting: Some(thread),
                ..
            }) if thread == thread_id => self.read_source(kind, target, true),
            _ => Vec::new(),
        }
    }

    /// A key while the composer is open: the keymap reads it unless a prompt or the discard
    /// question takes raw keys; a global action runs as elsewhere.
    pub(super) fn compose_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(composer) = self.compose.as_ref() else {
            return Vec::new();
        };
        let action = if composer.captures_text() {
            None
        } else {
            let views = [View::Compose, View::Global];
            match self.ctx.keymap.resolve(&views, &mut self.chord, key) {
                Resolved::Action(action) => Some(action),
                Resolved::Pending => return Vec::new(),
                Resolved::Unbound => None,
            }
        };
        if let Some(action) = action.filter(|a| !a.views().contains(&View::Compose)) {
            return self.act(action);
        }
        let outcome = match self.compose.as_mut() {
            Some(composer) => composer.key(action, key),
            None => return Vec::new(),
        };
        self.compose_outcome(outcome)
    }

    /// A composer action from the palette: as its key would.
    pub(super) fn compose_act(&mut self, action: Action) -> Vec<Effect> {
        let outcome = match self.compose.as_mut() {
            Some(composer) if !composer.captures_text() => {
                composer.key(Some(action), KeyEvent::from(KeyCode::Null))
            }
            _ => return Vec::new(),
        };
        self.compose_outcome(outcome)
    }

    fn compose_outcome(&mut self, outcome: ComposeOutcome) -> Vec<Effect> {
        match outcome {
            ComposeOutcome::Stay => Vec::new(),
            ComposeOutcome::Edit => self.open_editor(),
            ComposeOutcome::Send => self.send_draft(),
            ComposeOutcome::Close => self.close_compose(),
            ComposeOutcome::Discard => self.discard_compose(),
            ComposeOutcome::Attach(path) => self.attach(path),
            ComposeOutcome::Remove(index) => self.remove_attachment(index),
            ComposeOutcome::HeadersChanged => {
                let first = self
                    .compose
                    .as_ref()
                    .is_some_and(|c| c.composing().draft.is_none());
                self.save_draft(if first { After::Edit } else { After::Review }, None)
            }
            ComposeOutcome::SuggestionsWanted(prefix) => match self.compose.as_mut() {
                Some(composer) => {
                    let seq = composer.next_seq();
                    vec![self.ctx.store.suggest(seq, prefix)]
                }
                None => Vec::new(),
            },
        }
    }

    /// `q`: close the review; the draft stays.
    fn close_compose(&mut self) -> Vec<Effect> {
        if self.composing.unsaved {
            let lines = vec![
                "The last save failed: closing now loses what changed since.".to_string(),
                "Yes closes; no keeps the review open.".to_string(),
            ];
            self.modal = Some(Modal::Confirm(Confirm::new(
                "Discard the unsaved changes?",
                lines,
                Command::DiscardUnsaved,
            )));
            return Vec::new();
        }
        let saved = self
            .compose
            .as_ref()
            .is_some_and(|c| c.composing().draft.is_some());
        self.hide_composer();
        let text = if saved {
            "draft kept"
        } else {
            "The message was not saved."
        };
        self.status.notify(text, self.tick);
        Vec::new()
    }

    /// `d` answered yes, or Esc in a new message's opening prompts (nothing saved yet).
    fn discard_compose(&mut self) -> Vec<Effect> {
        match self.compose.as_ref().and_then(|c| c.composing().draft) {
            Some(draft) => vec![self.ctx.link.draft_discard(draft)],
            None => {
                self.hide_composer();
                Vec::new()
            }
        }
    }

    fn attach(&mut self, path: String) -> Vec<Effect> {
        let Some(composer) = self.compose.as_mut() else {
            return Vec::new();
        };
        let size = match attachable(&path, composer.composing()) {
            Ok(size) => size,
            Err(why) => {
                composer.set_error(why.clone());
                self.status.notify(escape(&why), self.tick);
                return Vec::new();
            }
        };
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let attachments = &mut composer.composing_mut().attachments;
        let before = attachments.clone();
        attachments.push(AttachmentRow {
            name,
            mime: String::new(),
            size,
            path: Some(path),
            message: None,
            part: None,
        });
        self.save_draft(After::Review, Some(before))
    }

    fn remove_attachment(&mut self, index: usize) -> Vec<Effect> {
        let Some(composer) = self.compose.as_mut() else {
            return Vec::new();
        };
        let attachments = &mut composer.composing_mut().attachments;
        if index >= attachments.len() {
            return Vec::new();
        }
        let before = attachments.clone();
        attachments.remove(index);
        self.save_draft(After::Review, Some(before))
    }

    /// `y`: queue the saved draft; the review closes when the daemon has it.
    fn send_draft(&mut self) -> Vec<Effect> {
        let Some(draft) = self.compose.as_ref().and_then(|c| c.composing().draft) else {
            self.status.notify("The draft is not saved yet.", self.tick);
            return Vec::new();
        };
        if self.composing.pending.is_some() {
            self.status.notify("Wait for the draft to save.", self.tick);
            return Vec::new();
        }
        if self
            .compose
            .as_ref()
            .is_some_and(|c| c.composing().recipients().is_empty())
        {
            let key = self.ctx.keymap.label(Action::ComposeHeaders);
            let text = format!("Add a recipient first: {key} changes To, Cc and Bcc.");
            self.status.notify(text, self.tick);
            return Vec::new();
        }
        if self.composing.unsaved {
            let text = "The draft could not be saved: fix it before sending.";
            self.status.notify(text, self.tick);
            return Vec::new();
        }
        let seq = self.next_compose_seq();
        self.composing.pending = Some(Pending::Sending { seq });
        vec![self.ctx.link.draft_send(seq, draft, client_token())]
    }

    /// The editor exited (or could not start).
    pub(super) fn editor_done(&mut self, result: Result<i32, String>) -> Vec<Effect> {
        if self.composing.pending != Some(Pending::Editing) {
            return Vec::new();
        }
        self.composing.pending = None;
        let code = match result {
            Ok(code) => code,
            Err(why) => {
                self.remove_compose_file();
                tracing::warn!("the editor could not run");
                self.status
                    .error(escape(&why), self.ctx.clock.now(), self.tick);
                return Vec::new();
            }
        };
        let Some(path) = self.composing.file.clone() else {
            return Vec::new();
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                self.remove_compose_file();
                let text = format!(
                    "Could not read the draft file back: {}",
                    escape(&error.to_string())
                );
                self.status.error(text, self.ctx.clock.now(), self.tick);
                return Vec::new();
            }
        };
        if code != 0 {
            return self.ask_to_keep(text, code);
        }
        self.apply_file_text(text)
    }

    /// A non-zero exit: "Keep what you wrote?"; the file stays until it is answered.
    fn ask_to_keep(&mut self, text: String, code: i32) -> Vec<Effect> {
        self.composing.kept = Some(text);
        let lines = vec![
            format!("The editor exited with status {code}."),
            "Yes keeps the text and saves the draft; no keeps the draft as it was.".to_string(),
        ];
        self.modal = Some(Modal::Confirm(Confirm::new(
            "Keep what you wrote?",
            lines,
            Command::KeepText,
        )));
        Vec::new()
    }

    /// The compose file's text: into the draft and saved, or its error shown and the editor
    /// opened on it again.
    fn apply_file_text(&mut self, text: String) -> Vec<Effect> {
        match file::parse(&text) {
            // The file stays until a save succeeds: it holds the text if the save fails.
            Ok((headers, body)) => {
                let Some(composer) = self.compose.as_mut() else {
                    return Vec::new();
                };
                let composing = composer.composing_mut();
                composing.headers = headers;
                composing.body = body;
                self.save_draft(After::Review, None)
            }
            Err(error) => {
                let text = format!(
                    "The draft file has an error on line {}: {}",
                    error.line,
                    escape(&error.message)
                );
                self.status.error(text, self.ctx.clock.now(), self.tick);
                let Some(path) = self.composing.file.clone() else {
                    return Vec::new();
                };
                self.composing.pending = Some(Pending::Editing);
                vec![Effect::RunEditor {
                    command: self.ctx.editor.clone(),
                    path,
                }]
            }
        }
    }

    /// The account picker, the drafts picker and "Keep what you wrote?" answered; any other
    /// command is handed back.
    pub(super) fn compose_command(&mut self, command: Command) -> Result<Vec<Effect>, Command> {
        Ok(match command {
            Command::Account(index) => self.start_new(index),
            Command::ResumeDraft(draft) => self.resume_draft(draft),
            Command::KeepText => match self.composing.kept.take() {
                Some(text) => self.apply_file_text(text),
                None => Vec::new(),
            },
            Command::DiscardUnsaved => {
                self.hide_composer();
                let text = "unsaved changes discarded";
                self.status.notify(text, self.tick);
                Vec::new()
            }
            other => return Err(other),
        })
    }

    /// A modal closed without an answer: "Keep what you wrote?" answered no drops the text.
    pub(super) fn compose_modal_closed(&mut self) {
        if self.composing.kept.take().is_some() {
            self.remove_compose_file();
        }
    }
}
