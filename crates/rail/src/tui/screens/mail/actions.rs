//! The mail screen's actions: the selection, the six thread actions from the list and the
//! reader, the trash confirmation, the undo notice and `z`, mark-read on open, failures on
//! their rows and in `:errors`. Every call goes through the daemon link; the store change
//! comes back as a re-read.

use std::collections::BTreeMap;

use rail_core::ops::{Action as MailAction, ActionApplyResponse, ActionFailed, OpError, OutboxId};
use rail_core::store::view::ThreadDetail;

use super::selection::{Picked, Selection};
use super::{Focus, MailScreen, error_line};
use crate::tui::app::Effect;
use crate::tui::data::{ApplyCall, MailEvent};
use crate::tui::keys::Action;
use crate::tui::modals::{Command, Confirm, Modal};
use crate::tui::text::{escape, fit};

/// Ticks the undo notice stays: `UNDO_GRACE` (10 s) at one tick per 250 ms.
pub const UNDO_TICKS: usize = 40;
const SHOWN_SUBJECT_CHARS: usize = 120;

/// The newest action with grace, while `z` can take it back.
#[derive(Debug)]
pub(super) struct LastAction {
    seq: u64,
    rows: Vec<OutboxId>,
    count: usize,
    until: usize,
}

#[derive(Debug, Default)]
pub(crate) struct ActionState {
    pub(crate) selection: Selection,
    last: Option<LastAction>,
    seq: u64,
    /// Threads whose action failed: the row and the escaped line on their list row.
    pub(crate) failures: BTreeMap<i64, (OutboxId, String)>,
    /// What `y` on the trash confirmation applies to.
    trashing: Vec<(i64, Picked)>,
    /// The picker waiting for its labels (filled in by the label pickers).
    #[allow(dead_code, reason = "the label pickers read it")]
    pub(super) picking: Option<super::actions_pick::Picking>,
}

/// "archived" and "archive", for the notice and the failure line.
pub(crate) fn verbs(action: &MailAction) -> (&'static str, &'static str) {
    match action {
        MailAction::Archive {} => ("archived", "archive"),
        MailAction::Trash {} => ("trashed", "trash"),
        MailAction::Star { on: true } => ("starred", "star"),
        MailAction::Star { on: false } => ("unstarred", "unstar"),
        MailAction::Read { on: true } => ("marked read", "mark read"),
        MailAction::Read { on: false } => ("marked unread", "mark unread"),
        MailAction::Labels { .. } => ("labelled", "change the labels of"),
        MailAction::Move { .. } => ("moved", "move"),
    }
}

impl MailScreen {
    /// The action keys; `None` for every other action.
    pub(super) fn mail_action(&mut self, action: Action) -> Option<Vec<Effect>> {
        let thread_view = matches!(self.focus, Focus::List | Focus::Reader);
        Some(match action {
            Action::GlobalUndo => self.undo(),
            Action::ListSelect if self.focus == Focus::List => self.toggle_selected(),
            Action::ListClearSelection if self.focus == Focus::List => {
                self.actions.selection.clear();
                Vec::new()
            }
            Action::ListArchive
            | Action::ListTrash
            | Action::ListStar
            | Action::ListRead
            | Action::ListLabels
            | Action::ListMove
                if thread_view =>
            {
                self.thread_action(action)
            }
            _ => return None,
        })
    }

    fn toggle_selected(&mut self) -> Vec<Effect> {
        if let Some(row) = self.list.selected().cloned() {
            self.actions.selection.toggle(&row);
        }
        self.list_action(Action::ListDown)
    }

    /// The reader's thread; else the selection; else the cursor row.
    pub(super) fn targets(&self) -> Vec<(i64, Picked)> {
        if self.focus == Focus::Reader {
            return self
                .reader
                .as_ref()
                .map(|r| {
                    let d = r.detail();
                    let picked = Picked {
                        account_id: d.account_id,
                        starred: d.messages.iter().any(|m| m.is_starred),
                        unread: d.messages.iter().any(|m| !m.is_read),
                    };
                    vec![(d.thread_id, picked)]
                })
                .unwrap_or_default();
        }
        if !self.actions.selection.is_empty() {
            return self.actions.selection.targets();
        }
        self.list
            .selected()
            .map(|r| vec![(r.key.thread_id, Picked::of(r))])
            .unwrap_or_default()
    }

