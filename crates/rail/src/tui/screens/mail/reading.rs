//! The mail screen's reader side: opening a thread, fetching its bodies, the reader's keys,
//! links and attachments.

use rail_core::ops::{BodyFetchResponse, ErrorCode, MAX_BODY_FETCH, OpError};
use rail_core::store::view::ThreadDetail;

use super::{Focus, Glyphs, MailScreen, error_line};
use crate::tui::app::Effect;
use crate::tui::data::ReadError;
use crate::tui::keys::Action;
use crate::tui::modals::{
    AttachmentList, AttachmentRow, Command, Confirm, LinkPrompt, Modal, ModalOutcome,
};
use crate::tui::screens::reader::{Reader, ReaderOutcome};
use crate::tui::text::{escape, fit, safe_link};

/// How much of a refused address the status bar repeats.
const SHOWN_LINK_CHARS: usize = 120;

impl MailScreen {
    pub(super) fn read_thread(&mut self, thread_id: i64) -> Effect {
        let seq = self.next_seq();
        self.latest_thread = seq;
        self.ctx.store.thread(seq, thread_id)
    }

    pub(super) fn open_selected(&mut self) -> Vec<Effect> {
        let Some(thread_id) = self.list.selected().map(|r| r.key.thread_id) else {
            return Vec::new();
        };
        vec![self.read_thread(thread_id)]
    }

    pub(super) fn thread_read(
        &mut self,
        thread_id: i64,
        result: Result<Option<ThreadDetail>, ReadError>,
    ) -> Vec<Effect> {
        match result {
            Ok(Some(detail)) => {
                match self.reader.as_mut().filter(|r| r.thread_id() == thread_id) {
                    Some(reader) => reader.refresh(detail),
                    None => {
                        let glyphs = Glyphs::of(self.ctx.icons);
                        self.reader = Some(Reader::new(detail, self.ctx.clock.clone(), glyphs));
                    }
                }
                self.focus = Focus::Reader;
                self.fetch_wanted()
            }
            Ok(None) => {
                self.close_reader();
                self.status
                    .notify("That thread is no longer in the store.", self.tick);
                Vec::new()
            }
            Err(error) => {
                self.read_failed(error);
                Vec::new()
            }
        }
    }

    /// One `body.fetch` per `MAX_BODY_FETCH` of the open thread's uncached messages.
    pub(crate) fn fetch_wanted(&mut self) -> Vec<Effect> {
        let Some(reader) = self.reader.as_mut() else {
            return Vec::new();
        };
        let wanted = reader.wanted();
        if wanted.is_empty() {
            return Vec::new();
        }
        let ids: Vec<i64> = wanted.iter().map(|(id, _)| *id).collect();
        let account = self
            .account_ids
            .iter()
            .position(|id| *id == Some(reader.account_id()))
            .and_then(|i| self.accounts.get(i))
            .map(|a| a.name.clone());
        reader.mark_fetching(&ids);
        let Some(account) = account else {
            let gone = OpError::not_found("this account is no longer configured");
            reader.fetch_failed(&gone);
            return Vec::new();
        };
        let thread = reader.thread_id();
        wanted
            .chunks(MAX_BODY_FETCH)
            .map(|chunk| {
                let ids = chunk.iter().map(|(_, remote)| remote.clone()).collect();
                self.ctx.link.fetch_bodies(thread, account.clone(), ids)
            })
            .collect()
    }

