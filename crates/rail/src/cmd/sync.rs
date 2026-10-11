//! `rail sync [<account>] [--no-wait]`: wake the sync and, unless `--no-wait`, show progress
//! until each account is caught up or fails. Exit 0 when all are caught up, 1 if any failed,
//! 2 if any needs a login (2 wins). Ctrl-C ends this command only; the daemon keeps syncing.

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Duration;

use rail_core::daemon::wire::Notification;
use rail_core::daemon::{Connector, DaemonClient};
use rail_core::ops::op::{SyncNow, SyncStatus};
use rail_core::ops::{
    AccountSyncStatus, OpError, SyncNowRequest, SyncPhase, SyncStatusRequest, SyncTarget,
};
use rail_core::paths::Paths;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::time::Instant;

use crate::cmd::account::{escaped, outside, report};
use crate::cmd::text::stored;
use crate::daemon_link;

/// How long `rail sync` keeps watching while no watched account's status changes; it then
/// leaves the sync to the daemon. A sync that keeps making progress is watched to the end.
const STALL: Duration = Duration::from_secs(10 * 60);
/// How often the status is asked for between progress notifications.
const POLL: Duration = Duration::from_secs(1);
/// The exit code after Ctrl-C, as a shell reports a command SIGINT ended.
const INTERRUPTED: u8 = 130;
/// What a watch that saw no change for `STALL` says.
const STALLED: &str = "still syncing in the daemon; `rail doctor` shows progress";
/// How often one watch follows a restart (a new daemon, or the daemon restarting its sync)
/// before it gives up.
const MAX_REJOINS: u32 = 3;

/// Why a watch asks the daemon to sync again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejoin {
    /// The connection closed: a new daemon took over (an upgrade, `rail daemon stop`).
    DaemonGone,
    /// A pass count went down on a live connection: the daemon restarted its sync engines
    /// (its store writer restarted, or `config.toml` changed).
    SyncRestarted,
}

impl Rejoin {
    fn note(self) -> &'static str {
        match self {
            Self::DaemonGone => "the daemon restarted; asking the new one to sync",
            Self::SyncRestarted => "sync restarted in the daemon; asking it to sync again",
        }
    }
}

/// How a watch ended; each carries the accounts' statuses then.
enum Ended {
    /// Every target was reached.
    Done(Vec<AccountSyncStatus>),
    /// Nothing changed for the stall interval.
    Stalled(Vec<AccountSyncStatus>),
    Interrupted,
}

