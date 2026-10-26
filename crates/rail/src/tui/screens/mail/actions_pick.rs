//! The label and move pickers (`l`, `m`): refused for a selection across accounts, otherwise a
//! store read of the account's labels (`l`) or move targets (`m`, by its folder model: Gmail's
//! labels, or a Microsoft 365 account's Inbox, Archive, Junk and own folders) and what the
//! threads are in, then the picker; an account with nothing to offer says so in its own words
//! (labels on Gmail, folders on Microsoft 365); its answer is a `labels` or `move` action with
//! grace. The snooze chooser (`h`) shares the picking state; its answer goes to `snooze_flow`.

use std::collections::BTreeSet;

use rail_core::ops::Action as MailAction;
use rail_core::store::view::LabelChoices;

use super::MailScreen;
use super::selection::Picked;
use crate::tui::app::Effect;
use crate::tui::data::ReadError;
use crate::tui::modals::{Command, Modal, Picker, PickerItem, PickerKind};

/// A picker waiting for its labels, and what it applies to.
#[derive(Debug)]
pub(crate) struct Picking {
    pub(super) seq: u64,
    pub(super) mode: PickerKind,
    pub(super) targets: Vec<(i64, Picked)>,
}

impl MailScreen {
    pub(super) fn open_picker(
        &mut self,
        mode: PickerKind,
        targets: Vec<(i64, Picked)>,
    ) -> Vec<Effect> {
        let accounts: BTreeSet<i64> = targets.iter().map(|(_, p)| p.account_id).collect();
        let Some(account_id) = accounts.first().copied().filter(|_| accounts.len() == 1) else {
            self.status.notify(
                "Labels belong to one account: select threads from one account.",
                self.tick,
            );
            return Vec::new();
        };
        let seq = self.next_seq();
        let threads = targets.iter().map(|(id, _)| *id).collect();
        self.actions.picking = Some(Picking { seq, mode, targets });
        let read = match mode {
            PickerKind::Move => self.ctx.store.move_choices(seq, account_id, threads),
            _ => self.ctx.store.labels(seq, account_id, threads),
        };
        vec![read]
    }

    pub(super) fn labels_read(
        &mut self,
        seq: u64,
        result: Result<LabelChoices, ReadError>,
    ) -> Vec<Effect> {
        let Some(mode) = self
            .actions
            .picking
            .as_ref()
            .filter(|p| p.seq == seq)
            .map(|p| p.mode)
        else {
            return Vec::new();
        };
        match result {
            Ok(choices) if choices.labels.is_empty() => {
                let picking = self.actions.picking.take();
                let folders = picking
                    .as_ref()
                    .and_then(|p| p.targets.first())
                    .is_some_and(|(_, t)| self.is_m365(t.account_id));
                let what = if folders { "folders" } else { "labels" };
                self.status.notify(
                    format!("This account has no {what} to choose from."),
                    self.tick,
                );
            }
            Ok(choices) => {
                let items = choices
                    .labels
                    .into_iter()
                    .map(|l| PickerItem {
                        checked: mode == PickerKind::Labels
                            && choices.current.contains(&l.folder_id),
                        id: l.folder_id,
                        name: l.name,
                        swatch: Vec::new(),
                    })
                    .collect();
                self.modal = Some(Modal::Picker(Picker::new(mode, items)));
            }
            Err(error) => {
                self.actions.picking = None;
                self.side_read_failed(error, "labels");
            }
        }
        Vec::new()
    }

    pub(super) fn picker_command(&mut self, command: Command) -> Vec<Effect> {
        let Some(picking) = self.actions.picking.take() else {
            return Vec::new();
        };
        let action = match command {
            Command::Labels { add, remove } if add.is_empty() && remove.is_empty() => {
                self.status.notify("No labels changed.", self.tick);
                return Vec::new();
            }
            Command::Labels { add, remove } => MailAction::Labels { add, remove },
            Command::MoveTo(to) => MailAction::Move { to },
            Command::Snooze(until) => return self.send_snooze(&picking.targets, until),
            Command::SnoozeReturn => return self.return_snoozed(&picking.targets),
            _ => return Vec::new(),
        };
        self.send_action(action, &picking.targets, true)
    }
}