    pub(super) fn bodies_answered(
        &mut self,
        thread_id: i64,
        result: Result<BodyFetchResponse, OpError>,
    ) -> Vec<Effect> {
        let mut effects = self.dirty();
        let Some(reader) = self.reader.as_mut().filter(|r| r.thread_id() == thread_id) else {
            return effects;
        };
        match result {
            Ok(response) => {
                for failure in reader.fetched(&response) {
                    self.login_hint(&failure);
                }
                effects.push(self.read_thread(thread_id));
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, "a body fetch failed");
                reader.fetch_failed(&error);
            }
        }
        effects
    }

    /// NeedsAuth: the account's login hint in the status bar, once per session.
    fn login_hint(&mut self, failure: &OpError) {
        if failure.code != ErrorCode::Auth {
            return;
        }
        let account = self
            .reader
            .as_ref()
            .and_then(|r| {
                self.account_ids
                    .iter()
                    .position(|id| *id == Some(r.account_id()))
            })
            .and_then(|i| self.accounts.get(i))
            .map(|a| a.name.clone())
            .unwrap_or_default();
        if self.status.first_login_hint(&account) {
            self.status
                .error(error_line(failure), self.ctx.clock.now(), self.tick);
        }
    }

    pub(super) fn reader_action(&mut self, action: Action) -> Vec<Effect> {
        let Some(reader) = self.reader.as_mut() else {
            self.focus = Focus::List;
            return Vec::new();
        };
        match reader.act(action) {
            ReaderOutcome::Stay => {}
            ReaderOutcome::Back => self.close_reader(),
            ReaderOutcome::FollowLink => self.follow_link(),
            ReaderOutcome::Attachments => self.attachments(),
            // Task 9 opens the HTML in the browser.
            ReaderOutcome::OpenBrowser => {}
        }
        Vec::new()
    }

    pub(super) fn close_reader(&mut self) {
        self.reader = None;
        self.modal = None;
        // An answer to a read still in flight must not reopen the thread.
        self.latest_thread = self.next_seq();
        self.focus = Focus::List;
    }

    fn follow_link(&mut self) {
        let Some(reader) = self.reader.as_ref() else {
            return;
        };
        match reader.links_at_cursor() {
            None => self.status.notify("This message has no links.", self.tick),
            Some((links, numbers)) if numbers.len() == 1 => self.confirm_link(&links, numbers[0]),
            Some((links, _)) => self.modal = Some(Modal::LinkNumber(LinkPrompt::new(links))),
        }
    }

    /// Show link `number`'s address before opening it; refuse one Rail would not open.
    fn confirm_link(&mut self, links: &[String], number: usize) {
        let Some(address) = links.get(number.wrapping_sub(1)) else {
            return;
        };
        match safe_link(address) {
            Some(url) => {
                let lines = vec![url.as_str().to_string()];
                self.modal = Some(Modal::Confirm(Confirm::new(
                    "Open this link?",
                    lines,
                    Command::OpenLink(url),
                )));
            }
            None => {
                let text = format!(
                    "Rail does not open this link: {}",
                    fit(address, SHOWN_LINK_CHARS)
                );
                tracing::warn!("refused to open a link Rail does not accept");
                self.status.error(text, self.ctx.clock.now(), self.tick);
            }
        }
    }

    fn attachments(&mut self) {
        let Some(reader) = self.reader.as_ref() else {
            return;
        };
        let rows: Vec<AttachmentRow> = reader
            .detail()
            .messages
            .iter()
            .flat_map(|m| m.attachments.iter())
            .map(|a| AttachmentRow {
                name: a.filename.clone(),
                mime_type: a.mime_type.clone(),
                size: a.size,
            })
            .collect();
        if rows.is_empty() {
            self.status
                .notify("No attachments in this thread.", self.tick);
            return;
        }
        self.modal = Some(Modal::Attachments(AttachmentList::new(rows)));
    }

    pub(super) fn modal_outcome(&mut self, outcome: ModalOutcome) -> Vec<Effect> {
        match outcome {
            ModalOutcome::Stay => {}
            ModalOutcome::Close => self.modal = None,
            ModalOutcome::Run(Command::ChooseLink { links, number }) => {
                self.modal = None;
                self.confirm_link(&links, number);
            }
            ModalOutcome::Run(Command::OpenLink(url)) => {
                self.modal = None;
                if let Err(error) = (self.ctx.opener)(&url) {
                    tracing::warn!(error = %error, "could not start the browser");
                    let text = format!(
                        "Could not start the browser: {}",
                        escape(&error.to_string())
                    );
                    self.status.error(text, self.ctx.clock.now(), self.tick);
                }
            }
        }
        Vec::new()
    }

    /// The open reader (tests).
    #[cfg(test)]
    pub(crate) fn reader(&self) -> Option<&Reader> {
        self.reader.as_ref()
    }

    /// The text of the reader's cursor line (tests).
    #[cfg(test)]
    pub(crate) fn reader_cursor_text(&self) -> Option<String> {
        self.reader.as_ref()?.cursor_line().map(|l| l.text)
    }

    #[cfg(test)]
    pub(crate) fn error_count(&self) -> usize {
        self.status.errors().count()
    }
}
