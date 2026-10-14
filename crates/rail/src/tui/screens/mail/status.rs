//! The status bar: each account's sync state from `sync.status` and `sync.progress`, whether
//! the daemon is reachable, one transient notice, and the recent errors `:errors` lists.

use std::collections::{HashSet, VecDeque};

use rail_core::error::ErrorClass;
use rail_core::ops::{AccountSyncStatus, ErrorCode, OpError, SyncPhase};

use super::AccountEntry;
use super::view::Glyphs;
use crate::tui::clock::Clock;
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

/// The header's one phrase for every account, and whether it is a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub text: String,
    pub error: bool,
}

/// How bad an account's state is: the worst one names the summary.
const RANK_SYNCED: u8 = 0;
const RANK_BUSY: u8 = 1;
const RANK_RETRYING: u8 = 2;
const RANK_STOPPED: u8 = 3;
const RANK_BROKEN: u8 = 4;

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

    pub(super) fn status_of(&self, account: &AccountEntry) -> Option<&AccountSyncStatus> {
        self.statuses
            .iter()
            .find(|s| s.address.eq_ignore_ascii_case(&account.address))
    }

    /// The header's summary: the daemon when it is not connected, else the worst account's
    /// state (the first such account on a tie), failed outbox rows before a stopped account,
    /// `stopped` alone when every account is, and `Synced` at the earliest success time when
    /// nothing is wrong.
    pub fn summary(
        &self,
        accounts: &[AccountEntry],
        glyphs: &Glyphs,
        keymap: &Keymap,
        clock: &Clock,
    ) -> Summary {
        if let Some(text) = self.daemon_text(keymap) {
            return Summary { text, error: true };
        }
        let ranked: Vec<(u8, String)> = accounts
            .iter()
            .map(|a| rank(&escape(&a.name), self.status_of(a), glyphs))
            .collect();
        let failed: u64 = accounts
            .iter()
            .filter_map(|a| self.status_of(a))
            .map(|s| s.outbox.failed)
            .sum();
        let worst = ranked.iter().map(|(r, _)| *r).max().unwrap_or(RANK_BUSY);
        if failed > 0 && worst <= RANK_STOPPED {
            let text = format!("{} {failed} failed", glyphs.problem);
            return Summary { text, error: true };
        }
        if worst == RANK_STOPPED && ranked.iter().all(|(r, _)| *r == RANK_STOPPED) {
            return Summary {
                text: "stopped".to_string(),
                error: true,
            };
        }
        if worst > RANK_SYNCED {
            return worst_summary(&ranked, worst);
        }
        let earliest = accounts
            .iter()
            .filter_map(|a| self.status_of(a))
            .filter_map(|s| s.last_success_at)
            .min();
        let text = match earliest {
            Some(at) => format!("{} Synced {}", glyphs.account, clock.short(at)),
            None => NO_SYNC_YET.to_string(),
        };
        Summary { text, error: false }
    }

    /// The daemon's state when it is not plainly connected.
    fn daemon_text(&self, keymap: &Keymap) -> Option<String> {
        let refresh = keymap.label(Action::ListRefresh);
        match self.daemon {
            DaemonState::Connected => None,
            DaemonState::Connecting => Some("connecting to the daemon…".to_string()),
            DaemonState::Unreachable => Some(format!("daemon unreachable ({refresh} retries)")),
            DaemonState::Reachable => Some(format!(
                "daemon reachable again ({refresh} resumes live updates)"
            )),
        }
    }

    /// The bar's text (every part escaped): the notice, then each account.
    #[cfg(test)]
    pub fn line(&self, accounts: &[AccountEntry], glyphs: &Glyphs) -> String {
        let mut parts: Vec<String> = self.notice().map(str::to_string).into_iter().collect();
        for account in accounts {
            let status = self
                .statuses
                .iter()
                .find(|s| s.address.eq_ignore_ascii_case(&account.address));
            parts.push(account_part(&escape(&account.name), status, glyphs));
        }
        parts.join("  ·  ")
    }
}

