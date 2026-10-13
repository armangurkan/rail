//! The mail screen: the thread list over the local store, the sidebar of accounts and labels,
//! and the status bar (the reader and the modals join in later tasks). Keys go through the
//! keymap; every read and daemon call is a job whose answer comes back as a `MailEvent`.

mod actions;
mod actions_pick;
mod composing;
mod composing_answers;
mod composing_flow;
mod drafts_flow;
mod drafts_pane;
mod layout;
mod list;
mod m365;
#[cfg(test)]
mod modal_tests;
mod palette;
mod reading;
mod selection;
mod sidebar;
mod status;
#[cfg(test)]
mod tests;
mod themes;
mod view;
#[cfg(test)]
mod view_tests;

use std::cell::Cell;
use std::sync::Arc;

use crossterm::event::KeyEvent;
use rail_core::config::{AccountKind, Config, Icons};
use rail_core::ops::OpError;
use rail_core::paths::Paths;
use rail_core::store::filter::{Filter, parse};
use rail_core::store::view::PageAt;
use ratatui::Frame;
use ratatui::style::Color;

#[cfg(test)]
pub(crate) use actions::UNDO_TICKS;
#[cfg(test)]
pub use list::MAX_WINDOW;
pub use list::{ListWindow, Place};
pub use sidebar::Sidebar;
pub use status::{DaemonState, StatusBar, error_line};
pub use view::Glyphs;
pub(crate) use view::visible_offset;

use crate::tui::app::{Effect, Msg, TaskResult};
use crate::tui::clock::Clock;
use crate::tui::data::{
    DaemonEvent, DaemonLink, ListOutcome, ListRequest, MailEvent, REQUERY_DEBOUNCE, ReadError,
    ScopeChoice, StoreReader, Timer, timer,
};
use crate::tui::effects::Opener;
use crate::tui::keys::{Action, Chord, Keymap, Resolved, View};
use crate::tui::modals::{FilterLine, Help, Modal, Palette};
use crate::tui::open::{OpenFiles, older_than, sweep_dir};
use crate::tui::screen::Screen;
use crate::tui::screens::compose::Composer;
use crate::tui::screens::reader::Reader;
use crate::tui::theme::Theme;
use crate::tui::theme::choice::{ColorDepth, ThemeChoice};

/// Rows the list is assumed to show before the first frame says.
const FALLBACK_HEIGHT: usize = 20;
/// Compose files older than this at start are a crash's leftovers.
const COMPOSE_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);

/// What the mail screen needs from the outside world.
#[derive(Clone)]
pub struct MailContext {
    pub paths: Paths,
    pub keymap: Arc<Keymap>,
    pub store: StoreReader,
    pub link: DaemonLink,
    /// Starts the browser (`o`, `gx`).
    pub opener: Opener,
    pub clock: Clock,
    pub icons: Icons,
    pub split_threshold: u16,
    /// The terminal shows OSC 8 hyperlinks.
    pub hyperlinks: bool,
    /// What the terminal can show; the theme picker needs it to preview.
    pub depth: ColorDepth,
    /// The scheme and accent in force, which the pickers change one at a time.
    pub theme_choice: ThemeChoice,
    /// The editor's command line, the compose file's path appended when it runs.
    pub editor: Vec<String>,
    /// Each account's signature (in `config.toml`'s order): the file's text, why it could not
    /// be read, or `None` when none is set.
    pub signatures: Vec<Option<Result<String, String>>>,
}

/// One configured account, in `config.toml`'s order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEntry {
    pub name: String,
    pub address: String,
    pub kind: AccountKind,
    /// `None` takes the theme's colour by position.
    pub color: Option<Color>,
}

