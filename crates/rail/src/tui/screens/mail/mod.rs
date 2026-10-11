//! The mail screen: the thread list over the local store, the sidebar of accounts and labels,
//! and the status bar (the reader and the modals join in later tasks). Keys go through the
//! keymap; every read and daemon call is a job whose answer comes back as a `MailEvent`.

mod layout;
mod list;
#[cfg(test)]
mod modal_tests;
mod reading;
mod sidebar;
mod status;
#[cfg(test)]
mod tests;
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
    ScopeChoice, SidebarOutcome, StoreReader, Timer, timer,
};
use crate::tui::effects::Opener;
use crate::tui::keys::{Action, Chord, Keymap, Resolved, View};
use crate::tui::modals::{FilterLine, Help, Modal, Palette, PaletteEntry, Target};
use crate::tui::open::OpenFiles;
use crate::tui::screen::Screen;
use crate::tui::screens::reader::Reader;
use crate::tui::theme::Theme;

/// Rows the list is assumed to show before the first frame says.
const FALLBACK_HEIGHT: usize = 20;
/// Account colours for accounts without `color`, by position.
const PALETTE: [Color; 6] = [
    Color::Cyan,
    Color::Magenta,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Red,
];

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
}

/// One configured account, in `config.toml`'s order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEntry {
    pub name: String,
    pub address: String,
    pub kind: AccountKind,
    pub color: Color,
}