const NO_SYNC_YET: &str = "– no sync yet";

/// The first account at the worst rank gives the phrase.
fn worst_summary(ranked: &[(u8, String)], worst: u8) -> Summary {
    let text = ranked
        .iter()
        .find(|(r, _)| *r == worst)
        .map_or_else(|| NO_SYNC_YET.to_string(), |(_, text)| text.clone());
    Summary {
        text,
        error: worst >= RANK_STOPPED,
    }
}

/// An account's rank and the summary phrase it would give.
fn rank(name: &str, s: Option<&AccountSyncStatus>, glyphs: &Glyphs) -> (u8, String) {
    let Some(s) = s else {
        return (RANK_BUSY, NO_SYNC_YET.to_string());
    };
    let needs_login = (
        RANK_BROKEN,
        format!("{} {name} needs login", glyphs.problem),
    );
    if s.needs_auth {
        return needs_login;
    }
    match s.phase {
        SyncPhase::Idle if s.last_pass_failed => (
            RANK_BROKEN,
            format!("{} {name} sync failed", glyphs.problem),
        ),
        SyncPhase::Idle if s.last_success_at.is_none() => (RANK_BUSY, NO_SYNC_YET.to_string()),
        SyncPhase::Idle => (RANK_SYNCED, String::new()),
        SyncPhase::Stopped => (RANK_STOPPED, format!("{name} stopped")),
        SyncPhase::Retrying => (RANK_RETRYING, format!("{} retrying", glyphs.syncing)),
        SyncPhase::Backfill => {
            let count = match s.backfill_estimate {
                Some(total) => format!("{}/{total}", s.backfill_fetched),
                None => s.backfill_fetched.to_string(),
            };
            (RANK_BUSY, format!("{} Backfill {count}", glyphs.syncing))
        }
        SyncPhase::Labels | SyncPhase::Delta | SyncPhase::Bodies | SyncPhase::Prune => {
            (RANK_BUSY, format!("{} Syncing {name}", glyphs.syncing))
        }
        SyncPhase::Unsupported => (RANK_BUSY, NO_SYNC_YET.to_string()),
        SyncPhase::NeedsAuth => needs_login,
    }
}

