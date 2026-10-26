//! `h`: the snooze chooser and what it sends. The chooser opens once the store says which of the
//! threads are snoozed (its title then says until when, and "Return to inbox now" comes first);
//! a time snoozes them with grace, one `snooze.set` per account, which `z` takes back like an
//! archive; returning sends `snooze.clear`. `snooze.woke` reads the list again and says what came
//! back.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use rail_core::guarded::{GuardedReadError, read_capped};
use rail_core::ops::{Action as MailAction, OpError, SnoozeClearResponse, SnoozeWoke};
use rail_core::paths::{Paths, write_private_atomic};

use super::MailScreen;
use super::actions_pick::Picking;
use super::selection::Picked;
use crate::tui::app::Effect;
use crate::tui::data::{ReadError, job};
use crate::tui::keys::Action;
use crate::tui::modals::{Modal, Picker, PickerKind};
use crate::tui::snooze::{parse_snooze, snooze_choices};
use crate::tui::text::{escape, fit};

/// The most of a subject the wake notice shows; the status bar cuts it to its width too.
const SHOWN_SUBJECT_CHARS: usize = 120;
/// The longest typed time kept for "Last used"; longer text is neither kept nor read back.
const LAST_CHARS: usize = 100;
/// The most of the file read: `LAST_CHARS` characters of at most four bytes each.
const LAST_BYTES: u64 = 4 * LAST_CHARS as u64;

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
    /// when the one thread chosen is snoozed.
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
                let last = (self.last_snooze.as_deref()).and_then(|t| parse_snooze(t, &clock).ok());
                let chooser = Picker::snooze(&choices, clock, (until, !ends.is_empty()), last);
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
        let sent = self.send_with(MailAction::Archive {}, targets, true, Some(until));
        kept.into_iter().chain(sent).collect()
    }

    /// The last typed time, read when the screen starts.
    pub(super) fn load_last_snooze(&mut self) {
        self.last_snooze = read_last(&last_file(&self.ctx.paths));
    }

    /// Keep `said` for "Last used": for the session at once, and in the state file (owner-only,
    /// written atomically) off the screen's thread. A failed write is logged; the session keeps it.
    fn remember_snooze(&mut self, said: String) -> Effect {
        self.last_snooze = Some(said.clone());
        let path = last_file(&self.ctx.paths);
        job(move |_sink| async move {
            let written =
                tokio::task::spawn_blocking(move || write_private_atomic(&path, said.as_bytes()))
                    .await;
            if !matches!(written, Ok(Ok(()))) {
                tracing::warn!("could not keep the last snooze time in the state directory");
            }
        })
    }

    /// "Return to inbox now": one `snooze.clear` per account.
    pub(super) fn return_snoozed(&mut self, targets: &[(i64, Picked)]) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (account_id, threads) in self.group_by_account(targets) {
            let Some(account) = self.account_name(account_id) else {
                self.status
                    .notify("This account is no longer configured.", self.tick);
                continue;
            };
            effects.push(self.ctx.link.unsnooze(account, threads));
        }
        effects
    }

    /// `snooze.clear` answered: how many went back, or why not.
    pub(super) fn unsnoozed(
        &mut self,
        result: Result<SnoozeClearResponse, OpError>,
    ) -> Vec<Effect> {
        match result {
            Ok(answer) => {
                let text = format!("returned {} to the inbox", answer.returned);
                self.status.notify(text, self.tick);
            }
            Err(error) => self.action_error("return to the inbox", &error),
        }
        self.dirty()
    }

    /// The notice once a snooze answered: when it ends, and `z` while it has a row to undo.
    pub(super) fn snoozed_notice(&self, until: i64, undoable: bool) -> String {
        let when = self.ctx.clock.ahead(until);
        if !undoable {
            return format!("Snoozed until {when}");
        }
        let key = self.ctx.keymap.label(Action::GlobalUndo);
        format!("Snoozed until {when} · {key} undoes")
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
