//! `h`: the snooze chooser and what it sends. The chooser opens once the store says which of the
//! threads are snoozed (its title then says until when, or how many threads it snoozes, and
//! "Return to inbox now" comes first); a time snoozes them with grace, `snooze.set` in calls of
//! up to 200 threads per account, which `z` takes back like an archive; returning sends
//! `snooze.clear` the same way. `snooze.woke` reads the list again and says what came back.

use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rail_core::guarded::{GuardedReadError, read_capped};
use rail_core::ops::{
    Action as MailAction, MAX_ACTION_THREADS, OpError, SnoozeClearResponse, SnoozeWoke,
};
use rail_core::paths::{Paths, write_private_atomic};

use super::MailScreen;
use super::actions::put_back_threads;
use super::actions_pick::Picking;
use super::bulk::SHOWN_SUBJECT_CHARS;
use super::selection::Picked;
use crate::tui::app::Effect;
use crate::tui::data::{ReadError, job};
use crate::tui::keys::Action;
use crate::tui::modals::{Modal, Picker, PickerKind};
use crate::tui::snooze::{parse_snooze, snooze_choices};
use crate::tui::text::{count, escape, fit};

/// The longest typed time kept for "Last used"; longer text is neither kept nor read back.
const LAST_CHARS: usize = 100;
/// The most of the file read: `LAST_CHARS` characters of at most four bytes each.
const LAST_BYTES: u64 = 4 * LAST_CHARS as u64;

/// "Return to inbox now" while its calls answer: what they put back so far, and the threads it
/// took from the selection, which a failed call gives back.
pub(super) struct Returning {
    pending: usize,
    returned: usize,
    selected: Vec<(i64, Picked)>,
    /// A failed call already gave its threads back.
    restored: bool,
    /// A call failed: its error stays on the bar while the others answer.
    failed: bool,
}

/// Counts and flags only: the threads' ids are not named.
impl fmt::Debug for Returning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Returning")
            .field("pending", &self.pending)
            .field("returned", &self.returned)
            .field("selected", &self.selected.len())
            .field("restored", &self.restored)
            .field("failed", &self.failed)
            .finish()
    }
}

/// What a snooze was sent for, and what its answered calls snoozed and left out (no mail in
/// the inbox).
#[derive(Debug, Clone, Copy)]
pub(super) struct SnoozeCount {
    pub(super) sent: usize,
    pub(super) snoozed: usize,
    pub(super) left_out: usize,
}

/// Where the last typed snooze time is kept, so the chooser offers it again after a restart.
fn last_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("snooze-last")
}

impl MailScreen {
    /// `h` on the list or the reader: read which of the threads are snoozed, then the chooser.
    pub(super) fn open_snooze(&mut self, targets: Vec<(i64, Picked)>) -> Vec<Effect> {
        let seq = self.next_seq();
        let threads = targets.iter().map(|(id, _)| *id).collect();
        self.actions.picking = Some(Picking {
            seq,
            mode: PickerKind::Snooze,
            targets,
        });
        vec![self.ctx.store.snoozes(seq, threads)]
    }

    /// The store said when the threads' snoozes end: the chooser opens, its title naming the end
    /// when the one thread chosen is snoozed, or counting the threads chosen.
    pub(super) fn snoozes_read(
        &mut self,
        seq: u64,
        result: Result<Vec<i64>, ReadError>,
    ) -> Vec<Effect> {
        let Some(count) = self
            .actions
            .picking
            .as_ref()
            .filter(|p| p.seq == seq && p.mode == PickerKind::Snooze)
            .map(|p| p.targets.len())
        else {
            return Vec::new();
        };
        match result {
            Ok(ends) => {
                let clock = self.ctx.clock.clone();
                let until = ends.first().copied().filter(|_| count == 1);
                let choices = snooze_choices(&clock);
                let last = (self.actions.last_snooze.as_deref())
                    .and_then(|t| parse_snooze(t, &clock).ok());
                let chooser =
                    Picker::snooze(&choices, clock, (count, until, !ends.is_empty()), last);
                self.modal = Some(Modal::Picker(chooser));
            }
            Err(error) => {
                self.actions.picking = None;
                self.side_read_failed(error, "snoozes");
            }
        }
        Vec::new()
    }