pub(super) fn account_part(
    name: &str,
    status: Option<&AccountSyncStatus>,
    glyphs: &Glyphs,
) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::clock::Clock;
    use crate::tui::test_util::{NOW, keymap, status, work_and_home};
    use jiff::tz::TimeZone;
    use rail_core::config::Icons;

    fn bar(statuses: Vec<AccountSyncStatus>) -> StatusBar {
        let mut bar = StatusBar::default();
        bar.connected(statuses);
        bar
    }

    fn summary_of(bar: &StatusBar) -> Summary {
        bar.summary(
            &work_and_home(),
            &Glyphs::of(Icons::Unicode),
            &keymap(),
            &Clock::fixed(NOW, TimeZone::UTC),
        )
    }

    #[test]
    fn every_account_synced_says_synced_at_the_earliest_time() {
        let mut work = status("work", "w@example.com", SyncPhase::Idle);
        work.last_success_at = Some(NOW - 60);
        let mut home = status("home", "h@example.com", SyncPhase::Idle);
        home.last_success_at = Some(NOW - 3_600);
        let s = summary_of(&bar(vec![work, home]));
        assert_eq!(
            s.text, "● Synced 11:40",
            "the earliest of the two, as Clock::short shows it"
        );
        assert!(!s.error);
    }

    #[test]
    fn the_worst_account_names_the_summary() {
        let cases: Vec<(SyncPhase, &str, bool)> = vec![
            (SyncPhase::Delta, "⟳ Syncing home", false),
            (SyncPhase::Retrying, "⟳ retrying", false),
            (SyncPhase::Stopped, "home stopped", true),
            (SyncPhase::NeedsAuth, "! home needs login", true),
        ];
        for (phase, text, error) in cases {
            let s = summary_of(&bar(vec![
                status("work", "w@example.com", SyncPhase::Idle),
                status("home", "h@example.com", phase),
            ]));
            assert_eq!(s.text, text, "{phase:?}");
            assert_eq!(s.error, error, "{phase:?}");
        }
    }

    #[test]
    fn a_backfill_shows_its_count_and_sync_failed_beats_syncing() {
        let mut home = status("home", "h@example.com", SyncPhase::Backfill);
        home.backfill_fetched = 1_200;
        home.backfill_estimate = Some(5_000);
        let s = summary_of(&bar(vec![
            status("work", "w@example.com", SyncPhase::Idle),
            home,
        ]));
        assert_eq!(s.text, "⟳ Backfill 1200/5000");
        let mut work = status("work", "w@example.com", SyncPhase::Idle);
        work.last_pass_failed = true;
        let s = summary_of(&bar(vec![
            work,
            status("home", "h@example.com", SyncPhase::Delta),
        ]));
        assert_eq!(s.text, "! work sync failed");
        assert!(s.error);
    }

    #[test]
    fn failed_rows_are_summed_and_beat_a_stopped_account() {
        let mut work = status("work", "w@example.com", SyncPhase::Stopped);
        work.outbox.failed = 2;
        let mut home = status("home", "h@example.com", SyncPhase::Idle);
        home.outbox.failed = 1;
        let s = summary_of(&bar(vec![work, home]));
        assert_eq!(s.text, "! 3 failed");
        assert!(s.error);
    }

    #[test]
    fn every_account_stopped_is_just_stopped_and_a_lost_daemon_beats_everything() {
        let s = summary_of(&bar(vec![
            status("work", "w@example.com", SyncPhase::Stopped),
            status("home", "h@example.com", SyncPhase::Stopped),
        ]));
        assert_eq!(s.text, "stopped");
        let mut lost = bar(vec![status("work", "w@example.com", SyncPhase::Idle)]);
        lost.daemon = DaemonState::Unreachable;
        let s = summary_of(&lost);
        assert_eq!(s.text, "daemon unreachable (Ctrl-r retries)");
        assert!(s.error);
    }

    #[test]
    fn no_status_yet_and_no_success_time_say_no_sync_yet() {
        let s = summary_of(&bar(Vec::new()));
        assert_eq!(s.text, "– no sync yet");
        let mut fresh = status("work", "w@example.com", SyncPhase::Idle);
        fresh.last_success_at = None;
        let mut other = status("home", "h@example.com", SyncPhase::Idle);
        other.last_success_at = None;
        assert_eq!(summary_of(&bar(vec![fresh, other])).text, "– no sync yet");
        let synced = status("work", "w@example.com", SyncPhase::Idle);
        let mut never = status("home", "h@example.com", SyncPhase::Idle);
        never.last_success_at = None;
        let s = summary_of(&bar(vec![synced, never]));
        assert_eq!(s.text, "– no sync yet", "one account that never synced");
        assert!(!s.error);
    }

    #[test]
    fn ascii_glyphs_and_an_escaped_account_name() {
        let mut bar = StatusBar::default();
        bar.connected(vec![status("work", "w@example.com", SyncPhase::Delta)]);
        let accounts = vec![crate::tui::screens::mail::AccountEntry {
            name: "Evil\u{7}Corp".into(),
            address: "w@example.com".into(),
            kind: rail_core::config::AccountKind::Gmail,
            color: None,
        }];
        let s = bar.summary(
            &accounts,
            &Glyphs::of(Icons::Ascii),
            &keymap(),
            &Clock::fixed(NOW, TimeZone::UTC),
        );
        assert_eq!(
            s.text, "~ Syncing Evil\\u{7}Corp",
            "escape writes the bell as its escape"
        );
    }
}
