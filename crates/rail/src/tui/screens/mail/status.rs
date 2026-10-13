//! The status bar: each account's sync state from `sync.status` and `sync.progress`, whether
//! the daemon is reachable, one transient notice, and the recent errors `:errors` lists.

use std::collections::{HashSet, VecDeque};

use rail_core::error::ErrorClass;
use rail_core::ops::{AccountSyncStatus, ErrorCode, OpError, SyncPhase};

use super::AccountEntry;
use super::view::Glyphs;
use crate::tui::keys::{Action, Keymap};
use crate::tui::text::{escape, outside};

/// Ticks (250 ms each) a notice stays: five seconds.
pub const NOTICE_TICKS: usize = 20;
/// Errors `:errors` keeps.
pub const MAX_ERRORS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    Connecting,
    Connected,
    Unreachable,
    /// The watch was lost, but a request (a body fetch) got through since: the daemon is
    /// back, and live updates wait for `Ctrl-r`.
    Reachable,
}

/// One error as shown: when (Unix seconds) and the escaped text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEntry {
    pub at: i64,
    pub text: String,
    /// The failed outbox row `Enter` in `:errors` sends again.
    pub retry: Option<rail_core::ops::RowToken>,
}

#[derive(Debug)]
pub struct StatusBar {
    statuses: Vec<AccountSyncStatus>,
    pub daemon: DaemonState,
    /// The notice, the tick it expires at, and whether it is an error.
    notice: Option<(String, usize, bool)>,
    errors: VecDeque<ErrorEntry>,
    /// Accounts whose login hint was shown this session.
    hinted: HashSet<String>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            statuses: Vec::new(),
            daemon: DaemonState::Connecting,
            notice: None,
            errors: VecDeque::new(),
            hinted: HashSet::new(),
        }
    }
}

/// An error on one line: Rail's message, the outside text labelled and escaped, the hint.
pub fn error_line(error: &OpError) -> String {
    let mut line = escape(&error.message);
    if let Some(detail) = &error.detail {
        line.push_str(&format!(" ({})", outside(detail.get())));
    }
    if let Some(hint) = &error.hint {
        line.push_str(&format!(" hint: {}", escape(hint)));
    }
    line
}

impl StatusBar {
    pub fn connected(&mut self, statuses: Vec<AccountSyncStatus>) {
        self.statuses = statuses;
        self.daemon = DaemonState::Connected;
    }

    pub fn progress(&mut self, status: AccountSyncStatus) {
        match self
            .statuses
            .iter_mut()
            .find(|s| s.address == status.address)
        {
            Some(known) => *known = status,
            None => self.statuses.push(status),
        }
    }

    pub fn connecting(&mut self) {
        self.daemon = DaemonState::Connecting;
    }

    /// The daemon went away (or could not be reached).
    pub fn lost(&mut self, error: &OpError, now: i64, tick: usize) {
        self.daemon = DaemonState::Unreachable;
        self.error(error_line(error), now, tick);
    }

    /// A request reached the daemon: after a lost watch, the daemon is reachable again.
    pub fn answered(&mut self) {
        if self.daemon == DaemonState::Unreachable {
            self.daemon = DaemonState::Reachable;
        }
    }

    /// A request failed in the way a lost link does (the daemon closed the connection or is
    /// not serving): after "reachable again", the daemon is unreachable once more.
    pub fn failed(&mut self, error: &OpError) {
        let link_down = error.code == ErrorCode::Daemon && error.class == ErrorClass::Transient;
        if link_down && self.daemon == DaemonState::Reachable {
            self.daemon = DaemonState::Unreachable;
        }
    }

    pub fn notify(&mut self, text: impl Into<String>, tick: usize) {
        self.notice = Some((text.into(), tick + NOTICE_TICKS, false));
    }

    /// Show `text` (already escaped) and keep it for `:errors`. The caller logs the code.
    pub fn error(&mut self, text: String, now: i64, tick: usize) {
        self.notice = Some((text.clone(), tick + NOTICE_TICKS, true));
        self.errors.push_back(ErrorEntry {
            at: now,
            text,
            retry: None,
        });
        if self.errors.len() > MAX_ERRORS {
            self.errors.pop_front();
        }
    }

    /// A notice that stays `ticks` ticks.
    pub fn notify_for(&mut self, text: impl Into<String>, tick: usize, ticks: usize) {
        self.notice = Some((text.into(), tick + ticks, false));
    }