    fn thread_action(&mut self, action: Action) -> Vec<Effect> {
        let targets = self.targets();
        if targets.is_empty() {
            self.status.notify("No thread to act on.", self.tick);
            return Vec::new();
        }
        match action {
            Action::ListArchive => self.send_action(MailAction::Archive {}, &targets, true),
            Action::ListTrash => self.confirm_trash(targets),
            Action::ListStar => {
                let on = !targets.iter().all(|(_, p)| p.starred);
                self.send_action(MailAction::Star { on }, &targets, true)
            }
            Action::ListRead => {
                let on = targets.iter().any(|(_, p)| p.unread);
                self.send_action(MailAction::Read { on }, &targets, true)
            }
            Action::ListLabels => self.open_picker(crate::tui::modals::PickerMode::Toggle, targets),
            Action::ListMove => self.open_picker(crate::tui::modals::PickerMode::Move, targets),
            _ => Vec::new(),
        }
    }

    fn account_name(&self, account_id: i64) -> Option<String> {
        let index = self
            .account_ids
            .iter()
            .position(|id| *id == Some(account_id))?;
        self.accounts.get(index).map(|a| a.name.clone())
    }

    /// Send `action` for `targets`, one call per account; with grace it becomes the undoable
    /// action. Clears the selection; archive, trash and move close the reader.
    pub(super) fn send_action(
        &mut self,
        action: MailAction,
        targets: &[(i64, Picked)],
        grace: bool,
    ) -> Vec<Effect> {
        let mut by_account: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for (thread, picked) in targets {
            by_account
                .entry(picked.account_id)
                .or_default()
                .push(*thread);
            self.actions.failures.remove(thread);
        }
        self.actions.seq += 1;
        let seq = self.actions.seq;
        if grace {
            self.actions.last = Some(LastAction {
                seq,
                rows: Vec::new(),
                count: targets.len(),
                until: self.tick + UNDO_TICKS,
            });
            self.actions.selection.clear();
        }
        let leaves = matches!(
            action,
            MailAction::Archive {} | MailAction::Trash {} | MailAction::Move { .. }
        );
        if grace && leaves && self.focus == Focus::Reader {
            self.close_reader();
        }
        let mut effects = Vec::new();
        for (account_id, threads) in by_account {
            let Some(account) = self.account_name(account_id) else {
                self.status
                    .notify("This account is no longer configured.", self.tick);
                continue;
            };
            effects.push(self.ctx.link.apply(ApplyCall {
                seq,
                account,
                threads,
                action: action.clone(),
                grace,
            }));
        }
        effects
    }

    fn confirm_trash(&mut self, targets: Vec<(i64, Picked)>) -> Vec<Effect> {
        let first = targets.first().map(|(id, _)| *id);
        let subject = self
            .list
            .rows()
            .iter()
            .find(|r| Some(r.key.thread_id) == first)
            .map(|r| r.subject.clone())
            .or_else(|| self.reader.as_ref().map(|r| r.detail().subject.clone()));
        let line = match (targets.len(), subject) {
            (1, Some(subject)) => {
                format!("Trash \"{}\"?", fit(&escape(&subject), SHOWN_SUBJECT_CHARS))
            }
            (n, _) => format!("Trash {n} threads?"),
        };
        let lines = vec![
            line,
            "Gmail keeps trashed mail in its Trash for 30 days.".to_string(),
        ];
        self.actions.trashing = targets;
        self.modal = Some(Modal::Confirm(Confirm::new(
            "Trash?",
            lines,
            Command::Trash,
        )));
        Vec::new()
    }