pub async fn run(
    paths: &Paths,
    account: Option<String>,
    no_wait: bool,
) -> color_eyre::Result<ExitCode> {
    let link = daemon_link::connector(paths);
    let request = SyncNowRequest {
        account,
        watch: !no_wait,
    };
    let mut session = Session::start(&link, request).await.map_err(report)?;
    if session.targets.is_empty() {
        return nothing_to_follow(&session.client).await;
    }
    if no_wait {
        for target in &session.targets {
            println!("{}: syncing in the background", escaped(&target.name));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut interrupt = signal(SignalKind::interrupt())?;
    let stall = crate::test_hooks::sync_stall().unwrap_or(STALL);
    let ended = follow(&mut session, &mut interrupt, stall)
        .await
        .map_err(report)?;
    Ok(finish(ended))
}

/// One watch: the connection, its notifications, the passes to wait for, and each account's
/// last pass count (a count that goes down means a restart).
struct Session<'a> {
    link: &'a Connector,
    request: SyncNowRequest,
    client: DaemonClient,
    notes: broadcast::Receiver<Notification>,
    targets: Vec<SyncTarget>,
    seen: HashMap<String, u64>,
    rejoins: u32,
}

impl<'a> Session<'a> {
    /// Connect (starting the daemon if needed) and send `sync.now`.
    async fn start(link: &'a Connector, request: SyncNowRequest) -> Result<Self, OpError> {
        let client = link.client().await?;
        let notes = client.notifications();
        let targets = client.call::<SyncNow>(&request).await?.targets;
        Ok(Self {
            link,
            request,
            client,
            notes,
            targets,
            seen: HashMap::new(),
            rejoins: 0,
        })
    }

    /// The targets' statuses. When the daemon went away (the connection closed) or restarted
    /// its sync (a pass count went down), connect again, send `sync.now` again and wait for
    /// the passes it names instead: the old pass numbers mean nothing there.
    async fn statuses(&mut self) -> Result<Vec<AccountSyncStatus>, OpError> {
        let why = match statuses(&self.client, &self.targets).await {
            Ok(mine) if !went_back(&self.seen, &mine) => {
                self.seen = passes(&mine);
                return Ok(mine);
            }
            Ok(_) => Rejoin::SyncRestarted,
            Err(_) if self.client.is_closed() => Rejoin::DaemonGone,
            Err(error) => return Err(error),
        };
        self.rejoin(why).await?;
        let mine = statuses(&self.client, &self.targets).await?;
        self.seen = passes(&mine);
        Ok(mine)
    }

    async fn rejoin(&mut self, why: Rejoin) -> Result<(), OpError> {
        self.rejoins += 1;
        if self.rejoins > MAX_REJOINS {
            return Err(OpError::internal(
                "the daemon or its sync kept restarting during `rail sync`",
            ));
        }
        eprintln!("{}", why.note());
        self.client = self.link.client().await?;
        self.notes = self.client.notifications();
        let fresh = self.client.call::<SyncNow>(&self.request).await?.targets;
        self.targets = rebase(&self.targets, fresh);
        Ok(())
    }
}

/// Each account's completed passes.
fn passes(mine: &[AccountSyncStatus]) -> HashMap<String, u64> {
    mine.iter()
        .map(|s| (s.address.clone(), s.passes_completed))
        .collect()
}

/// True when an account's pass count went down: the daemon, or its sync, restarted.
fn went_back(seen: &HashMap<String, u64>, mine: &[AccountSyncStatus]) -> bool {
    mine.iter().any(|s| {
        seen.get(&s.address)
            .is_some_and(|&was| s.passes_completed < was)
    })
}

/// The targets after a rejoin: the accounts watched before, with the passes the new daemon
/// named for them (an account it no longer syncs drops out).
fn rebase(old: &[SyncTarget], fresh: Vec<SyncTarget>) -> Vec<SyncTarget> {
    fresh
        .into_iter()
        .filter(|t| old.iter().any(|o| o.address == t.address))
        .collect()
}

/// Print how the watch ended; the exit code.
fn finish(ended: Ended) -> ExitCode {
    match ended {
        Ended::Done(finals) => print_finals(&finals),
        Ended::Stalled(now) => {
            let failed: Vec<AccountSyncStatus> = now
                .into_iter()
                .filter(|s| s.needs_auth || s.last_pass_failed)
                .collect();
            let code = print_finals(&failed);
            eprintln!("{STALLED}");
            code
        }
        Ended::Interrupted => {
            eprintln!("stopped watching; the sync carries on in the daemon (see `rail doctor`)");
            ExitCode::from(INTERRUPTED)
        }
    }
}

/// No engine to wake: say why for each Gmail account (a sync that could not start shows its
/// error), or that there is none.
async fn nothing_to_follow(client: &DaemonClient) -> color_eyre::Result<ExitCode> {
    let all = client
        .call::<SyncStatus>(&SyncStatusRequest::default())
        .await
        .map_err(report)?
        .accounts;
    let gmail: Vec<AccountSyncStatus> = all
        .into_iter()
        .filter(|s| s.phase != SyncPhase::Unsupported)
        .collect();
    if gmail.is_empty() {
        println!("No Gmail account to sync; add one with `rail account add gmail`.");
        return Ok(ExitCode::SUCCESS);
    }
    Ok(print_finals(&gmail))
}

fn print_finals(finals: &[AccountSyncStatus]) -> ExitCode {
    for status in finals {
        for line in final_lines(status) {
            println!("{line}");
        }
    }
    ExitCode::from(exit_code(finals))
}

/// Watch until every target is reached, nothing changes for `stall`, or Ctrl-C.
async fn follow(
    session: &mut Session<'_>,
    interrupt: &mut Signal,
    stall: Duration,
) -> Result<Ended, OpError> {
    tokio::select! {
        watched = watch(session, stall) => watched,
        _ = interrupt.recv() => Ok(Ended::Interrupted),
    }
}

/// When the watched statuses last changed.
struct Stall {
    limit: Duration,
    last: Vec<AccountSyncStatus>,
    since: Instant,
}

impl Stall {
    fn new(limit: Duration, now: Instant) -> Self {
        Self {
            limit,
            last: Vec::new(),
            since: now,
        }
    }

    /// True once `mine` has stayed the same for the limit.
    fn stalled(&mut self, mine: &[AccountSyncStatus], now: Instant) -> bool {
        if self.last != mine {
            self.last = mine.to_vec();
            self.since = now;
            return false;
        }
        now.duration_since(self.since) >= self.limit
    }
}

/// Print each account's progress line when it changes, until every target is reached or no
/// status changed for `stall`. Woken by each progress notification and at least every `POLL`.
async fn watch(session: &mut Session<'_>, stall: Duration) -> Result<Ended, OpError> {
    let mut shown: HashMap<String, String> = HashMap::new();
    let mut tick = tokio::time::interval(POLL);
    let mut quiet = Stall::new(stall, Instant::now());
    loop {
        let mine = session.statuses().await?;
        for status in &mine {
            let line = progress_line(status, rail_core::auth::unix_now());
            if shown.get(&status.address) != Some(&line) {
                println!("{line}");
                shown.insert(status.address.clone(), line);
            }
        }
        if reached(&session.targets, &mine) {
            return Ok(Ended::Done(mine));
        }
        if quiet.stalled(&mine, Instant::now()) {
            return Ok(Ended::Stalled(mine));
        }
        tokio::select! {
            _ = tick.tick() => {}
            note = session.notes.recv() => {
                if matches!(note, Err(broadcast::error::RecvError::Closed)) {
                    tick.tick().await;
                }
                drain(&mut session.notes);
            }
        }
    }
}

/// The targets' statuses, in the daemon's order.
async fn statuses(
    client: &DaemonClient,
    targets: &[SyncTarget],
) -> Result<Vec<AccountSyncStatus>, OpError> {
    let all = client
        .call::<SyncStatus>(&SyncStatusRequest::default())
        .await?
        .accounts;
    Ok(all
        .into_iter()
        .filter(|s| targets.iter().any(|t| t.address == s.address))
        .collect())
}

/// True once each target finished the pass it asked for, needs a login, or is gone.
fn reached(targets: &[SyncTarget], mine: &[AccountSyncStatus]) -> bool {
    targets.iter().all(|t| {
        mine.iter()
            .find(|s| s.address == t.address)
            .is_none_or(|s| s.needs_auth || s.passes_completed >= t.pass)
    })
}

/// Drop the notifications already queued: one status poll answers them all.
fn drain(notes: &mut broadcast::Receiver<Notification>) {
    while !matches!(
        notes.try_recv(),
        Err(TryRecvError::Empty | TryRecvError::Closed)
    ) {}
}

pub fn exit_code(finals: &[AccountSyncStatus]) -> u8 {
    if finals.iter().any(|s| s.needs_auth) {
        2
    } else if finals.iter().any(|s| s.last_pass_failed) {
        1
    } else {
        0
    }
}

/// One account's progress; `now` (Unix seconds) turns a retry time into "in Ns". Counts and
/// phases only: no mail content.
pub fn progress_line(status: &AccountSyncStatus, now: i64) -> String {
    let what = match status.phase {
        SyncPhase::Labels => "reading labels".to_string(),
        SyncPhase::Backfill => match status.backfill_estimate {
            Some(total) => format!(
                "fetching envelopes ({} of about {total})",
                status.backfill_fetched
            ),
            None => format!("fetching envelopes ({})", status.backfill_fetched),
        },
        SyncPhase::Delta => "fetching changes".to_string(),
        SyncPhase::Bodies => format!("fetching bodies ({})", status.bodies_fetched),
        SyncPhase::Prune => "tidying the store".to_string(),
        SyncPhase::Retrying => match status.next_attempt_at {
            Some(at) => format!("Gmail is busy; retrying in {}s", (at - now).max(0)),
            None => "Gmail is busy; retrying".to_string(),
        },
        SyncPhase::NeedsAuth => "needs a login".to_string(),
        SyncPhase::Idle | SyncPhase::Unsupported | SyncPhase::Stopped => {
            status.phase.as_str().to_string()
        }
    };
    format!("{}: {what}", escaped(&status.name))
}

/// How an account ended: up to date, needing a login, or failed with Rail's message and the
/// outside text quoted on a line of its own.
pub fn final_lines(status: &AccountSyncStatus) -> Vec<String> {
    let name = &escaped(&status.name);
    if status.needs_auth {
        return vec![format!(
            "{name}: needs a login; run `rail account login {name}`"
        )];
    }
    if !status.last_pass_failed {
        return vec![format!("{name}: up to date ({})", stored(status))];
    }
    let Some(error) = &status.last_error else {
        return vec![format!("{name}: sync failed")];
    };
    let mut lines = vec![format!("{name}: sync failed: {}", escaped(&error.message))];
    if let Some(detail) = &error.detail {
        lines.push(outside(detail.get()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pass count that went down on a live connection is the daemon restarting its sync
    /// (a writer restart, a config change), not a new daemon; each says what happened.
    #[test]
    fn a_sync_restart_is_not_called_a_daemon_restart() {
        assert_eq!(
            Rejoin::SyncRestarted.note(),
            "sync restarted in the daemon; asking it to sync again"
        );
        assert!(!Rejoin::SyncRestarted.note().contains("daemon restarted"));
        assert_eq!(
            Rejoin::DaemonGone.note(),
            "the daemon restarted; asking the new one to sync"
        );
    }
    use rail_core::config::AccountKind;
    use rail_core::ops::{SyncErrorInfo, SyncPhase, Untrusted};

    fn status(name: &str) -> AccountSyncStatus {
        AccountSyncStatus::new(name, &format!("{name}@example.com"), AccountKind::Gmail)
    }

    fn failed(name: &str) -> AccountSyncStatus {
        let mut s = status(name);
        s.last_pass_failed = true;
        s.last_error = Some(SyncErrorInfo {
            code: "permanent".into(),
            class: None,
            message: "Gmail refused the request (HTTP 400)".into(),
            at: 1_700_000_000,
            detail: Some(Untrusted::new("bad\u{1b}[2J request".into())),
        });
        s
    }

    fn needs_login(name: &str) -> AccountSyncStatus {
        let mut s = failed(name);
        s.needs_auth = true;
        s.phase = SyncPhase::NeedsAuth;
        s
    }

    #[test]
    fn exit_codes_say_caught_up_failed_or_needs_a_login() {
        assert_eq!(exit_code(&[status("a"), status("b")]), 0);
        assert_eq!(exit_code(&[status("a"), failed("b")]), 1);
        assert_eq!(exit_code(&[failed("a"), needs_login("b")]), 2);
        assert_eq!(exit_code(&[needs_login("a"), failed("b")]), 2, "2 beats 1");
        assert_eq!(exit_code(&[]), 0);
    }

    #[test]
    fn progress_lines_name_the_phase_and_counts() {
        let mut s = status("work");
        s.phase = SyncPhase::Backfill;
        s.backfill_fetched = 1200;
        s.backfill_estimate = Some(45_000);
        assert_eq!(
            progress_line(&s, 0),
            "work: fetching envelopes (1200 of about 45000)"
        );
        s.phase = SyncPhase::Bodies;
        s.bodies_fetched = 12;
        assert_eq!(progress_line(&s, 0), "work: fetching bodies (12)");
        s.phase = SyncPhase::Retrying;
        s.next_attempt_at = Some(1_700_000_030);
        assert_eq!(
            progress_line(&s, 1_700_000_000),
            "work: Gmail is busy; retrying in 30s"
        );
    }

    #[test]
    fn final_lines_quote_outside_text_on_a_line_of_its_own() {
        let mut ok = status("work");
        (ok.messages, ok.threads, ok.bodies) = (120, 80, 40);
        assert_eq!(
            final_lines(&ok),
            ["work: up to date (120 messages, 80 threads, 40 bodies)"]
        );
        let lines = final_lines(&failed("work"));
        assert_eq!(
            lines[0],
            "work: sync failed: Gmail refused the request (HTTP 400)"
        );
        assert!(lines[1].starts_with("from outside Rail"), "{lines:?}");
        assert!(!lines.join("\n").contains('\u{1b}'));
        assert_eq!(
            final_lines(&needs_login("work"))[0],
            "work: needs a login; run `rail account login work`"
        );
    }

    #[test]
    fn one_message_is_singular() {
        let mut ok = status("work");
        (ok.messages, ok.threads, ok.bodies) = (1, 1, 1);
        assert_eq!(
            final_lines(&ok),
            ["work: up to date (1 message, 1 thread, 1 body)"]
        );
    }

    /// A watch ends only after the stall interval with no change at all.
    #[test]
    fn a_watch_stalls_only_after_the_interval_without_change() {
        let start = Instant::now();
        let limit = Duration::from_secs(600);
        let mut quiet = Stall::new(limit, start);
        let mut s = status("work");
        assert!(!quiet.stalled(std::slice::from_ref(&s), start));
        let later = start + Duration::from_secs(599);
        assert!(!quiet.stalled(std::slice::from_ref(&s), later));
        s.backfill_fetched = 10;
        assert!(
            !quiet.stalled(std::slice::from_ref(&s), later),
            "progress resets it"
        );
        assert!(!quiet.stalled(std::slice::from_ref(&s), later + Duration::from_secs(599)));
        assert!(quiet.stalled(std::slice::from_ref(&s), later + limit));
    }

    fn target(name: &str, pass: u64) -> SyncTarget {
        SyncTarget {
            name: name.into(),
            address: format!("{name}@example.com"),
            pass,
        }
    }

    /// Review minor 9: a pass count that goes down means a new daemon, and the watch then
    /// waits for the passes the new daemon names, for the accounts it watched before.
    #[test]
    fn a_new_daemon_is_noticed_and_the_targets_rebased() {
        let mut before = status("work");
        before.passes_completed = 7;
        let seen = passes(std::slice::from_ref(&before));
        let mut same = before.clone();
        same.passes_completed = 8;
        assert!(!went_back(&seen, std::slice::from_ref(&same)));
        let mut restarted = before;
        restarted.passes_completed = 0;
        assert!(went_back(&seen, std::slice::from_ref(&restarted)));
        assert!(!went_back(&HashMap::new(), &[restarted]));
        let old = [target("work", 9)];
        let fresh = vec![target("work", 1), target("home", 2)];
        let rebased = rebase(&old, fresh);
        assert_eq!(rebased.len(), 1);
        assert_eq!((rebased[0].name.as_str(), rebased[0].pass), ("work", 1));
        let mut caught_up = status("work");
        caught_up.passes_completed = 1;
        assert!(reached(&rebased, &[caught_up.clone()]));
        assert!(
            !reached(&old, &[caught_up]),
            "the old target would never come"
        );
    }

    /// Account names reach the terminal escaped, like every other field.
    #[test]
    fn account_names_are_escaped() {
        let s = status("w\u{1b}[2J");
        assert_eq!(progress_line(&s, 0), "w\\u{1b}[2J: idle");
        assert!(
            !final_lines(&needs_login("w\u{1b}"))
                .join("")
                .contains('\u{1b}')
        );
    }
}
