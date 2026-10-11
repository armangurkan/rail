//! The mail screen's reader side: opening a thread, fetching its bodies, the reader's keys,
//! links and attachments.

use rail_core::ops::{BodyFetchResponse, ErrorCode, MAX_BODY_FETCH, OpError};
use rail_core::store::view::{BodyState, HtmlBody, ThreadDetail};

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
                let mut effects = self.fetch_wanted();
                effects.extend(self.resume_open());
                effects
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
                let failures = reader.fetched(&response);
                // A fetch that did not produce the body `o` waited for ends the wait.
                if let Some(id) = self.pending_open
                    && self
                        .reader
                        .as_ref()
                        .is_some_and(|r| r.error_of(id).is_some())
                {
                    self.pending_open = None;
                }
                for failure in failures {
                    self.login_hint(&failure);
                }
                effects.push(self.read_thread(thread_id));
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, "a body fetch failed");
                reader.fetch_failed(&error);
                self.pending_open = None;
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
            ReaderOutcome::OpenBrowser => return self.open_html(),
        }
        Vec::new()
    }

    pub(super) fn close_reader(&mut self) {
        self.reader = None;
        self.modal = None;
        self.open_files.remove_all();
        self.pending_open = None;
        // An answer to a read still in flight must not reopen the thread.
        self.latest_thread = self.next_seq();
        self.focus = Focus::List;
    }

    /// `o` on the message at the cursor (or the latest).
    pub(super) fn open_html(&mut self) -> Vec<Effect> {
        let Some(reader) = self.reader.as_ref() else {
            return Vec::new();
        };
        let Some(message) = reader
            .message_at_cursor()
            .or_else(|| reader.detail().messages.last())
        else {
            return Vec::new();
        };
        let (id, body, has_html) = (message.id, message.body.clone(), message.has_html);
        match body {
            BodyState::NotFetched => self.open_after_fetch(id),
            _ if !has_html => {
                self.status.notify(
                    "This message has no HTML part; the reader shows all of its text.",
                    self.tick,
                );
                Vec::new()
            }
            _ => self.confirm_open(id),
        }
    }

    /// `o` on a message with no body yet: wait for a fetch that is running or start one; say
    /// so when none can happen (it failed before), instead of waiting for nothing.
    fn open_after_fetch(&mut self, id: i64) -> Vec<Effect> {
        let Some(reader) = self.reader.as_ref() else {
            return Vec::new();
        };
        if let Some(error) = reader.error_of(id) {
            let text = format!("Cannot open this message: {error}");
            self.status.notify(text, self.tick);
            return Vec::new();
        }
        let fetching = reader.is_fetching(id);
        if !fetching && !reader.wanted().iter().any(|(wanted, _)| *wanted == id) {
            self.status.notify(
                "Cannot open this message: its body is not available.",
                self.tick,
            );
            return Vec::new();
        }
        self.pending_open = Some(id);
        self.status
            .notify("fetching the message for the browser…", self.tick);
        self.fetch_wanted()
    }

    /// Ask once per session, naming remote images; then read the HTML.
    fn confirm_open(&mut self, message_id: i64) -> Vec<Effect> {
        if self.html_confirmed {
            return vec![self.ctx.store.html(message_id)];
        }
        let lines = vec![
            "Rail writes this message's HTML to a private temporary file and opens it in your browser."
                .to_string(),
            "Remote images in this message will load there, which can tell the sender you opened it."
                .to_string(),
            "Rail asks once per session; the file is removed when you close the reader."
                .to_string(),
        ];
        self.modal = Some(Modal::Confirm(Confirm::new(
            "Open in the browser?",
            lines,
            Command::OpenHtml(message_id),
        )));
        Vec::new()
    }

    /// After a re-read: the message `o` waited for has its body now (or failed).
    fn resume_open(&mut self) -> Vec<Effect> {
        let Some(id) = self.pending_open else {
            return Vec::new();
        };
        if let Some(error) = self.reader.as_ref().and_then(|r| r.error_of(id)) {
            let text = format!("Cannot open this message: {error}");
            self.pending_open = None;
            self.status.notify(text, self.tick);
            return Vec::new();
        }
        let found = self
            .reader
            .as_ref()
            .and_then(|r| r.detail().messages.iter().find(|m| m.id == id))
            .map(|m| (m.body.clone(), m.has_html));
        match found {
            Some((BodyState::NotFetched, _)) => Vec::new(),
            Some((_, true)) => {
                self.pending_open = None;
                self.confirm_open(id)
            }
            Some((_, false)) | None => {
                self.pending_open = None;
                self.status
                    .notify("This message has no HTML part to open.", self.tick);
                Vec::new()
            }
        }
    }

    pub(super) fn html_read(
        &mut self,
        message_id: i64,
        result: Result<Option<HtmlBody>, ReadError>,
    ) -> Vec<Effect> {
        // The reader closed while the read was in flight: nobody asked for this any more.
        if self.reader.is_none() {
            return Vec::new();
        }
        match result {
            Ok(Some(HtmlBody::Html(html))) => self.open_file(&html),
            Ok(Some(HtmlBody::NotFetched)) => {
                self.pending_open = Some(message_id);
                return self.fetch_wanted();
            }
            Ok(Some(HtmlBody::Absent) | None) => {
                self.status
                    .notify("This message has no HTML part to open.", self.tick);
            }
            Err(error) => self.read_failed(error),
        }
        Vec::new()
    }

    fn open_file(&mut self, html: &str) {
        let opened = self
            .open_files
            .write(html)
            .map_err(|e| {
                format!(
                    "Could not write the message for the browser: {}",
                    escape(&e.to_string())
                )
            })
            .and_then(|path| {
                url::Url::from_file_path(&path).map_err(|()| {
                    "Could not open the message: its path is not absolute".to_string()
                })
            })
            .and_then(|url| {
                (self.ctx.opener)(&url)
                    .map_err(|e| format!("Could not start the browser: {}", escape(&e.to_string())))
            });
        if let Err(text) = opened {
            tracing::warn!("could not open a message in the browser");
            self.status.error(text, self.ctx.clock.now(), self.tick);
        }
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
            ModalOutcome::Run(Command::OpenHtml(message_id)) => {
                self.modal = None;
                self.html_confirmed = true;
                return vec![self.ctx.store.html(message_id)];
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

    /// The message `o` waits on (tests).
    #[cfg(test)]
    pub(crate) fn pending_open(&self) -> Option<i64> {
        self.pending_open
    }

    /// Replace the browser opener (tests).
    #[cfg(test)]
    pub(crate) fn set_opener(&mut self, opener: crate::tui::effects::Opener) {
        self.ctx.opener = opener;
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
