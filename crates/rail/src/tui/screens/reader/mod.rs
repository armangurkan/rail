//! The thread reader: a title block for the thread (the subject, the newest message's sender,
//! time and account, its recipients and the count), the newest message open under it, and the
//! older messages as cards (number, sender, time, then a preview or the message itself) in the
//! order `thread_order` says; bodies wrapped at the pane's width with folded quotes and
//! numbered links, "fetching…" for bodies the store does not hold yet. One component, drawn in
//! the split layout's pane or as a full-screen view; the mail screen decides which.

mod cards;
#[cfg(test)]
mod keys_tests;
mod lines;
#[cfg(test)]
mod lines_tests;
#[cfg(test)]
mod test_threads;
#[cfg(test)]
mod tests;
mod view;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rail_core::config::ThreadOrder;
use rail_core::ops::{BodyFetchResponse, BodyOutcome, OpError};
use rail_core::store::view::{BodyState, MessageView, ThreadDetail};
use ratatui::style::Color;

pub use lines::{LineKind, ReaderLine, Tone};

use crate::tui::clock::Clock;
use crate::tui::keys::Action;
use crate::tui::screens::mail::{Glyphs, error_line};
use crate::tui::text::markers;
use crate::tui::theme::Theme;

/// Columns assumed before the first frame.
const FALLBACK_WIDTH: u16 = 80;

/// The lines built for one width and one generation of the reader's state.
type CachedLines = (u16, u64, Rc<Vec<ReaderLine>>);

/// What a reader key asks of the mail screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderOutcome {
    Stay,
    Back,
    FollowLink,
    OpenBrowser,
    Attachments,
}

/// The thread's account as the title block shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderAccount {
    /// Its place in `config.toml`'s order: the theme's colour when it has none of its own.
    pub index: usize,
    /// Its own colour (`color` in `[[accounts]]`).
    pub color: Option<Color>,
    /// `AccountEntry::badge`: one escaped cell.
    pub badge: String,
    /// What the screen calls the account: outside text, escaped when drawn.
    pub label: String,
}

impl ReaderAccount {
    /// An account that is no longer configured: no badge or label, the terminal's colour.
    pub fn unknown() -> Self {
        Self {
            index: 0,
            color: Some(Color::Reset),
            badge: String::new(),
            label: String::new(),
        }
    }

    /// The badge's colour: the account's own, else the theme's by place.
    pub fn color(&self, theme: &Theme) -> Color {
        self.color
            .unwrap_or_else(|| theme.account_color(self.index))
    }
}

pub struct Reader {
    detail: ThreadDetail,
    clock: Clock,
    glyphs: Glyphs,
    /// The thread's account, for the title block's badge.
    account: ReaderAccount,
    /// How the older messages run (`thread_order`, `:thread-order`).
    order: ThreadOrder,
    /// `h`: open messages show their header fields, for this session.
    headers: bool,
    expanded: Vec<bool>,
    unfolded: HashSet<(usize, usize)>,
    /// Message ids with a `body.fetch` in flight.
    fetching: HashSet<i64>,
    /// Message ids asked for since the thread was opened: never asked twice, so a daemon that
    /// answers without storing cannot start a loop. Opening the thread again asks again.
    asked: HashSet<i64>,
    /// Escaped error text per message id.
    errors: HashMap<i64, String>,
    /// The last re-read failed: the subject line shows this (escaped) until a read succeeds.
    read_error: Option<String>,
    cursor: usize,
    scroll: usize,
    /// Bumped by every change of what the lines show.
    generation: Cell<u64>,
    cache: RefCell<Option<CachedLines>>,
    /// The text area's size from the last frame.
    size: Cell<(u16, u16)>,
}

/// The newest message starts open, the older ones collapsed.
fn default_expanded(detail: &ThreadDetail) -> Vec<bool> {
    let last = detail.messages.len().saturating_sub(1);
    (0..detail.messages.len()).map(|i| i == last).collect()
}