impl AccountEntry {
    /// The configured accounts; `color` read as a ratatui colour name or `#rrggbb`, else the
    /// palette by position.
    pub fn from_config(config: &Config) -> Vec<AccountEntry> {
        config
            .accounts
            .iter()
            .enumerate()
            .map(|(index, account)| AccountEntry {
                name: account.name.clone(),
                address: account.address.to_ascii_lowercase(),
                kind: account.kind,
                color: account
                    .color
                    .as_deref()
                    .and_then(|c| c.parse::<Color>().ok())
                    .unwrap_or(PALETTE[index % PALETTE.len()]),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Sidebar,
    Reader,
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
    /// The HTML files `o` wrote this session, removed when the reader closes and on drop.
    open_files: OpenFiles,
    /// The browser confirmation was answered yes this session.
    html_confirmed: bool,
    /// The message `o` is waiting on a body fetch for.
    pending_open: Option<i64>,
    seq: u64,
    latest_list: u64,
    /// The newest thread read: an older answer is dropped.
    latest_thread: u64,
    latest_sidebar: u64,
    requery_token: u64,
    requery_pending: bool,
    chord: Chord,
    tick: usize,
    panic_on_key: bool,
}

impl MailScreen {
    pub fn new(ctx: MailContext, accounts: Vec<AccountEntry>) -> Self {
        let open_files = OpenFiles::new(&ctx.paths.cache_dir);
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
            open_files,
            html_confirmed: false,
            pending_open: None,
            seq: 0,
            latest_list: 0,
            latest_thread: 0,
            latest_sidebar: 0,
            requery_token: 0,
            requery_pending: false,
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

    /// The store changed: re-read the visible window and the sidebar once things settle.
    pub(crate) fn dirty(&mut self) -> Vec<Effect> {
        if self.requery_pending {
            return Vec::new();
        }
        self.requery_pending = true;
        self.requery_token += 1;
        vec![timer(REQUERY_DEBOUNCE, self.requery_token, Timer::Requery)]
    }

    fn requery(&mut self) -> Vec<Effect> {
        self.requery_pending = false;
        self.list.cancel_loading();
        let (at, place) = self
            .navigating
            .unwrap_or_else(|| (self.list.requery_at(), Place::Keep));
        let list = self.list_request(at, place);
        let sidebar_seq = self.next_seq();
        self.latest_sidebar = sidebar_seq;
        let mut effects = vec![self.ctx.store.refresh(list, sidebar_seq)];
        if let Some(id) = self.reader.as_ref().map(Reader::thread_id) {
            effects.push(self.read_thread(id));
        }
        effects
    }

    /// Show `scope`. The old rows stay drawn until the new ones arrive (spec §3); a page
    /// read of the old scope still in flight is dropped by its sequence number.
    fn set_scope(&mut self, scope: ScopeChoice) -> Vec<Effect> {
        self.sidebar.point_at(&scope);
        self.scope = scope;
        self.list.cancel_loading();
        vec![self.read_list(PageAt::First, Place::Top)]
    }

    /// The views keys are read in, most specific first.
    pub(crate) fn views(&self) -> [View; 2] {
        let view = match self.focus {
            Focus::List => View::List,
            Focus::Sidebar => View::Sidebar,
            Focus::Reader => View::Reader,
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
        let views = self.views();
        match self.ctx.keymap.resolve(&views, &mut self.chord, key) {
            Resolved::Action(action) => self.act(action),
            Resolved::Pending | Resolved::Unbound => Vec::new(),
        }
    }

    pub(crate) fn act(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::GlobalPalette => {
                self.modal = Some(Modal::Palette(Palette::new(self.palette_entries())));
                return Vec::new();
            }
            Action::GlobalHelp => {
                self.modal = Some(Modal::Help(Help::new(&self.ctx.keymap, &self.views())));
                return Vec::new();
            }
            Action::ListFilter if self.focus == Focus::List => {
                self.modal = Some(Modal::Filter(FilterLine::new(&self.filter_text)));
                return Vec::new();
            }
            _ => {}
        }
        if let Some(effects) = self.shared_action(action) {
            return effects;
        }
        match self.focus {
            Focus::List => self.list_action(action),
            Focus::Sidebar => self.sidebar_action(action),
            Focus::Reader => self.reader_action(action),
        }
    }

    /// Actions of both the list and the sidebar.
    fn shared_action(&mut self, action: Action) -> Option<Vec<Effect>> {
        if self.focus == Focus::Reader {
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
            other => return account_number(other).map(|n| self.go_to_account(n)),
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
            DaemonState::Unreachable => {
                self.status.connecting();
                effects.push(self.ctx.link.watch());
            }
            DaemonState::Connecting => {}
        }
        effects.extend(self.requery());
        effects
    }

    fn on_mail(&mut self, event: MailEvent) -> Vec<Effect> {
        match event {
            MailEvent::Listed { seq, result } => self.listed(seq, result),
            MailEvent::Sidebar { seq, result } => self.sidebar_counted(seq, result),
            MailEvent::Thread {
                seq,
                thread_id,
                result,
            } if seq == self.latest_thread => self.thread_read(thread_id, result),
            MailEvent::Bodies { thread_id, result } => self.bodies_answered(thread_id, result),
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
            _ => Vec::new(),
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
                self.place = Place::Keep;
            }
            Err(error) => {
                self.list.cancel_loading();
                self.read_failed(error);
            }
        }
        Vec::new()
    }

    fn sidebar_counted(
        &mut self,
        seq: u64,
        result: Result<SidebarOutcome, ReadError>,
    ) -> Vec<Effect> {
        if seq != self.latest_sidebar {
            return Vec::new();
        }
        match result {
            Ok(outcome) => self.sidebar.rebuild(&self.accounts, &outcome),
            Err(error) => self.read_failed(error),
        }
        Vec::new()
    }

    fn read_failed(&mut self, error: ReadError) {
        match error {
            ReadError::NotSetUp => self.store_state = StoreState::NotSetUp,
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
        let line = self.status.line(
            &self.accounts,
            &Glyphs::of(self.ctx.icons),
            &self.ctx.keymap,
        );
        if self.filter.is_empty() {
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

    /// Every palette entry: each key action of the active view and the global ones with its key, `errors`, each account and label.
    pub(crate) fn palette_entries(&self) -> Vec<PaletteEntry> {
        let key = |a: Action| self.ctx.keymap.label(a);
        let views = self.views();
        let mut entries: Vec<PaletteEntry> = Action::ALL
            .iter()
            .filter(|a| a.views().iter().any(|v| views.contains(v)))
            .map(|&a| PaletteEntry {
                text: a.description().to_string(),
                search: format!("{} {}", a.description(), a.id()),
                key: key(a),
                target: Target::Action(a),
            })
            .collect();
        entries.push(PaletteEntry {
            text: "Show recent errors".into(),
            search: "errors Show recent errors".into(),
            key: String::new(),
            target: Target::Errors,
        });
        entries.extend(self.accounts.iter().enumerate().map(|(i, a)| PaletteEntry {
            text: format!("Go to {}'s inbox", a.name),
            search: format!("Go to {}'s inbox account {} {}", a.name, a.name, a.address),
            key: account_action(i + 1).map(key).unwrap_or_default(),
            target: Target::Scope(ScopeChoice::Account(i)),
        }));
        entries.extend(
            self.sidebar
                .items()
                .iter()
                .filter(|i| i.depth > 0)
                .map(|item| {
                    let account = item
                        .account
                        .and_then(|i| self.accounts.get(i))
                        .map_or("", |a| a.name.as_str());
                    PaletteEntry {
                        text: format!("Label: {account} · {}", item.label),
                        search: format!("label {account} {}", item.label),
                        key: String::new(),
                        target: Target::Scope(item.scope.clone()),
                    }
                }),
        );
        entries
    }
}

/// The `list.account_n` action for account number `n` (1 to 9).
fn account_action(n: usize) -> Option<Action> {
    let action = match n {
        1 => Action::ListAccount1,
        2 => Action::ListAccount2,
        3 => Action::ListAccount3,
        4 => Action::ListAccount4,
        5 => Action::ListAccount5,
        6 => Action::ListAccount6,
        7 => Action::ListAccount7,
        8 => Action::ListAccount8,
        9 => Action::ListAccount9,
        _ => return None,
    };
    Some(action)
}

/// The account number of a `list.account_n` action.
fn account_number(action: Action) -> Option<usize> {
    let number = match action {
        Action::ListAccount1 => 1,
        Action::ListAccount2 => 2,
        Action::ListAccount3 => 3,
        Action::ListAccount4 => 4,
        Action::ListAccount5 => 5,
        Action::ListAccount6 => 6,
        Action::ListAccount7 => 7,
        Action::ListAccount8 => 8,
        Action::ListAccount9 => 9,
        _ => return None,
    };
    Some(number)
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
        vec![
            self.ctx.link.watch(),
            self.read_list(PageAt::First, Place::Top),
            self.read_sidebar(),
        ]
    }

    fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(key) => self.on_key(key),
            Msg::Tick => {
                self.tick = self.tick.wrapping_add(1);
                self.status.tick(self.tick);
                Vec::new()
            }
            Msg::Resize(..) => Vec::new(),
            Msg::Task(TaskResult::Mail(event)) => self.on_mail(event),
            Msg::Task(TaskResult::JobPanicked(message)) => {
                tracing::error!(panic = %message, "a mail screen job panicked");
                let text = format!(
                    "internal error: {}; see rail.log",
                    crate::tui::text::escape(&message)
                );
                self.status.error(text, self.ctx.clock.now(), self.tick);
                Vec::new()
            }
            Msg::Task(_) => Vec::new(),
        }
    }

    fn view(&self, frame: &mut Frame, theme: &Theme) {
        self.render(frame, theme);
    }

    fn captures_text(&self) -> bool {
        self.modal.as_ref().is_some_and(Modal::captures_text)
    }

    /// The active view's own keys and a waiting chord are the screen's; the global quit key
    /// is the shell's unless the view binds it (`q` closes the reader).
    fn claims_key(&self, key: KeyEvent) -> bool {
        if self.modal.is_some() || self.chord.is_pending() {
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