    /// `error`, kept in `:errors` with the outbox row `Enter` retries.
    pub fn error_with_retry(
        &mut self,
        text: String,
        now: i64,
        tick: usize,
        row: rail_core::ops::RowToken,
    ) {
        self.error(text, now, tick);
        if let Some(last) = self.errors.back_mut() {
            last.retry = Some(row);
        }
    }

    /// Some `:errors` line still offers to retry `row`.
    pub fn offers_retry(&self, row: rail_core::ops::OutboxId) -> bool {
        self.errors
            .iter()
            .any(|e| e.retry.as_ref().is_some_and(|t| t.row == row))
    }

    /// A retried row's `:errors` lines no longer offer a retry.
    pub fn retried(&mut self, row: rail_core::ops::OutboxId) {
        for entry in self
            .errors
            .iter_mut()
            .filter(|e| e.retry.as_ref().is_some_and(|t| t.row == row))
        {
            entry.retry = None;
        }
    }

    /// True the first time an account's login hint is asked for this session.
    pub fn first_login_hint(&mut self, account: &str) -> bool {
        self.hinted.insert(account.to_string())
    }

    pub fn tick(&mut self, tick: usize) {
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, until, _)| tick >= *until)
        {
            self.notice = None;
        }
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(text, _, _)| text.as_str())
    }

    pub fn notice_is_error(&self) -> bool {
        self.notice.as_ref().is_some_and(|(_, _, error)| *error)
    }

    pub fn errors(&self) -> impl DoubleEndedIterator<Item = &ErrorEntry> {
        self.errors.iter()
    }

    /// The bar's text (every part escaped): the notice, each account, the daemon.
    pub fn line(&self, accounts: &[AccountEntry], glyphs: &Glyphs, keymap: &Keymap) -> String {
        let mut parts: Vec<String> = self.notice().map(str::to_string).into_iter().collect();
        for account in accounts {
            let status = self
                .statuses
                .iter()
                .find(|s| s.address.eq_ignore_ascii_case(&account.address));
            parts.push(account_part(&escape(&account.name), status, glyphs));
        }
        match self.daemon {
            DaemonState::Connected => {}
            DaemonState::Connecting => parts.push("connecting to the daemon…".to_string()),
            DaemonState::Unreachable => parts.push(format!(
                "daemon unreachable ({} retries)",
                keymap.label(Action::ListRefresh)
            )),
            DaemonState::Reachable => parts.push(format!(
                "daemon reachable again ({} resumes live updates)",
                keymap.label(Action::ListRefresh)
            )),
        }
        parts.join("  ·  ")
    }
}

fn account_part(name: &str, status: Option<&AccountSyncStatus>, glyphs: &Glyphs) -> String {
    let Some(s) = status else {
        return format!("{name} …");
    };
    let mut part = sync_part(name, s, glyphs);
    if s.outbox.sending > 0 {
        part.push_str(" sending…");
    } else if s.outbox.saving > 0 {
        part.push_str(&format!(" saving to {}…", s.kind.provider_name()));
    }
    if s.outbox.pending > 0 {
        part.push_str(&format!(" {} queued", s.outbox.pending));
    }
    if s.outbox.failed > 0 {
        part.push_str(&format!(" {} failed", s.outbox.failed));
    }
    part
}

fn sync_part(name: &str, s: &AccountSyncStatus, glyphs: &Glyphs) -> String {
    if s.needs_auth {
        return format!("{name} {} needs login", glyphs.problem);
    }
    match s.phase {
        SyncPhase::Unsupported => format!("{name} - no sync yet"),
        SyncPhase::Backfill => match s.backfill_estimate {
            Some(total) => format!("{name} {} {}/{total}", glyphs.syncing, s.backfill_fetched),
            None => format!("{name} {} {}", glyphs.syncing, s.backfill_fetched),
        },
        SyncPhase::Labels | SyncPhase::Delta | SyncPhase::Bodies | SyncPhase::Prune => {
            format!("{name} {}", glyphs.syncing)
        }
        SyncPhase::Retrying => format!("{name} {} retrying", glyphs.syncing),
        SyncPhase::Stopped => format!("{name} stopped"),
        SyncPhase::NeedsAuth => format!("{name} {} needs login", glyphs.problem),
        SyncPhase::Idle if s.last_pass_failed => format!("{name} {} sync failed", glyphs.problem),
        SyncPhase::Idle => format!("{name} {}", glyphs.ok),
    }
}