impl Reader {
    pub fn new(
        detail: ThreadDetail,
        clock: Clock,
        glyphs: Glyphs,
        account: ReaderAccount,
        order: ThreadOrder,
    ) -> Self {
        Self {
            expanded: default_expanded(&detail),
            detail,
            clock,
            glyphs,
            account,
            order,
            headers: false,
            unfolded: HashSet::new(),
            fetching: HashSet::new(),
            asked: HashSet::new(),
            errors: HashMap::new(),
            read_error: None,
            cursor: 0,
            scroll: 0,
            generation: Cell::new(0),
            cache: RefCell::new(None),
            size: Cell::new((0, 0)),
        }
    }

    pub fn thread_id(&self) -> i64 {
        self.detail.thread_id
    }

    pub fn account_id(&self) -> i64 {
        self.detail.account_id
    }

    pub fn detail(&self) -> &ThreadDetail {
        &self.detail
    }

    /// The thread's account (tests).
    #[cfg(test)]
    pub fn account(&self) -> &ReaderAccount {
        &self.account
    }

    /// How the older messages run (tests).
    #[cfg(test)]
    pub fn order(&self) -> ThreadOrder {
        self.order
    }

    fn changed(&self) {
        self.generation.set(self.generation.get() + 1);
    }

    /// Dates are written against `clock` from now on (`:time`).
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = clock;
        self.changed();
    }

    /// The older messages run in `order` from now on (`:thread-order`); the cursor goes to the
    /// start of the message it was in.
    pub fn set_order(&mut self, order: ThreadOrder) {
        if order == self.order {
            return;
        }
        let message = self.cursor_line().and_then(|l| l.kind.message());
        self.order = order;
        self.changed();
        self.land(message);
        let page = usize::from(self.size.get().1.max(1));
        self.scroll = crate::tui::screens::mail::visible_offset(self.scroll, self.cursor, page);
    }

    /// The thread read again: each message keeps its expansion (new ones get the default),
    /// errors stay only on bodies still missing, folds stay open.
    /// An unchanged thread leaves everything as it is, unless a body it waited for did not
    /// arrive (that message gets its note).
    pub fn refresh(&mut self, detail: ThreadDetail) {
        // A failed re-read's error is the subject line's until a read works.
        if self.read_error.take().is_some() {
            self.changed();
        }
        if detail == self.detail && !self.missed_a_body() {
            return;
        }
        let defaults = default_expanded(&detail);
        let before: HashMap<i64, bool> = self
            .detail
            .messages
            .iter()
            .zip(&self.expanded)
            .map(|(m, open)| (m.id, *open))
            .collect();
        self.expanded = detail
            .messages
            .iter()
            .zip(defaults)
            .map(|(m, default)| before.get(&m.id).copied().unwrap_or(default))
            .collect();
        self.errors.retain(|id, _| {
            detail
                .messages
                .iter()
                .any(|m| m.id == *id && m.body == BodyState::NotFetched)
        });
        for message in detail
            .messages
            .iter()
            .filter(|m| m.body == BodyState::NotFetched)
        {
            let waited = self.asked.contains(&message.id) && !self.fetching.contains(&message.id);
            if waited && !self.errors.contains_key(&message.id) {
                self.errors.insert(
                    message.id,
                    "The body did not arrive; open the thread again to try again.".into(),
                );
            }
        }
        self.detail = detail;
        self.changed();
        // The cursor stays on its line, or on the last one when the thread got shorter.
        let last = self.current().len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
    }

    /// A re-read of this thread failed with `text` (escaped).
    pub fn read_failed(&mut self, text: String) {
        self.read_error = Some(text);
        self.changed();
    }

    /// A body asked for and answered is still missing, with no note yet.
    fn missed_a_body(&self) -> bool {
        self.detail.messages.iter().any(|m| {
            m.body == BodyState::NotFetched
                && self.asked.contains(&m.id)
                && !self.fetching.contains(&m.id)
                && !self.errors.contains_key(&m.id)
        })
    }

    /// Bodies to ask the daemon for: not fetched, not asked for yet, not failed.
    pub fn wanted(&self) -> Vec<(i64, String)> {
        self.detail
            .messages
            .iter()
            .filter(|m| m.body == BodyState::NotFetched)
            .filter(|m| !self.asked.contains(&m.id) && !self.errors.contains_key(&m.id))
            .map(|m| (m.id, m.remote_id.clone()))
            .collect()
    }

    /// The recorded fetch error for a message, if any.
    pub fn error_of(&self, id: i64) -> Option<&str> {
        self.errors.get(&id).map(String::as_str)
    }

    /// True while a `body.fetch` for the message is in flight.
    pub fn is_fetching(&self, id: i64) -> bool {
        self.fetching.contains(&id)
    }

    pub fn mark_fetching(&mut self, ids: &[i64]) {
        self.fetching.extend(ids.iter().copied());
        self.asked.extend(ids.iter().copied());
        self.changed();
    }

    /// A `body.fetch` answer: every id leaves `fetching`; missing and failed ones get their
    /// message. The failures' errors, for the screen (a login hint).
    pub fn fetched(&mut self, response: &BodyFetchResponse) -> Vec<OpError> {
        let mut failures = Vec::new();
        for result in &response.results {
            let Some(id) = self.id_of(&result.message_id) else {
                continue;
            };
            self.fetching.remove(&id);
            match &result.outcome {
                BodyOutcome::Fetched | BodyOutcome::Unreadable => {}
                BodyOutcome::Missing => {
                    self.errors
                        .insert(id, "The mail provider no longer has this message.".into());
                }
                BodyOutcome::Failed(error) => {
                    self.errors.insert(
                        id,
                        format!("Could not fetch this message: {}", error_line(error)),
                    );
                    failures.push(error.clone());
                }
            }
        }
        self.changed();
        failures
    }

    /// The whole request failed (the daemon went away): every message still fetching says so.
    pub fn fetch_failed(&mut self, error: &OpError) {
        let text = format!("Could not fetch this message: {}", error_line(error));
        for id in self.fetching.drain() {
            self.errors.insert(id, text.clone());
        }
        self.changed();
    }

    fn id_of(&self, remote: &str) -> Option<i64> {
        self.detail
            .messages
            .iter()
            .find(|m| m.remote_id == remote)
            .map(|m| m.id)
    }

    /// The lines at `width`, built once per width and state.
    pub fn lines(&self, width: u16) -> Rc<Vec<ReaderLine>> {
        let generation = self.generation.get();
        if let Some((w, g, lines)) = self.cache.borrow().as_ref()
            && *w == width
            && *g == generation
        {
            return lines.clone();
        }
        let state = lines::State {
            expanded: &self.expanded,
            unfolded: &self.unfolded,
            fetching: &self.fetching,
            errors: &self.errors,
            clock: &self.clock,
            glyphs: &self.glyphs,
            account: &self.account,
            headers: self.headers,
            order: self.order,
            read_error: self.read_error.as_deref(),
        };
        let built = Rc::new(cards::build(
            &self.detail,
            &state,
            usize::from(width.max(1)),
        ));
        *self.cache.borrow_mut() = Some((width, generation, built.clone()));
        built
    }

    fn current(&self) -> Rc<Vec<ReaderLine>> {
        let width = match self.size.get().0 {
            0 => FALLBACK_WIDTH,
            w => w,
        };
        self.lines(width)
    }

    pub fn cursor_line(&self) -> Option<ReaderLine> {
        self.current().get(self.cursor).cloned()
    }

    pub fn message_at_cursor(&self) -> Option<&MessageView> {
        let message = self.cursor_line()?.kind.message()?;
        self.detail.messages.get(message)
    }

    /// The links of the cursor's message and the marker numbers on the cursor line.
    pub fn links_at_cursor(&self) -> Option<(Vec<String>, Vec<usize>)> {
        let line = self.cursor_line()?;
        let message = self.detail.messages.get(line.kind.message()?)?;
        if message.links.is_empty() {
            return None;
        }
        let numbers = match line.kind {
            LineKind::Body { .. } | LineKind::Quote { .. } => {
                markers(&line.text, message.links.len())
                    .into_iter()
                    .map(|m| m.number)
                    .collect()
            }
            _ => Vec::new(),
        };
        Some((message.links.clone(), numbers))
    }

    pub fn act(&mut self, action: Action) -> ReaderOutcome {
        let lines = self.current();
        let last = lines.len().saturating_sub(1);
        let page = usize::from(self.size.get().1.max(1));
        match action {
            Action::ReaderDown => self.cursor = (self.cursor + 1).min(last),
            Action::ReaderUp => self.cursor = self.cursor.saturating_sub(1),
            Action::ReaderPageDown => self.cursor = (self.cursor + page).min(last),
            Action::ReaderPageUp => self.cursor = self.cursor.saturating_sub(page),
            Action::ReaderTop => self.cursor = 0,
            Action::ReaderBottom => self.cursor = last,
            Action::ReaderNext => self.jump(&lines, true),
            Action::ReaderPrev => self.jump(&lines, false),
            Action::ReaderExpand => self.toggle_message(&lines),
            Action::ReaderUnfold => self.toggle_fold(&lines),
            Action::ReaderHeaders => self.toggle_headers(&lines),
            Action::ReaderBack => return ReaderOutcome::Back,
            Action::ReaderFollowLink => return ReaderOutcome::FollowLink,
            Action::ReaderOpenBrowser => return ReaderOutcome::OpenBrowser,
            Action::ReaderAttachments => return ReaderOutcome::Attachments,
            _ => {}
        }
        self.scroll = crate::tui::screens::mail::visible_offset(self.scroll, self.cursor, page);
        ReaderOutcome::Stay
    }

    fn jump(&mut self, lines: &[ReaderLine], forward: bool) {
        let starts = starts(lines);
        let found = if forward {
            starts.into_iter().find(|&i| i > self.cursor)
        } else {
            starts.into_iter().rev().find(|&i| i < self.cursor)
        };
        if let Some(index) = found {
            self.cursor = index;
        }
    }

    fn toggle_message(&mut self, lines: &[ReaderLine]) {
        let Some(message) = lines.get(self.cursor).and_then(|l| l.kind.message()) else {
            return;
        };
        if let Some(open) = self.expanded.get_mut(message) {
            *open = !*open;
        }
        self.changed();
        self.land(Some(message));
    }

    /// `h`: every open message's header fields shown or hidden, for the session; the cursor
    /// goes to the start of the message it was in.
    fn toggle_headers(&mut self, lines: &[ReaderLine]) {
        let message = lines.get(self.cursor).and_then(|l| l.kind.message());
        self.headers = !self.headers;
        self.changed();
        self.land(message);
    }

    /// The cursor on `message`'s first line as the lines are now; where it was (clamped) when
    /// it is on no message's line.
    fn land(&mut self, message: Option<usize>) {
        let fresh = self.current();
        let last = fresh.len().saturating_sub(1);
        self.cursor = message
            .and_then(|m| start_of(&fresh, m))
            .unwrap_or(self.cursor.min(last));
    }

    fn toggle_fold(&mut self, lines: &[ReaderLine]) {
        let target = match lines.get(self.cursor).map(|l| l.kind) {
            Some(LineKind::Fold { message, block } | LineKind::Quote { message, block }) => {
                (message, block)
            }
            Some(LineKind::Card { .. } | LineKind::Title { .. } | LineKind::Preview { .. }) => {
                return self.toggle_message(lines);
            }
            _ => return,
        };
        if !self.unfolded.remove(&target) {
            self.unfolded.insert(target);
        }
        self.changed();
        let fresh = self.current();
        self.cursor = fresh
            .iter()
            .position(|l| {
                matches!(l.kind, LineKind::Fold { message, block } | LineKind::Quote { message, block } if (message, block) == target)
            })
            .unwrap_or(self.cursor.min(fresh.len().saturating_sub(1)));
    }
}

/// Where each message starts, in drawn order: the subject line for the newest, the card line
/// for each older one.
fn starts(lines: &[ReaderLine]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|&(i, line)| match line.kind {
            LineKind::Card { .. } => true,
            LineKind::Title { .. } => {
                i == 0 || !matches!(lines[i - 1].kind, LineKind::Title { .. })
            }
            _ => false,
        })
        .map(|(i, _)| i)
        .collect()
}

/// The line `message` starts on.
fn start_of(lines: &[ReaderLine], message: usize) -> Option<usize> {
    starts(lines)
        .into_iter()
        .find(|&i| lines[i].kind.message() == Some(message))
}