    /// A time was chosen: the threads are snoozed until then, with grace. Their archive closes
    /// the reader as archive does. A typed time (`said`) is kept for "Last used" unless it is
    /// longer than Rail reads back, so the session offers what a restart would.
    pub(super) fn send_snooze(
        &mut self,
        targets: &[(i64, Picked)],
        until: i64,
        said: Option<String>,
    ) -> Vec<Effect> {
        let kept = said
            .filter(|said| said.chars().count() <= LAST_CHARS)
            .map(|said| self.remember_snooze(said));
        let sent = self.send_with(&MailAction::Archive {}, targets, true, Some(until));
        kept.into_iter().chain(sent).collect()
    }

    /// The last typed time, read when the screen starts.
    pub(super) fn load_last_snooze(&mut self) {
        self.actions.last_snooze = read_last(&last_file(&self.ctx.paths));
    }

    /// Keep `said` for "Last used": for the session at once, and in the state file (owner-only,
    /// written atomically) off the screen's thread. A failed write is logged; the session keeps it.
    fn remember_snooze(&mut self, said: String) -> Effect {
        self.actions.last_snooze = Some(said.clone());
        let path = last_file(&self.ctx.paths);
        job(move |_sink| async move {
            let written =
                tokio::task::spawn_blocking(move || write_private_atomic(&path, said.as_bytes()))
                    .await;
            match written {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(
                    kind = ?error.kind(),
                    "could not keep the last snooze time in the state directory"
                ),
                Err(error) => tracing::warn!(
                    panicked = error.is_panic(),
                    "could not keep the last snooze time in the state directory"
                ),
            }
        })
    }

    /// "Return to inbox now": `snooze.clear` in calls of up to `MAX_ACTION_THREADS` threads per
    /// account, under one seq. Like any applied action, it uses up the selection it came from.
    pub(super) fn return_snoozed(&mut self, targets: &[(i64, Picked)]) -> Vec<Effect> {
        let selected = if self.targets_are_selection(targets) {
            self.actions.selection.clear();
            targets.to_vec()
        } else {
            Vec::new()
        };
        let seq = self.next_seq();
        let mut effects = Vec::new();
        for (account_id, threads) in self.group_by_account(targets) {
            let Some(account) = self.account_name(account_id) else {
                self.status
                    .notify("This account is no longer configured.", self.tick);
                continue;
            };
            for chunk in threads.chunks(MAX_ACTION_THREADS) {
                effects.push(self.ctx.link.unsnooze(seq, account.clone(), chunk.to_vec()));
            }
        }
        if !effects.is_empty() {
            let back = Returning {
                pending: effects.len(),
                returned: 0,
                selected,
                restored: false,
                failed: false,
            };
            self.actions.returning.insert(seq, back);
        }
        effects
    }

    /// One `snooze.clear` call of the Return with `seq` answered: the newest Return's notice sums
    /// what its calls put back (an older one's answers are not counted there). A failed call's
    /// error stays while the others answer, and its `threads` come back to the selection,
    /// whichever Return it belongs to.
    pub(super) fn unsnoozed(
        &mut self,
        seq: u64,
        threads: &[i64],
        result: Result<SnoozeClearResponse, OpError>,
    ) -> Vec<Effect> {
        let tick = self.tick;
        let newest = self.actions.returning.keys().next_back() == Some(&seq);
        let mut done = false;
        let failed = match (self.actions.returning.get_mut(&seq), result) {
            (Some(back), Ok(answer)) => {
                back.pending = back.pending.saturating_sub(1);
                back.returned = back.returned.saturating_add(answer.returned);
                if newest && !back.failed {
                    let text = format!("returned {} to the inbox", count(back.returned));
                    self.status.notify(text, tick);
                }
                done = back.pending == 0;
                None
            }
            (Some(back), Err(error)) => {
                back.pending = back.pending.saturating_sub(1);
                back.failed = true;
                let selection = &mut self.actions.selection;
                put_back_threads(&back.selected, &mut back.restored, selection, threads);
                done = back.pending == 0;
                Some(error)
            }
            (None, Ok(_)) => None,
            (None, Err(error)) => Some(error),
        };
        if done {
            self.actions.returning.remove(&seq);
        }
        if let Some(error) = failed {
            self.action_error("return to the inbox", &error);
        }
        self.dirty()
    }

