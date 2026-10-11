//! `rail sync [<account>] [--no-wait]`: wake the sync and, unless `--no-wait`, show progress
//! until each account is caught up or fails. Exit 0 when all are caught up, 1 if any failed,
//! 2 if any needs a login (2 wins). Ctrl-C ends this command only; the daemon keeps syncing.

use std::collections::HashMap;
use std::process::ExitCode;
use std::time::Duration;

use rail_core::daemon::DaemonClient;
use rail_core::daemon::wire::Notification;
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
    let client = daemon_link::connector(paths)
        .client()
        .await
        .map_err(report)?;
    let mut notes = client.notifications();
    let request = SyncNowRequest {
        account,
        watch: !no_wait,
    };
    let targets = client
        .call::<SyncNow>(&request)
        .await
        .map_err(report)?
        .targets;
    if targets.is_empty() {
        return nothing_to_follow(&client).await;
    }
    if no_wait {
        for target in &targets {
            println!("{}: syncing in the background", escaped(&target.name));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut interrupt = signal(SignalKind::interrupt())?;
    let stall = crate::test_hooks::sync_stall().unwrap_or(STALL);
    let ended = follow(&client, &mut notes, &targets, &mut interrupt, stall)
        .await
        .map_err(report)?;
    Ok(finish(ended))
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
    client: &DaemonClient,
    notes: &mut broadcast::Receiver<Notification>,
    targets: &[SyncTarget],
    interrupt: &mut Signal,
    stall: Duration,
) -> Result<Ended, OpError> {
    tokio::select! {
        watched = watch(client, notes, targets, stall) => watched,
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
async fn watch(
    client: &DaemonClient,
    notes: &mut broadcast::Receiver<Notification>,
    targets: &[SyncTarget],
    stall: Duration,
) -> Result<Ended, OpError> {
    let mut shown: HashMap<String, String> = HashMap::new();
    let mut tick = tokio::time::interval(POLL);
    let mut quiet = Stall::new(stall, Instant::now());
    loop {
        let mine = statuses(client, targets).await?;
        for status in &mine {
            let line = progress_line(status, rail_core::auth::unix_now());
            if shown.get(&status.address) != Some(&line) {
                println!("{line}");
                shown.insert(status.address.clone(), line);
            }
        }
        if reached(targets, &mine) {
            return Ok(Ended::Done(mine));
        }
        if quiet.stalled(&mine, Instant::now()) {
            return Ok(Ended::Stalled(mine));
        }
        tokio::select! {
            _ = tick.tick() => {}
            note = notes.recv() => {
                if matches!(note, Err(broadcast::error::RecvError::Closed)) {
                    tick.tick().await;
                }
                drain(notes);
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

    /// T9-A: a watch ends only after the stall interval with no change at all.
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