impl AccountEntry {
    /// The configured accounts; `color` read as a ratatui colour name or `#rrggbb`, else `None`
    /// (the theme's colour by position).
    pub fn from_config(config: &Config) -> Vec<AccountEntry> {
        config
            .accounts
            .iter()
            .map(|account| AccountEntry {
                name: account.name.clone(),
                address: account.address.to_ascii_lowercase(),
                kind: account.kind,
                color: account
                    .color
                    .as_deref()
                    .and_then(|c| c.parse::<Color>().ok()),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Sidebar,
    Reader,
    /// The composer's review or prompts, over the list or the reader.
    Compose,
}

/// What the list area shows instead of rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreState {
    Loading,
    Ready,
    NotSetUp,
    Failed(OpError),
}

pub struct MailScreen {
    ctx: MailContext,
    accounts: Vec<AccountEntry>,
    account_ids: Vec<Option<i64>>,
    scope: ScopeChoice,
    filter: Filter,
    /// The filter line's text, kept while the line is closed.
    filter_text: String,
    /// The newest filter keystroke: only its timer reads.
    filter_token: u64,
    list: ListWindow,
    /// Where the selection goes when the newest list read answers.
    place: Place,
    sidebar: Sidebar,
    status: StatusBar,
    pub(crate) focus: Focus,
    /// Set by `b` and `Tab`; by the width until then.
    sidebar_shown: Option<bool>,
    store_state: StoreState,
    /// The list area's rows and the frame's width, from the last frame drawn.
    size: Cell<(u16, u16)>,
    /// The newest list read that replaces the window (a scope change, `Home`, `End`) while it
    /// is in flight: a re-query reuses it instead of overriding it.
    navigating: Option<(PageAt, Place)>,
    /// The open thread, drawn in the pane or full screen.
    reader: Option<Reader>,
    modal: Option<Modal>,
    pub(crate) actions: actions::ActionState,
    /// The theme kept and, while `:theme` or `:accent` is open, the one to restore.
    pub(crate) themes: themes::ThemeState,
    /// The HTML files `o` wrote this session, removed when the reader closes and on drop.
    open_files: OpenFiles,
    /// The browser confirmation was answered yes this session.
    html_confirmed: bool,
    /// The message `o` is waiting on a body fetch for.
    pending_open: Option<i64>,
    /// The message being written, drawn over everything but the status bar.
    compose: Option<Composer>,
    composing: composing::ComposeState,
    /// The Drafts folder's pane and what its keys wait on.
    drafts: drafts_flow::DraftsState,
    seq: u64,
    latest_list: u64,
    /// The newest thread read: an older answer is dropped.
    latest_thread: u64,
    latest_sidebar: u64,
    requery_token: u64,
    /// A re-query timer is armed.
    requery_pending: bool,
    /// The running refresh's sequence number: one refresh at a time, ended by its `RefreshEnded`.
    refreshing: Option<u64>,
    /// The store changed while a refresh ran: run once more when it ends.
    dirty_again: bool,
    chord: Chord,
    tick: usize,
    panic_on_key: bool,
}

impl MailScreen {
    /// The dot colour of account `index`: its own, or the theme's by position.
    pub(super) fn account_color(&self, index: usize, theme: &Theme) -> Color {
        self.accounts
            .get(index)
            .and_then(|a| a.color)
            .unwrap_or_else(|| theme.account_color(index))
    }

    pub fn new(ctx: MailContext, accounts: Vec<AccountEntry>) -> Self {
        let open_files = OpenFiles::new(&ctx.paths.cache_dir);
        let themes = themes::ThemeState::new(ctx.theme_choice);
        Self {
            ctx,
            account_ids: vec![None; accounts.len()],
            accounts,
            scope: ScopeChoice::Unified,
            filter: parse(""),
            filter_text: String::new(),
            filter_token: 0,
            list: ListWindow::default(),
            place: Place::Top,
            sidebar: Sidebar::default(),
            status: StatusBar::default(),
            focus: Focus::List,
            sidebar_shown: None,
            store_state: StoreState::Loading,
            size: Cell::new((0, 0)),
            navigating: None,
            reader: None,
            modal: None,
            actions: actions::ActionState::default(),
            themes,
            open_files,
            html_confirmed: false,
            pending_open: None,
            compose: None,
            composing: composing::ComposeState::default(),
            drafts: drafts_flow::DraftsState::default(),
            seq: 0,
            latest_list: 0,
            latest_thread: 0,
            latest_sidebar: 0,
            requery_token: 0,
            requery_pending: false,
            refreshing: None,
            dirty_again: false,
            chord: Chord::default(),
            tick: 0,
            panic_on_key: false,
        }
    }

    /// Debug builds' test hook (`RAIL_TEST_MAIL_PANIC`): panic on the first key, so a binary
    /// test can check that the terminal is restored.
    pub fn with_panic_on_key(mut self, on: bool) -> Self {
        self.panic_on_key = on;
        self
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn height(&self) -> usize {
        match self.size.get().0 {
            0 => FALLBACK_HEIGHT,
            rows => usize::from(rows),
        }
    }

    /// Shown when set by `b`/`Tab`, else from the last frame's width.
    fn sidebar_visible(&self) -> bool {
        self.sidebar_shown
            .unwrap_or_else(|| layout::sidebar_by_default(self.size.get().1))
    }

    fn list_request(&mut self, at: PageAt, place: Place) -> ListRequest {
        let seq = self.next_seq();
        self.latest_list = seq;
        self.place = place;
        ListRequest {
            seq,
            scope: self.scope.clone(),
            addresses: self.accounts.iter().map(|a| a.address.clone()).collect(),
            names: self.accounts.iter().map(|a| a.name.clone()).collect(),
            filter: self.filter.clone(),
            at,
            utc_offset: self.ctx.clock.utc_offset(),
        }
    }

    fn read_list(&mut self, at: PageAt, place: Place) -> Effect {
        self.navigating = matches!(at, PageAt::First | PageAt::Last).then_some((at, place));
        let request = self.list_request(at, place);
        self.ctx.store.list(request)
    }

    fn read_sidebar(&mut self) -> Effect {
        let seq = self.next_seq();
        self.latest_sidebar = seq;
        let addresses = self.accounts.iter().map(|a| a.address.clone()).collect();
        self.ctx.store.sidebar(seq, addresses)
    }

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

    fn requery(&mut self) -> Vec<Effect> {
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
            .unwrap_or_else(|| (self.list.requery_at(), Place::Keep));
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
    fn refreshed(&mut self) -> Vec<Effect> {
        self.refreshing = None;
        if std::mem::take(&mut self.dirty_again) {
            return self.dirty();
        }
        Vec::new()
    }

    /// Show `scope`. The old rows stay drawn until the new ones arrive (spec §3); a page
    /// read of the old scope still in flight is dropped by its sequence number.
    fn set_scope(&mut self, scope: ScopeChoice) -> Vec<Effect> {
        self.sidebar.point_at(&scope);
        self.scope = scope;
        self.list.cancel_loading();
        if self.in_drafts() {
            return self.open_drafts_pane();
        }
        vec![self.read_list(PageAt::First, Place::Top)]
    }

    /// The views keys are read in, most specific first.
    pub(crate) fn views(&self) -> [View; 2] {
        let view = match self.focus {
            Focus::List => View::List,
            Focus::Sidebar => View::Sidebar,
            Focus::Reader => View::Reader,
            Focus::Compose => View::Compose,
        };
        [view, View::Global]
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.panic_on_key {
            panic!("rail test panic in the mail screen");
        }
        if let Some(modal) = self.modal.as_mut() {
            let outcome = modal.key(key);
            return self.modal_outcome(outcome);
        }
        if self.focus == Focus::Compose {
            return self.compose_key(key);
        }
        let views = self.views();
        match self.ctx.keymap.resolve(&views, &mut self.chord, key) {
            Resolved::Action(action) => self.act(action),
            Resolved::Pending | Resolved::Unbound => Vec::new(),
        }
    }

    pub(crate) fn act(&mut self, action: Action) -> Vec<Effect> {
        if let Some(effects) = self.drafts_action(action) {
            return effects;
        }
        match action {
            // The shell quits on its own key; this is `q` read after a chord's first key.
            Action::GlobalQuit => return vec![Effect::Quit],
            Action::GlobalPalette => {
                self.modal = Some(Modal::Palette(Palette::new(self.palette_entries())));
                return Vec::new();
            }
            Action::GlobalHelp => {
                let help = Help::new(&self.ctx.keymap, &self.views(), self.ctx.icons);
                self.modal = Some(Modal::Help(help));
                return Vec::new();
            }
            Action::ListFilter if self.focus == Focus::List => {
                self.modal = Some(Modal::Filter(FilterLine::new(&self.filter_text)));
                return Vec::new();
            }
            _ => {}
        }
        if let Some(effects) = self.compose_action(action) {
            return effects;
        }
        if let Some(effects) = self.mail_action(action) {
            return effects;
        }
        if let Some(effects) = self.shared_action(action) {
            return effects;
        }
        match self.focus {
            Focus::List => self.list_action(action),
            Focus::Sidebar => self.sidebar_action(action),
            Focus::Reader => self.reader_action(action),
            Focus::Compose => self.compose_act(action),
        }
    }

    /// Actions of both the list and the sidebar.
    fn shared_action(&mut self, action: Action) -> Option<Vec<Effect>> {
        if matches!(self.focus, Focus::Reader | Focus::Compose) {
            // Only the refresh key works in the reader: the status bar offers it after the
            // daemon link is lost, whatever is on screen.
            return (action == Action::ListRefresh).then(|| self.refresh());
        }
        let effects = match action {
            Action::ListSidebar => {
                let shown = !self.sidebar_visible();
                self.sidebar_shown = Some(shown);
                if !shown {
                    self.focus = Focus::List;
                }
                Vec::new()
            }
            Action::SidebarFocus => {
                self.sidebar_shown = Some(true);
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::List,
                    _ => Focus::Sidebar,
                };
                Vec::new()
            }
            Action::ListUnified => self.set_scope(ScopeChoice::Unified),
            Action::ListRefresh => self.refresh(),
            other => return palette::account_number(other).map(|n| self.go_to_account(n)),
        };
        Some(effects)
    }

    fn go_to_account(&mut self, number: usize) -> Vec<Effect> {
        if number > self.accounts.len() {
            self.status
                .notify(format!("There is no account {number}"), self.tick);
            return Vec::new();
        }
        self.set_scope(ScopeChoice::Account(number - 1))
    }

    fn list_action(&mut self, action: Action) -> Vec<Effect> {
        let height = self.height();
        let page = isize::try_from(height).unwrap_or(isize::MAX);
        let wanted = match action {
            Action::ListOpen => return self.open_selected(),
            Action::ListDown => self.list.move_by(1, height),
            Action::ListUp => self.list.move_by(-1, height),
            Action::ListPageDown => self.list.move_by(page, height),
            Action::ListPageUp => self.list.move_by(-page, height),
            Action::ListTop => self.list.top(height),
            Action::ListBottom => self.list.bottom(height),
            _ => None,
        };
        match wanted {
            Some(at) => {
                let place = match at {
                    PageAt::First => Place::Top,
                    PageAt::Last => Place::Bottom,
                    _ => Place::Keep,
                };
                vec![self.read_list(at, place)]
            }
            None => Vec::new(),
        }
    }

    fn sidebar_action(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::SidebarDown => self.sidebar.down(),
            Action::SidebarUp => self.sidebar.up(),
            Action::SidebarOpen => {
                if let Some(item) = self.sidebar.selected() {
                    let scope = item.scope.clone();
                    self.focus = Focus::List;
                    return self.set_scope(scope);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// `Ctrl-r`: sync now and re-read when connected; subscribe again only after the daemon
    /// link reported itself lost (a live watch would forward every update twice); while the
    /// first subscription is still being made there is nothing to ask the daemon.
    fn refresh(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        match self.status.daemon {
            DaemonState::Connected => effects.push(self.ctx.link.refresh()),
            DaemonState::Unreachable | DaemonState::Reachable => {
                self.status.connecting();
                effects.push(self.ctx.link.watch());
            }
            DaemonState::Connecting => {}
        }
        effects.extend(self.requery());
        effects
    }

    fn mail_event(&mut self, event: MailEvent) -> Vec<Effect> {
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

    fn listed(&mut self, seq: u64, result: Result<ListOutcome, ReadError>) -> Vec<Effect> {
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

    fn read_failed(&mut self, error: ReadError) {
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

    fn on_daemon(&mut self, event: DaemonEvent) -> Vec<Effect> {
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

    /// The status bar's text, for tests and the view; led by the filter's operators while a
    /// filter is set.
    pub(crate) fn status_line(&self) -> String {
        let mut line = self.status.line(
            &self.accounts,
            &Glyphs::of(self.ctx.icons),
            &self.ctx.keymap,
        );
        if !self.actions.selection.is_empty() {
            line = format!("{} selected  ·  {line}", self.actions.selection.len());
        }
        // The Drafts folder's pane is not filtered.
        if self.filter.is_empty() || self.in_drafts() {
            return line;
        }
        let parsed = crate::tui::text::escape(&self.filter.describe().join(" · "));
        format!("filter: {parsed}  ·  {line}")
    }

    fn apply_filter(&mut self) -> Vec<Effect> {
        self.filter = parse(&self.filter_text);
        self.list.cancel_loading();
        vec![self.read_list(PageAt::First, Place::Top)]
    }
}

impl MailScreen {
    /// A job panicked: the status bar shows the message escaped; the log gets only its length,
    /// since a panic message can quote mail text. A refresh is ended by its own
    /// `RefreshEnded`, which its job sends even when it panics.
    fn job_panicked(&mut self, message: &str) -> Vec<Effect> {
        tracing::error!(message_bytes = message.len(), "a mail screen job panicked");
        let text = format!(
            "internal error: {}; see rail.log",
            crate::tui::text::escape(message)
        );
        self.status.error(text, self.ctx.clock.now(), self.tick);
        Vec::new()
    }
}

impl Screen for MailScreen {
    fn start(&mut self) -> Vec<Effect> {
        match self.open_files.sweep(std::time::SystemTime::now()) {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "removed message files a previous session left"),
            Err(error) => {
                tracing::warn!(error = %error, "could not clean the opened-messages directory");
            }
        }
        let cutoff = older_than(std::time::SystemTime::now(), COMPOSE_STALE_AFTER);
        match sweep_dir(&self.ctx.paths.compose_dir(), cutoff, Some("eml")) {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "removed compose files a previous session left"),
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "could not clean the compose directory");
            }
        }
        if self.ctx.depth == ColorDepth::Ansi && self.themes.current.scheme.is_some() {
            self.status.notify(themes::NEEDS_TRUECOLOR, self.tick);
        }
        vec![
            self.ctx.link.watch(),
            self.read_list(PageAt::First, Place::Top),
            self.read_sidebar(),
        ]
    }

    fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let focus = self.focus;
        let effects = match msg {
            Msg::Key(key) => self.on_key(key),
            Msg::Tick => {
                self.tick = self.tick.wrapping_add(1);
                self.status.tick(self.tick);
                Vec::new()
            }
            Msg::Resize(..) => Vec::new(),
            Msg::Task(TaskResult::Mail(event)) => self.mail_event(event),
            Msg::Task(TaskResult::EditorDone(result)) => {
                self.mail_event(MailEvent::EditorDone(result))
            }
            Msg::Task(TaskResult::JobPanicked(message)) => self.job_panicked(&message),
            Msg::Task(_) => Vec::new(),
        };
        if self.focus != focus {
            // A chord's first key belongs to the view it was typed in.
            self.chord.clear();
        }
        effects
    }

    fn view(&self, frame: &mut Frame, theme: &Theme) {
        self.render(frame, theme);
    }

    fn captures_text(&self) -> bool {
        match &self.modal {
            Some(modal) => modal.captures_text(),
            None => self.compose.as_ref().is_some_and(Composer::captures_text),
        }
    }

    /// The active view's own keys and a waiting chord are the screen's; the global quit key
    /// is the shell's unless the view binds it (`q` closes the reader).
    fn claims_key(&self, key: KeyEvent) -> bool {
        if self.modal.is_some() || self.chord.is_pending() || self.captures_text() {
            return true;
        }
        let mut chord = self.chord;
        let [view, _] = self.views();
        !matches!(
            self.ctx.keymap.resolve(&[view], &mut chord, key),
            Resolved::Unbound
        )
    }
}