    /// The notice once a snooze answered: when it ends, and `z` while it has a row to undo.
    /// Sent for more than one thread, it counts what its answered calls snoozed, and then any
    /// they left out: last, so a narrow bar cuts that count before the undo hint.
    pub(super) fn snoozed_notice(
        &self,
        until: i64,
        undoable: bool,
        SnoozeCount {
            sent,
            snoozed,
            left_out,
        }: SnoozeCount,
    ) -> String {
        let when = self.ctx.clock.ahead(until);
        let head = if sent == 1 {
            format!("Snoozed until {when}")
        } else {
            format!("Snoozed {} until {when}", count(snoozed))
        };
        let undo = if undoable {
            format!(" · {} undoes", self.ctx.keymap.label(Action::GlobalUndo))
        } else {
            String::new()
        };
        let left = if left_out > 0 {
            format!(" · {} not in the inbox", count(left_out))
        } else {
            String::new()
        };
        format!("{head}{undo}{left}")
    }

    /// `snooze.woke`: the list is read again, and the notice names the thread when one came back.
    pub(super) fn snooze_woke(&mut self, woke: SnoozeWoke) -> Vec<Effect> {
        let mut effects = self.dirty();
        effects.push(self.ctx.store.woke(woke.threads));
        effects
    }

    /// The wake's notice, once the store gave the subject of the one thread that came back.
    pub(super) fn woke_read(
        &mut self,
        count: usize,
        subject: Result<Option<String>, ReadError>,
    ) -> Vec<Effect> {
        let text = match subject {
            Ok(Some(subject)) if count == 1 => {
                let shown = fit(&escape(&subject), SHOWN_SUBJECT_CHARS);
                format!("Back from snooze: {shown}")
            }
            Ok(_) => back(count),
            Err(error) => {
                // The subject only names the thread: the count still says what came back.
                if let ReadError::Failed(error) = &error {
                    tracing::warn!(code = ?error.code, "a woken thread's subject could not be read");
                }
                back(count)
            }
        };
        self.status.notify(text, self.tick);
        Vec::new()
    }
}

/// The time kept in `path`, read through `read_capped`: only a regular file is opened, so a pipe
/// or a device there never holds up the start, and at most `LAST_BYTES` are read. A missing file
/// offers nothing; one that cannot be read is logged and offers nothing, as does text Rail would
/// not have written (empty, not UTF-8, or longer than `LAST_CHARS`).
pub(super) fn read_last(path: &Path) -> Option<String> {
    let bytes = match read_capped(path, LAST_BYTES) {
        Ok(bytes) => bytes,
        Err(GuardedReadError::Io(error)) if error.kind() == ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                ?error,
                "the last snooze time in the state directory is not read"
            );
            return None;
        }
    };
    let text = String::from_utf8(bytes).ok()?;
    let text = text.trim();
    (!text.is_empty() && text.chars().count() <= LAST_CHARS).then(|| text.to_string())
}

/// "1 thread back from snooze", "3 threads back from snooze".
fn back(count: usize) -> String {
    let threads = if count == 1 { "thread" } else { "threads" };
    format!("{count} {threads} back from snooze")
}