    fn undo(&mut self) -> Vec<Effect> {
        match self.actions.last.take() {
            Some(last) if self.tick < last.until && !last.rows.is_empty() => {
                vec![self.ctx.link.undo(last.rows)]
            }
            _ => {
                self.status.notify("Nothing to undo.", self.tick);
                Vec::new()
            }
        }
    }

    /// Mark-read on open: no grace, no notice, nothing to undo.
    pub(super) fn mark_read_on_open(&mut self, detail: &ThreadDetail) -> Vec<Effect> {
        if detail.messages.iter().all(|m| m.is_read) {
            return Vec::new();
        }
        let picked = Picked {
            account_id: detail.account_id,
            starred: false,
            unread: true,
        };
        self.send_action(
            MailAction::Read { on: true },
            &[(detail.thread_id, picked)],
            false,
        )
    }

    pub(super) fn action_command(&mut self, command: Command) -> Vec<Effect> {
        match command {
            Command::Trash => {
                let targets = std::mem::take(&mut self.actions.trashing);
                self.send_action(MailAction::Trash {}, &targets, true)
            }
            Command::Retry(row) => {
                self.status.retried(row);
                self.actions.failures.retain(|_, (r, _)| *r != row);
                vec![self.ctx.link.retry(row)]
            }
            other => self.picker_command(other),
        }
    }

    pub(super) fn action_event(&mut self, event: MailEvent) -> Vec<Effect> {
        match event {
            MailEvent::Applied { call, result } => self.applied(&call, result),
            MailEvent::Undone(result) => {
                match result {
                    Ok(answer) if answer.too_late.is_empty() => {
                        self.status.notify("undone", self.tick);
                    }
                    Ok(_) => self.status.notify("too late to undo", self.tick),
                    Err(error) => self.action_error("undo", &error),
                }
                self.dirty()
            }
            MailEvent::Retried { result, .. } => {
                match result {
                    Ok(answer) if answer.retried => {
                        self.status.notify("sending again", self.tick);
                    }
                    Ok(_) => self
                        .status
                        .notify("That action is no longer waiting for a retry.", self.tick),
                    Err(error) => self.action_error("retry", &error),
                }
                self.dirty()
            }
            MailEvent::ActionFailed(failure) => self.action_failed(*failure),
            MailEvent::Labels { seq, result } => self.labels_read(seq, result),
            _ => Vec::new(),
        }
    }

    fn applied(
        &mut self,
        call: &ApplyCall,
        result: Result<ActionApplyResponse, OpError>,
    ) -> Vec<Effect> {
        match result {
            Ok(answer) => {
                if let Some(last) = self
                    .actions
                    .last
                    .as_mut()
                    .filter(|l| l.seq == call.seq && call.grace)
                {
                    last.rows.extend(answer.rows);
                    let (done, _) = verbs(&call.action);
                    let key = self.ctx.keymap.label(Action::GlobalUndo);
                    let text = format!("{done} {} · {key} undoes", last.count);
                    let left = last.until.saturating_sub(self.tick);
                    self.status.notify_for(text, self.tick, left);
                }
            }
            Err(error) => self.action_error(verbs(&call.action).1, &error),
        }
        self.dirty()
    }

    fn action_error(&mut self, what: &str, error: &OpError) {
        tracing::warn!(code = ?error.code, "an action request failed");
        let text = format!("Could not {what}: {}", error_line(error));
        self.status.error(text, self.ctx.clock.now(), self.tick);
    }

    fn action_failed(&mut self, failure: ActionFailed) -> Vec<Effect> {
        let text = format!(
            "Could not {}: {}",
            verbs(&failure.action).1,
            error_line(&failure.error)
        );
        for thread in &failure.threads {
            self.actions
                .failures
                .insert(*thread, (failure.row, text.clone()));
        }
        self.status
            .error_with_retry(text, self.ctx.clock.now(), self.tick, failure.row);
        self.dirty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_undo_window_is_the_grace() {
        assert_eq!(
            UNDO_TICKS as u128 * crate::tui::run::TICK.as_millis(),
            rail_core::ops::UNDO_GRACE.as_millis()
        );
    }
}
