//! Answers from the store and the daemon: the debounced re-read after a change, the list and
//! sidebar answers, the daemon's status events, and a job that panicked.

use crate::tui::app::Effect;
use crate::tui::data::{
    DaemonEvent, ListOutcome, MailEvent, REQUERY_DEBOUNCE, ReadError, Timer, timer,
};
use crate::tui::screens::reader::Reader;

use super::{MailScreen, Place, StoreState, error_line};

impl MailScreen {
    /// The store changed: re-read the visible window and the sidebar once things settle, and
    /// after the refresh in flight, if any, has answered.
    pub(crate) fn dirty(&mut self) -> Vec<Effect> {
        if self.refreshing.is_some() {
            self.dirty_again = true;
            return Vec::new();
        }
        if self.requery_pending {
            return Vec::new();
        }
        self.requery_pending = true;
        self.requery_token += 1;
        vec![timer(REQUERY_DEBOUNCE, self.requery_token, Timer::Requery)]
    }

    pub(super) fn requery(&mut self) -> Vec<Effect> {
        self.requery_pending = false;
        if self.refreshing.is_some() {
            self.dirty_again = true;
            return Vec::new();
        }
        self.list.cancel_loading();
        if self.in_drafts() {
            return vec![self.read_sidebar(), self.read_drafts_pane()];
        }
        let (at, place) = self
            .navigating
            .unwrap_or_else(|| (self.list.requery_at(self.drawn_top.get()), Place::Keep));
        let list = self.list_request(at, place);
        let sidebar_seq = self.next_seq();
        self.latest_sidebar = sidebar_seq;
        let thread = self.reader.as_ref().map(Reader::thread_id).map(|id| {
            let seq = self.next_seq();
            self.latest_thread = seq;
            (seq, id)
        });
        let end = self.next_seq();
        self.refreshing = Some(end);
        vec![self.ctx.store.refresh(list, sidebar_seq, thread, end)]
    }

    /// The refresh in flight gave its last answer: run the next one if the store changed.
    pub(super) fn refreshed(&mut self) -> Vec<Effect> {
        self.refreshing = None;
        if std::mem::take(&mut self.dirty_again) {
            return self.dirty();
        }
        Vec::new()
    }

    pub(super) fn mail_event(&mut self, event: MailEvent) -> Vec<Effect> {
        match event {
            MailEvent::RefreshEnded { seq } if Some(seq) == self.refreshing => self.refreshed(),
            MailEvent::Listed { seq, result } => self.listed(seq, result),
            MailEvent::Sidebar { seq, result } => self.sidebar_counted(seq, result),
            MailEvent::Thread {
                seq,
                thread_id,
                result,
            } if seq == self.latest_thread => self.thread_read(thread_id, result),
            MailEvent::Bodies { thread_id, result } => {
                let mut effects = self.bodies_answered(thread_id, result);
                effects.extend(self.body_for_reply(thread_id));
                effects
            }
            MailEvent::Html { message_id, result } => self.html_read(message_id, result),
            MailEvent::Daemon(event) => self.on_daemon(event),
            MailEvent::Timer {
                token,
                kind: Timer::Requery,
            } if token == self.requery_token && self.requery_pending => self.requery(),
            MailEvent::Timer {
                token,
                kind: Timer::Filter,
            } if token == self.filter_token => self.apply_filter(),
            other => self.action_event(other),
        }
    }

    pub(super) fn listed(
        &mut self,
        seq: u64,
        result: Result<ListOutcome, ReadError>,
    ) -> Vec<Effect> {
        if seq != self.latest_list {
            return Vec::new();
        }
        self.navigating = None;
        match result {
            Ok(outcome) => {
                self.account_ids = outcome.account_ids;
                self.store_state = StoreState::Ready;
                let height = self.height();
                self.list
                    .apply(outcome.page, outcome.at, self.place, height);
                self.actions.selection.refresh(self.list.rows());
                self.place = Place::Keep;
            }
            Err(error) => {
                self.list.cancel_loading();
                self.read_failed(error);
            }
        }
        Vec::new()
    }

    pub(super) fn read_failed(&mut self, error: ReadError) {
        match error {
            ReadError::NotSetUp => self.store_state = StoreState::NotSetUp,
            // Only an older read is skipped, and an older answer is dropped before this.
            ReadError::Stale => {}
            ReadError::Failed(error) => {
                tracing::warn!(code = ?error.code, "a mail screen read failed");
                let text = format!("The store could not be read: {}", error_line(&error));
                self.status.error(text, self.ctx.clock.now(), self.tick);
                self.store_state = StoreState::Failed(error);
            }
        }
    }

    pub(super) fn on_daemon(&mut self, event: DaemonEvent) -> Vec<Effect> {
        match event {
            DaemonEvent::Connected(statuses) => {
                self.status.connected(statuses);
                self.dirty()
            }
            DaemonEvent::Progress(status) => {
                self.status.progress(*status);
                self.dirty()
            }
            DaemonEvent::Lost(error) => {
                tracing::warn!(code = ?error.code, "the daemon link was lost");
                self.status.lost(&error, self.ctx.clock.now(), self.tick);
                self.queuing_lost(&error);
                // A fetch that outlives the connection must not stay at "fetching…".
                if let Some(reader) = self.reader.as_mut() {
                    reader.fetch_failed(&error);
                }
                self.pending_open = None;
                Vec::new()
            }
            DaemonEvent::Refreshed(Ok(())) => Vec::new(),
            DaemonEvent::Refreshed(Err(error)) => {
                // The watch may be live: only `Lost` says the link is gone.
                tracing::warn!(code = ?error.code, "sync.now failed");
                let text = format!("Could not sync now: {}", error_line(&error));
                self.status.error(text, self.ctx.clock.now(), self.tick);
                Vec::new()
            }
        }
    }

    /// A job panicked: the status bar shows the message escaped; the log gets only its length,
    /// since a panic message can quote mail text. A refresh is ended by its own
    /// `RefreshEnded`, which its job sends even when it panics.
    pub(super) fn job_panicked(&mut self, message: &str) -> Vec<Effect> {
        tracing::error!(message_bytes = message.len(), "a mail screen job panicked");
        let text = format!(
            "internal error: {}; see rail.log",
            crate::tui::text::escape(message)
        );
        self.status.error(text, self.ctx.clock.now(), self.tick);
        Vec::new()
    }
}
